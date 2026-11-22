//! Minimal Chrome DevTools Protocol client over a WebSocket, using flat sessions.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;

/// Default time a command may take before it is considered lost.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

pub struct CdpEvent {
    pub method: String,
    pub params: Value,
    pub session_id: Option<String>,
}

type Pending = Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>;

pub struct Cdp {
    out: mpsc::UnboundedSender<String>,
    pending: Arc<Pending>,
    next_id: AtomicU64,
}

impl Cdp {
    /// Connect to the browser endpoint. Events are delivered on the returned channel, which
    /// closes when the connection is lost.
    pub async fn connect(url: &str) -> Result<(Arc<Cdp>, mpsc::UnboundedReceiver<CdpEvent>)> {
        let mut cfg = WebSocketConfig::default();
        cfg.max_message_size = Some(256 << 20);
        cfg.max_frame_size = Some(256 << 20);
        let (ws, _) = tokio_tungstenite::connect_async_with_config(url, Some(cfg), true).await?;
        let (mut sink, mut stream) = ws.split();

        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        let pending: Arc<Pending> = Arc::new(Mutex::new(HashMap::new()));

        tokio::spawn(async move {
            while let Some(text) = out_rx.recv().await {
                if sink.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            let _ = sink.close().await;
        });

        let pending_r = pending.clone();
        tokio::spawn(async move {
            while let Some(msg) = stream.next().await {
                let text = match msg {
                    Ok(Message::Text(t)) => t,
                    Ok(Message::Close(_)) | Err(_) => break,
                    Ok(_) => continue,
                };
                let Ok(mut v) = serde_json::from_str::<Value>(text.as_str()) else { continue };
                if let Some(id) = v.get("id").and_then(Value::as_u64) {
                    let tx = pending_r.lock().unwrap().remove(&id);
                    if let Some(tx) = tx {
                        let res = match v.get_mut("error") {
                            Some(err) => Err(err
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or("unknown CDP error")
                                .to_string()),
                            None => Ok(v.get_mut("result").map(Value::take).unwrap_or(Value::Null)),
                        };
                        let _ = tx.send(res);
                    }
                } else if let Some(method) = v.get("method").and_then(Value::as_str) {
                    let ev = CdpEvent {
                        method: method.to_string(),
                        session_id: v.get("sessionId").and_then(Value::as_str).map(str::to_string),
                        params: v.get_mut("params").map(Value::take).unwrap_or(Value::Null),
                    };
                    if ev_tx.send(ev).is_err() {
                        break;
                    }
                }
            }
            // Fail every command still waiting for an answer; dropping ev_tx signals the disconnect.
            pending_r.lock().unwrap().clear();
        });

        Ok((Arc::new(Cdp { out: out_tx, pending, next_id: AtomicU64::new(1) }), ev_rx))
    }

    pub async fn call(&self, method: &str, params: Value, session: Option<&str>) -> Result<Value> {
        self.call_timeout(method, params, session, DEFAULT_TIMEOUT).await
    }

    pub async fn call_timeout(
        &self,
        method: &str,
        params: Value,
        session: Option<&str>,
        timeout: Duration,
    ) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = Value::String(s.to_string());
        }
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        if self.out.send(msg.to_string()).is_err() {
            self.pending.lock().unwrap().remove(&id);
            bail!("{method}: browser connection closed");
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(Ok(v))) => Ok(v),
            Ok(Ok(Err(e))) => Err(anyhow!("{method}: {e}")),
            Ok(Err(_)) => Err(anyhow!("{method}: browser connection closed")),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(anyhow!("{method}: timed out after {}s", timeout.as_secs()))
            }
        }
    }
}

//! HTTP + WebSocket server talking to the EFB app (and the debug viewer).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Map, Value};
use tokio::sync::{mpsc, oneshot};

use crate::config::Config;
use crate::host::{Command, Frame, Host, HostEvent, MouseInput, ViewConfig, WheelInput};
use crate::keys;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const ACK_TIMEOUT: Duration = Duration::from_secs(2);
const INDEX_HTML: &str = include_str!("../assets/index.html");

struct Client {
    tx: mpsc::UnboundedSender<Message>,
    http_transport: bool,
    paused: bool,
    configured: bool,
    awaiting_ack: bool,
    dirty: bool,
    sent_at: Instant,
    last_sent_id: u64,
}

impl Client {
    fn send(&self, msg: &Value) {
        let _ = self.tx.send(Message::Text(msg.to_string().into()));
    }

    /// Frames are flow-controlled per client: one in flight, newest wins.
    fn send_frame(&mut self, frame: Option<&Arc<Frame>>) {
        let Some(f) = frame else { return };
        if self.paused {
            return;
        }
        if self.awaiting_ack && self.sent_at.elapsed() < ACK_TIMEOUT {
            self.dirty = true;
            return;
        }
        if self.last_sent_id == f.id {
            return;
        }
        self.awaiting_ack = true;
        self.dirty = false;
        self.sent_at = Instant::now();
        self.last_sent_id = f.id;
        // Built by hand to avoid copying the image into a JSON value (base64 needs no escaping).
        let head = format!(r#"{{"type":"frame","id":{},"width":{},"height":{}"#, f.id, f.width, f.height);
        let msg = if self.http_transport {
            head + "}"
        } else {
            let mut s = String::with_capacity(head.len() + f.data.len() + 12);
            s.push_str(&head);
            s.push_str(r#","data":""#);
            s.push_str(&f.data);
            s.push_str("\"}");
            s
        };
        let _ = self.tx.send(Message::Text(msg.into()));
    }
}

/// `{ "type": kind, ...fields }`
fn typed(kind: &str, fields: Value) -> Value {
    let mut m = Map::new();
    m.insert("type".into(), kind.into());
    if let Value::Object(o) = fields {
        m.extend(o);
    }
    Value::Object(m)
}

/// JavaScript-like `Number(v) || 0`.
fn num(v: &Value) -> f64 {
    let n = match v {
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::String(s) => s.trim().parse().unwrap_or(0.0),
        Value::Bool(b) => *b as u8 as f64,
        _ => 0.0,
    };
    if n.is_finite() { n } else { 0.0 }
}

fn opt_num(v: &Value) -> Option<f64> {
    (!v.is_null()).then(|| num(v))
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Null => false,
        _ => true,
    }
}

fn modifier_mask(mods: &Value) -> u32 {
    keys::MODIFIERS
        .iter()
        .filter(|(name, ..)| truthy(&mods[*name]))
        .fold(0, |m, (.., bit)| m | bit)
}

pub struct App {
    cfg: Arc<Config>,
    host: RwLock<Option<Arc<Host>>>,
    clients: Mutex<HashMap<u64, Client>>,
    next_id: AtomicU64,
    events_tx: mpsc::UnboundedSender<HostEvent>,
}

impl App {
    pub fn new(cfg: Arc<Config>) -> (Arc<App>, mpsc::UnboundedReceiver<HostEvent>) {
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let app = Arc::new(App {
            cfg,
            host: RwLock::new(None),
            clients: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            events_tx,
        });
        (app, events_rx)
    }

    fn host(&self) -> Option<Arc<Host>> {
        self.host.read().unwrap().clone()
    }

    fn clients(&self) -> MutexGuard<'_, HashMap<u64, Client>> {
        self.clients.lock().unwrap()
    }

    pub async fn start_browser(&self) -> Result<()> {
        let host = Host::launch(self.cfg.clone(), self.events_tx.clone()).await?;
        *self.host.write().unwrap() = Some(host);
        Ok(())
    }

    pub async fn shutdown(&self) {
        self.clients().clear();
        let host = self.host.write().unwrap().take();
        if let Some(host) = host {
            host.close().await;
        }
    }

    fn broadcast(&self, msg: Value) {
        for c in self.clients().values() {
            c.send(&msg);
        }
    }

    fn update_streaming(&self) {
        let Some(host) = self.host() else { return };
        let any_active = self.clients().values().any(|c| c.configured && !c.paused);
        host.enqueue(Command::SetStreaming(any_active));
    }

    /// Forward browser events to the connected clients.
    pub async fn run_events(self: Arc<Self>, mut rx: mpsc::UnboundedReceiver<HostEvent>) {
        while let Some(ev) = rx.recv().await {
            match ev {
                HostEvent::Frame => {
                    let frame = self.host().and_then(|h| h.last_frame());
                    for c in self.clients().values_mut() {
                        c.send_frame(frame.as_ref());
                    }
                }
                HostEvent::State(s) => self.broadcast(typed("state", s)),
                HostEvent::Focus(f) => self.broadcast(typed("focus", f)),
                HostEvent::Select(s) => self.broadcast(typed("select", s)),
                HostEvent::Dialog(d) => self.broadcast(json!({ "type": "dialog", "dialog": d })),
                HostEvent::Notice { level, message } => {
                    println!("[browser] {level}: {message}");
                    self.broadcast(json!({ "type": "notice", "level": level, "message": message }));
                }
                HostEvent::Crash => {
                    eprintln!("[browser] browser process exited, restarting in 2s...");
                    self.broadcast(json!({ "type": "notice", "level": "error", "message": "Browser crashed, restarting..." }));
                    let old = self.host.write().unwrap().take();
                    tokio::spawn(self.clone().restart(old));
                }
            }
        }
    }

    async fn restart(self: Arc<Self>, old: Option<Arc<Host>>) {
        if let Some(old) = old {
            old.close().await;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        loop {
            match self.start_browser().await {
                Ok(()) => break,
                Err(e) => {
                    eprintln!("[browser] restart failed: {e:#}. Retrying in 5s...");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
        for c in self.clients().values_mut() {
            c.last_sent_id = 0;
            c.awaiting_ack = false;
        }
        self.update_streaming();
    }

    async fn client_session(self: Arc<Self>, socket: WebSocket, addr: SocketAddr) {
        let (mut sink, mut stream) = socket.split();
        let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let client = Client {
            tx,
            http_transport: false,
            paused: false,
            configured: false,
            awaiting_ack: false,
            dirty: false,
            sent_at: Instant::now(),
            last_sent_id: 0,
        };
        client.send(&json!({ "type": "hello", "version": VERSION, "home": self.cfg.home }));
        let total = {
            let mut clients = self.clients();
            clients.insert(id, client);
            clients.len()
        };
        println!("[ws] client connected from {} ({total} total)", addr.ip().to_canonical());

        let writer = tokio::spawn(async move {
            while let Some(m) = rx.recv().await {
                if sink.send(m).await.is_err() {
                    break;
                }
            }
        });
        while let Some(Ok(msg)) = stream.next().await {
            match msg {
                Message::Text(t) => {
                    if let Ok(v) = serde_json::from_str::<Value>(t.as_str()) {
                        self.handle_message(id, v);
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
        writer.abort();
        let total = {
            let mut clients = self.clients();
            clients.remove(&id);
            clients.len()
        };
        println!("[ws] client disconnected ({total} total)");
        self.update_streaming();
    }

    fn handle_message(self: &Arc<Self>, id: u64, msg: Value) {
        let Some(host) = self.host() else { return };
        let kind = msg["type"].as_str().unwrap_or_default();
        match kind {
            "config" => {
                {
                    let mut clients = self.clients();
                    let Some(c) = clients.get_mut(&id) else { return };
                    match msg["transport"].as_str() {
                        Some("http") => c.http_transport = true,
                        Some("data") => c.http_transport = false,
                        _ => {}
                    }
                    c.configured = true;
                    c.last_sent_id = 0;
                    c.awaiting_ack = false;
                }
                let view = ViewConfig {
                    width: opt_num(&msg["width"]),
                    height: opt_num(&msg["height"]),
                    zoom: opt_num(&msg["zoom"]),
                    mobile: msg["mobile"].as_bool(),
                    quality: opt_num(&msg["quality"]),
                };
                let (done_tx, done_rx) = oneshot::channel();
                host.enqueue(Command::Configure(view, done_tx));
                let app = self.clone();
                tokio::spawn(async move {
                    let _ = done_rx.await;
                    app.update_streaming();
                    let state = host.current_state();
                    let frame = host.last_frame();
                    if let Some(c) = app.clients().get_mut(&id) {
                        if let Some(s) = state {
                            c.send(&typed("state", s));
                        }
                        c.send_frame(frame.as_ref());
                    }
                });
            }
            "ack" => {
                let frame = host.last_frame();
                if let Some(c) = self.clients().get_mut(&id) {
                    c.awaiting_ack = false;
                    if c.dirty {
                        c.send_frame(frame.as_ref());
                    }
                }
            }
            "pause" => {
                if let Some(c) = self.clients().get_mut(&id) {
                    c.paused = true;
                }
                self.update_streaming();
            }
            "resume" => {
                if let Some(c) = self.clients().get_mut(&id) {
                    c.paused = false;
                    c.last_sent_id = 0;
                    c.awaiting_ack = false;
                }
                self.update_streaming();
                let frame = host.last_frame();
                if let Some(c) = self.clients().get_mut(&id) {
                    c.send_frame(frame.as_ref());
                }
            }
            "navigate" => host.enqueue(Command::Navigate(msg["url"].as_str().unwrap_or_default().to_string())),
            "home" => host.enqueue(Command::Navigate(self.cfg.home.clone())),
            "back" => host.enqueue(Command::History(-1)),
            "forward" => host.enqueue(Command::History(1)),
            "reload" => host.enqueue(Command::Reload),
            "stop" => host.enqueue(Command::Stop),
            "closeTab" => host.enqueue(Command::CloseTab),
            "mouse" => host.enqueue(Command::Mouse(MouseInput {
                action: msg["action"].as_str().unwrap_or_default().to_string(),
                x: num(&msg["x"]),
                y: num(&msg["y"]),
                button: msg["button"].as_str().unwrap_or("left").to_string(),
                click_count: msg["clickCount"].as_f64().map(|n| n as i64).unwrap_or(1),
                modifiers: modifier_mask(&msg["modifiers"]),
            })),
            "wheel" => host.enqueue(Command::Wheel(WheelInput {
                x: num(&msg["x"]),
                y: num(&msg["y"]),
                dx: num(&msg["dx"]),
                dy: num(&msg["dy"]),
                ndx: opt_num(&msg["ndx"]),
                ndy: opt_num(&msg["ndy"]),
            })),
            "key" => host.enqueue(Command::Key {
                key: msg["key"].as_str().unwrap_or_default().to_string(),
                modifiers: modifier_mask(&msg["modifiers"]),
            }),
            "text" => {
                if let Some(t) = msg["text"].as_str() {
                    host.enqueue(Command::Text(t.to_string()));
                }
            }
            "select" => {
                let index = msg["index"].as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64).unwrap_or(-1);
                host.enqueue(Command::Select(index));
            }
            "dialog" => {
                let accept = truthy(&msg["accept"]);
                let text = match &msg["text"] {
                    Value::Null => None,
                    Value::String(s) => Some(s.clone()),
                    other => Some(other.to_string()),
                };
                tokio::spawn(async move { host.dialog_response(accept, text).await });
            }
            "ping" => {
                if let Some(c) = self.clients().get(&id) {
                    c.send(&json!({ "type": "pong", "t": msg["t"] }));
                }
            }
            _ => {}
        }
    }
}

fn is_frame_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/frame").and_then(|p| p.strip_suffix(".jpg")) else { return false };
    rest.is_empty() || rest.strip_prefix('/').is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
}

/// Single entry point: WebSocket upgrades on any path, plus a few HTTP routes.
pub async fn handle(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
    uri: Uri,
) -> Response {
    if let Ok(ws) = ws {
        return ws
            .max_message_size(1024 * 1024)
            .on_upgrade(move |socket| app.client_session(socket, addr))
            .into_response();
    }
    let path = uri.path();
    let mut res = if is_frame_path(path) {
        // Latest frame as a plain JPEG (used by the "http" frame transport).
        match app.host().and_then(|h| h.last_frame()) {
            Some(f) => (
                [(header::CONTENT_TYPE, "image/jpeg"), (header::CACHE_CONTROL, "no-store")],
                f.bytes().to_vec(),
            )
                .into_response(),
            None => StatusCode::NO_CONTENT.into_response(),
        }
    } else if path == "/status" {
        let body = json!({
            "version": VERSION,
            "clients": app.clients().len(),
            "state": app.host().and_then(|h| h.current_state()),
        });
        ([(header::CONTENT_TYPE, "application/json")], body.to_string()).into_response()
    } else if path == "/" || path == "/index.html" {
        // Debug viewer: open http://127.0.0.1:8737/ in a normal browser to test without the sim.
        ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], INDEX_HTML).into_response()
    } else {
        (StatusCode::NOT_FOUND, "Not found").into_response()
    };
    res.headers_mut()
        .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_paths() {
        assert!(is_frame_path("/frame.jpg"));
        assert!(is_frame_path("/frame/42.jpg"));
        assert!(!is_frame_path("/frame/.jpg"));
        assert!(!is_frame_path("/frame/4x.jpg"));
        assert!(!is_frame_path("/frames.jpg"));
    }

    #[test]
    fn modifiers() {
        assert_eq!(modifier_mask(&json!({ "ctrl": true, "shift": true })), keys::MOD_CTRL | keys::MOD_SHIFT);
        assert_eq!(modifier_mask(&Value::Null), 0);
    }
}

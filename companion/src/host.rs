//! Owns the headless browser: tabs, viewport, screencast and input injection.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use regex::Regex;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command as Process};
use tokio::sync::{mpsc, oneshot};

use crate::cdp::{Cdp, CdpEvent};
use crate::config::Config;
use crate::keys::{self, KeyDef};

const PAGE_SCRIPT: &str = include_str!("../assets/page-script.js");
const STATE_TIMEOUT: Duration = Duration::from_secs(2);

pub struct Frame {
    pub id: u64,
    /// Base64 JPEG, as delivered by the screencast.
    pub data: String,
    pub width: u32,
    pub height: u32,
    bytes: OnceLock<Vec<u8>>,
}

impl Frame {
    pub fn bytes(&self) -> &[u8] {
        self.bytes
            .get_or_init(|| base64::engine::general_purpose::STANDARD.decode(&self.data).unwrap_or_default())
    }
}

pub enum HostEvent {
    /// A new frame is available in `Host::last_frame`.
    Frame,
    State(Value),
    Focus(Value),
    Select(Value),
    /// Dialog description, or null when it closed.
    Dialog(Value),
    Notice { level: &'static str, message: String },
    Crash,
}

#[derive(Default)]
pub struct ViewConfig {
    pub width: Option<f64>,
    pub height: Option<f64>,
    pub zoom: Option<f64>,
    pub mobile: Option<bool>,
    pub quality: Option<f64>,
}

pub struct MouseInput {
    pub action: String,
    pub x: f64,
    pub y: f64,
    pub button: String,
    pub click_count: i64,
    pub modifiers: u32,
}

pub struct WheelInput {
    pub x: f64,
    pub y: f64,
    pub dx: f64,
    pub dy: f64,
    pub ndx: Option<f64>,
    pub ndy: Option<f64>,
}

/// Commands run one at a time, in order, so that input ordering is preserved.
pub enum Command {
    Init(oneshot::Sender<Result<()>>),
    Configure(ViewConfig, oneshot::Sender<()>),
    SetStreaming(bool),
    Navigate(String),
    History(i64),
    Reload,
    Stop,
    CloseTab,
    Mouse(MouseInput),
    Wheel(WheelInput),
    Key { key: String, modifiers: u32 },
    Text(String),
    Select(i64),
    AddTab { target_id: String, activate: bool },
    RemoveTab(String),
    Shutdown,
}

struct Tab {
    target_id: String,
    session_id: String,
    loading: bool,
    screencasting: bool,
    view_key: String,
}

/// Size of the EFB view in device pixels, zoom = device pixels per CSS pixel.
struct View {
    width: u32,
    height: u32,
    zoom: f64,
    mobile: bool,
}

struct State {
    tabs: Vec<Tab>,
    /// Target id of the active tab.
    active: Option<String>,
    /// Session id -> target id of the tab that owns it (tab sessions and their iframe sessions).
    sessions: HashMap<String, String>,
    view: View,
    quality: u32,
    streaming: bool,
    last_frame: Option<Arc<Frame>>,
    frame_seq: u64,
    last_state: Option<Value>,
    last_state_json: String,
    last_focus_json: String,
    focus_gen: u64,
    state_gen: u64,
    /// Session that has a JavaScript dialog open.
    pending_dialog: Option<String>,
    /// Session and execution context of the last <select> that was opened.
    select_ctx: Option<(String, Option<i64>)>,
    mouse_button: &'static str,
}

impl State {
    fn tab(&self, target_id: &str) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.target_id == target_id)
    }
    fn tab_mut(&mut self, target_id: &str) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.target_id == target_id)
    }
    fn active_tab(&self) -> Option<&Tab> {
        self.active.as_deref().and_then(|id| self.tab(id))
    }
    /// True when `session` is the main session of the tab whose main frame is `frame_id`.
    fn is_main_frame(&self, session: &str, frame_id: &str) -> bool {
        self.tabs.iter().any(|t| t.session_id == session && t.target_id == frame_id)
    }
    fn owner_is_active(&self, session: &str) -> bool {
        self.active.is_some() && self.sessions.get(session) == self.active.as_ref()
    }
    fn css_size(&self) -> (f64, f64) {
        let v = &self.view;
        (
            (v.width as f64 / v.zoom).round().max(200.0),
            (v.height as f64 / v.zoom).round().max(150.0),
        )
    }
}

pub struct Host {
    cfg: Arc<Config>,
    cdp: Arc<Cdp>,
    state: Mutex<State>,
    cmd_tx: mpsc::UnboundedSender<Command>,
    events: mpsc::UnboundedSender<HostEvent>,
    child: Mutex<Option<Child>>,
    closing: AtomicBool,
    base_user_agent: String,
    refresh_lock: tokio::sync::Mutex<()>,
}

fn browser_args(cfg: &Config) -> Vec<String> {
    let mut args: Vec<String> = [
        // Puppeteer's defaults, minus --enable-automation and --mute-audio.
        "--allow-pre-commit-input",
        "--disable-background-networking",
        "--disable-background-timer-throttling",
        "--disable-backgrounding-occluded-windows",
        "--disable-breakpad",
        "--disable-client-side-phishing-detection",
        "--disable-component-extensions-with-background-pages",
        "--disable-crash-reporter",
        "--disable-default-apps",
        "--disable-dev-shm-usage",
        "--disable-hang-monitor",
        "--disable-infobars",
        "--disable-ipc-flooding-protection",
        "--disable-popup-blocking",
        "--disable-prompt-on-repost",
        "--disable-renderer-backgrounding",
        "--disable-search-engine-choice-screen",
        "--disable-sync",
        "--force-color-profile=srgb",
        "--metrics-recording-only",
        "--password-store=basic",
        "--use-mock-keychain",
        "--disable-features=Translate,MediaRouter,AcceptCHFrame,OptimizationHints,ProcessPerSiteUpToMainFrameThreshold,IsolateSandboxedIframes",
        // Companion specific.
        "--disable-blink-features=AutomationControlled",
        "--autoplay-policy=no-user-gesture-required",
        "--no-first-run",
        "--no-default-browser-check",
        "--hide-crash-restore-bubble",
        "--remote-debugging-port=0",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if cfg.headless {
        args.push("--headless=new".into());
        args.push("--hide-scrollbars".into());
    }
    // Headless screencasts ignore the emulated deviceScaleFactor: render at a fixed
    // high DPI instead and let the screencast downscale frames to the EFB size.
    args.push(format!("--force-device-scale-factor={}", cfg.render_scale));
    args.push(format!("--user-data-dir={}", cfg.profile_dir.display()));
    args.push("about:blank".into());
    args
}

static RE_SCHEME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(https?|file|about|data|chrome|edge|view-source):").unwrap());
static RE_LOCAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(localhost|\d{1,3}(\.\d{1,3}){3}|\[[0-9a-f:]+\])(:\d+)?(/.*)?$").unwrap()
});
static RE_DOMAIN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^[^/?#]+\.[a-z][a-z0-9-]+(:\d+)?([/?#].*)?$").unwrap());

/// JavaScript's encodeURIComponent.
fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Turn address bar input into a URL: URLs pass through, host names get a scheme, the rest is searched.
pub fn normalize_url(input: &str, search: &str) -> Option<String> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    if RE_SCHEME.is_match(s) {
        return Some(s.to_string());
    }
    if RE_LOCAL.is_match(s) {
        return Some(format!("http://{s}"));
    }
    if !s.chars().any(char::is_whitespace) && RE_DOMAIN.is_match(s) {
        return Some(format!("https://{s}"));
    }
    Some(search.replacen("%s", &encode_uri_component(s), 1))
}

fn button_mask(button: &str) -> u32 {
    match button {
        "left" => 1,
        "right" => 2,
        "middle" => 4,
        _ => 0,
    }
}

fn static_button(button: &str) -> &'static str {
    match button {
        "right" => "right",
        "middle" => "middle",
        "none" => "none",
        _ => "left",
    }
}

impl Host {
    /// Start the browser and open the home page.
    pub async fn launch(cfg: Arc<Config>, events: mpsc::UnboundedSender<HostEvent>) -> Result<Arc<Host>> {
        let exe = cfg.browser_path.clone().ok_or_else(|| {
            anyhow!("No Chromium based browser found. Install Microsoft Edge or Google Chrome, or pass --browser \"<path to exe>\".")
        })?;
        std::fs::create_dir_all(&cfg.profile_dir)
            .with_context(|| format!("cannot create profile directory {}", cfg.profile_dir.display()))?;

        let mut child = Process::new(&exe)
            .args(browser_args(&cfg))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("cannot start {}", exe.display()))?;

        // Chromium prints "DevTools listening on ws://..." on stderr once it is ready.
        let mut lines = BufReader::new(child.stderr.take().expect("piped stderr")).lines();
        let verbose = cfg.verbose;
        let found = tokio::time::timeout(Duration::from_secs(30), async {
            while let Ok(Some(line)) = lines.next_line().await {
                if verbose {
                    eprintln!("[browser:stderr] {line}");
                }
                if let (true, Some(i)) = (line.contains("DevTools listening on"), line.find("ws://")) {
                    return Some(line[i..].trim().to_string());
                }
            }
            None
        })
        .await;
        let ws_url = match found {
            Ok(Some(url)) => url,
            Ok(None) => bail!(
                "the browser exited during startup. Is another browser already using the profile {}?",
                cfg.profile_dir.display()
            ),
            Err(_) => bail!("the browser did not open its DevTools endpoint within 30s"),
        };
        tokio::spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                if verbose {
                    eprintln!("[browser:stderr] {line}");
                }
            }
        });

        let (cdp, cdp_events) = Cdp::connect(&ws_url).await.context("cannot connect to the browser")?;
        let version = cdp.call("Browser.getVersion", json!({}), None).await?;
        let base_user_agent = version["userAgent"].as_str().unwrap_or_default().replace("HeadlessChrome", "Chrome");

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let host = Arc::new(Host {
            cfg,
            cdp: cdp.clone(),
            state: Mutex::new(State {
                tabs: Vec::new(),
                active: None,
                sessions: HashMap::new(),
                view: View { width: 900, height: 1200, zoom: 1.0, mobile: false },
                quality: 70,
                streaming: false,
                last_frame: None,
                frame_seq: 0,
                last_state: None,
                last_state_json: String::new(),
                last_focus_json: String::new(),
                focus_gen: 0,
                state_gen: 0,
                pending_dialog: None,
                select_ctx: None,
                mouse_button: "none",
            }),
            cmd_tx,
            events,
            child: Mutex::new(Some(child)),
            closing: AtomicBool::new(false),
            base_user_agent,
            refresh_lock: tokio::sync::Mutex::new(()),
        });

        tokio::spawn(host.clone().run_events(cdp_events));
        tokio::spawn(host.clone().run_commands(cmd_rx));
        let weak = Arc::downgrade(&host);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            loop {
                tick.tick().await;
                let Some(host) = weak.upgrade() else { break };
                if host.closing.load(Ordering::Relaxed) {
                    break;
                }
                host.refresh_state().await;
            }
        });

        let (init_tx, init_rx) = oneshot::channel();
        host.enqueue(Command::Init(init_tx));
        let init = async {
            cdp.call("Target.setDiscoverTargets", json!({ "discover": true }), None).await?;
            init_rx.await.map_err(|_| anyhow!("browser closed during startup"))?
        };
        if let Err(e) = init.await {
            host.close().await;
            return Err(e);
        }
        Ok(host)
    }

    pub async fn close(&self) {
        self.closing.store(true, Ordering::Relaxed);
        let _ = self.cmd_tx.send(Command::Shutdown);
        let _ = self.cdp.call_timeout("Browser.close", json!({}), None, Duration::from_secs(3)).await;
        let child = self.child.lock().unwrap().take();
        if let Some(mut child) = child {
            if tokio::time::timeout(Duration::from_secs(5), child.wait()).await.is_err() {
                let _ = child.kill().await;
            }
        }
    }

    pub fn enqueue(&self, cmd: Command) {
        let _ = self.cmd_tx.send(cmd);
    }

    pub fn last_frame(&self) -> Option<Arc<Frame>> {
        self.st().last_frame.clone()
    }

    pub fn current_state(&self) -> Option<Value> {
        self.st().last_state.clone()
    }

    fn st(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    fn emit(&self, ev: HostEvent) {
        let _ = self.events.send(ev);
    }

    fn log(&self, msg: impl std::fmt::Display) {
        if self.cfg.verbose {
            println!("[browser] {msg}");
        }
    }

    /// Fire and forget a CDP command.
    fn spawn_call(self: &Arc<Self>, method: &'static str, params: Value, session: Option<String>) {
        let me = self.clone();
        tokio::spawn(async move {
            if let Err(e) = me.cdp.call(method, params, session.as_deref()).await {
                me.log(e);
            }
        });
    }

    fn active_session(&self) -> Option<(String, String)> {
        self.st().active_tab().map(|t| (t.target_id.clone(), t.session_id.clone()))
    }

    fn user_agent(&self, mobile: bool) -> String {
        if !mobile {
            return self.base_user_agent.clone();
        }
        let ver = Regex::new(r"Chrome/([\d.]+)")
            .unwrap()
            .captures(&self.base_user_agent)
            .map(|c| c[1].to_string())
            .unwrap_or_else(|| "130.0.0.0".into());
        format!("Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{ver} Mobile Safari/537.36")
    }

    // ---------------------------------------------------------------- command queue

    async fn run_commands(self: Arc<Self>, mut rx: mpsc::UnboundedReceiver<Command>) {
        while let Some(cmd) = rx.recv().await {
            if matches!(cmd, Command::Shutdown) {
                break;
            }
            if let Err(e) = self.exec(cmd).await {
                self.log(format!("command failed: {e}"));
            }
        }
    }

    async fn exec(self: &Arc<Self>, cmd: Command) -> Result<()> {
        match cmd {
            Command::Init(done) => {
                let res = self.init().await;
                let _ = done.send(res);
                Ok(())
            }
            Command::Configure(view, done) => {
                let res = self.configure(view).await;
                let _ = done.send(());
                res
            }
            Command::SetStreaming(on) => self.set_streaming(on).await,
            Command::Navigate(url) => {
                self.navigate(&url);
                Ok(())
            }
            Command::History(delta) => self.history_go(delta).await,
            Command::Reload => self.reload().await,
            Command::Stop => self.stop().await,
            Command::CloseTab => self.close_tab().await,
            Command::Mouse(m) => self.mouse(m).await,
            Command::Wheel(w) => self.wheel(w).await,
            Command::Key { key, modifiers } => self.key(&key, modifiers).await,
            Command::Text(text) => self.text(&text).await,
            Command::Select(index) => self.select_option(index).await,
            Command::AddTab { target_id, activate } => self.add_tab(target_id, activate).await,
            Command::RemoveTab(target_id) => self.remove_tab(&target_id).await,
            Command::Shutdown => Ok(()),
        }
    }

    async fn init(self: &Arc<Self>) -> Result<()> {
        let targets = self.cdp.call("Target.getTargets", json!({}), None).await?;
        let existing = targets["targetInfos"].as_array().and_then(|list| {
            list.iter()
                .find(|t| t["type"] == "page")
                .and_then(|t| t["targetId"].as_str().map(str::to_string))
        });
        let target_id = match existing {
            Some(id) => id,
            None => self.create_target().await?,
        };
        self.add_tab(target_id.clone(), true).await?;
        if self.st().active.is_none() {
            self.activate(&target_id).await?;
        }
        self.navigate(&self.cfg.home);
        Ok(())
    }

    async fn create_target(&self) -> Result<String> {
        let r = self.cdp.call("Target.createTarget", json!({ "url": "about:blank" }), None).await?;
        r["targetId"].as_str().map(str::to_string).ok_or_else(|| anyhow!("createTarget: no target id"))
    }

    // ---------------------------------------------------------------- CDP events

    async fn run_events(self: Arc<Self>, mut rx: mpsc::UnboundedReceiver<CdpEvent>) {
        while let Some(ev) = rx.recv().await {
            self.on_event(ev);
        }
        // Connection lost.
        let _ = self.cmd_tx.send(Command::Shutdown);
        if !self.closing.swap(true, Ordering::Relaxed) {
            self.emit(HostEvent::Crash);
        }
    }

    fn on_event(self: &Arc<Self>, mut ev: CdpEvent) {
        let p = &mut ev.params;
        let sid = ev.session_id.clone().unwrap_or_default();
        match ev.method.as_str() {
            "Page.screencastFrame" => {
                self.spawn_call("Page.screencastFrameAck", json!({ "sessionId": p["sessionId"] }), ev.session_id);
                {
                    let mut st = self.st();
                    if st.active_tab().map(|t| t.session_id.as_str()) != Some(sid.as_str()) {
                        return;
                    }
                    st.frame_seq += 1;
                    let frame = Arc::new(Frame {
                        id: st.frame_seq,
                        data: match p["data"].take() {
                            Value::String(s) => s,
                            _ => return,
                        },
                        width: st.view.width,
                        height: st.view.height,
                        bytes: OnceLock::new(),
                    });
                    st.last_frame = Some(frame);
                }
                self.emit(HostEvent::Frame);
            }
            "Runtime.bindingCalled" => {
                let payload: Value = p["payload"].as_str().and_then(|s| serde_json::from_str(s).ok()).unwrap_or(Value::Null);
                let mut st = self.st();
                if !st.owner_is_active(&sid) {
                    drop(st);
                    self.log(format!("ignored {} from inactive session", p["name"]));
                    return;
                }
                match p["name"].as_str() {
                    Some("__efbFocus") => {
                        drop(st);
                        self.on_focus_report(&payload);
                    }
                    Some("__efbSelect") => {
                        st.select_ctx = Some((sid, p["executionContextId"].as_i64()));
                        drop(st);
                        self.emit(HostEvent::Select(payload));
                    }
                    _ => {}
                }
            }
            "Page.frameStartedLoading" | "Page.frameStoppedLoading" => {
                let loading = ev.method == "Page.frameStartedLoading";
                let frame_id = p["frameId"].as_str().unwrap_or_default();
                let mut st = self.st();
                if !st.is_main_frame(&sid, frame_id) {
                    return;
                }
                if let Some(t) = st.tab_mut(frame_id) {
                    t.loading = loading;
                }
                drop(st);
                self.schedule_state();
            }
            "Page.loadEventFired" => {
                let mut st = self.st();
                let Some(t) = st.tabs.iter_mut().find(|t| t.session_id == sid) else { return };
                t.loading = false;
                drop(st);
                self.schedule_state();
            }
            "Page.frameNavigated" => {
                if p["frame"]["parentId"].is_null() && self.st().tabs.iter().any(|t| t.session_id == sid) {
                    self.schedule_state();
                }
            }
            "Page.navigatedWithinDocument" => {
                if self.st().is_main_frame(&sid, p["frameId"].as_str().unwrap_or_default()) {
                    self.schedule_state();
                }
            }
            "Page.javascriptDialogOpening" => {
                let kind = p["type"].as_str().unwrap_or("alert").to_string();
                if kind == "beforeunload" {
                    self.spawn_call("Page.handleJavaScriptDialog", json!({ "accept": true }), ev.session_id);
                    return;
                }
                let mut st = self.st();
                if !st.owner_is_active(&sid) {
                    drop(st);
                    self.spawn_call("Page.handleJavaScriptDialog", json!({ "accept": false }), ev.session_id);
                    return;
                }
                if st.pending_dialog.is_some() {
                    return;
                }
                st.pending_dialog = Some(sid);
                drop(st);
                self.emit(HostEvent::Dialog(json!({
                    "kind": kind,
                    "message": p["message"].as_str().unwrap_or_default(),
                    "defaultValue": p["defaultPrompt"].as_str().unwrap_or_default(),
                })));
            }
            "Page.javascriptDialogClosed" => {
                let mut st = self.st();
                if st.pending_dialog.as_deref() == Some(sid.as_str()) {
                    st.pending_dialog = None;
                    drop(st);
                    self.emit(HostEvent::Dialog(Value::Null));
                }
            }
            "Target.targetCreated" => {
                let info = &p["targetInfo"];
                if info["type"] == "page" && info["subtype"] != "prerender" {
                    if let Some(id) = info["targetId"].as_str() {
                        // New windows (popups, target=_blank) open as tabs.
                        self.enqueue(Command::AddTab { target_id: id.to_string(), activate: true });
                    }
                }
            }
            "Target.targetDestroyed" => {
                if let Some(id) = p["targetId"].as_str() {
                    self.enqueue(Command::RemoveTab(id.to_string()));
                }
            }
            "Target.targetInfoChanged" => {
                let id = p["targetInfo"]["targetId"].as_str().unwrap_or_default();
                if self.st().active.as_deref() == Some(id) {
                    self.schedule_state();
                }
            }
            "Target.targetCrashed" => {
                let id = p["targetId"].as_str().unwrap_or_default().to_string();
                let session = self.st().tab(&id).map(|t| t.session_id.clone());
                if let Some(session) = session {
                    self.emit(HostEvent::Notice { level: "error", message: "The page crashed, reloading".into() });
                    self.spawn_call("Page.reload", json!({}), Some(session));
                }
            }
            "Target.attachedToTarget" if ev.session_id.is_some() => {
                // Auto-attached child of a tab (out-of-process iframe, worker...).
                let Some(child) = p["sessionId"].as_str().map(str::to_string) else { return };
                let is_frame = p["targetInfo"]["type"] == "iframe";
                {
                    let mut st = self.st();
                    if let Some(owner) = st.sessions.get(&sid).cloned() {
                        st.sessions.insert(child.clone(), owner);
                    }
                }
                self.log(format!("attached {} ({})", p["targetInfo"]["type"], p["targetInfo"]["url"]));
                let me = self.clone();
                tokio::spawn(async move { me.setup_child(child, is_frame).await });
            }
            "Target.detachedFromTarget" => {
                if let Some(child) = p["sessionId"].as_str() {
                    let mut st = self.st();
                    if !st.tabs.iter().any(|t| t.session_id == child) {
                        st.sessions.remove(child);
                    }
                }
            }
            _ => {}
        }
    }

    /// Install the page script in an out-of-process iframe before it runs, then let it start.
    async fn setup_child(self: Arc<Self>, session: String, is_frame: bool) {
        let s = Some(session.as_str());
        let c = &self.cdp;
        if is_frame {
            let results = tokio::join!(
                c.call("Page.enable", json!({}), s),
                c.call("Runtime.addBinding", json!({ "name": "__efbFocus" }), s),
                c.call("Runtime.addBinding", json!({ "name": "__efbSelect" }), s),
                c.call("Page.addScriptToEvaluateOnNewDocument", json!({ "source": PAGE_SCRIPT }), s),
                c.call("Runtime.enable", json!({}), s),
                c.call(
                    "Target.setAutoAttach",
                    json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true }),
                    s
                ),
            );
            for r in [results.0, results.1, results.2, results.3, results.4, results.5] {
                if let Err(e) = r {
                    self.log(format!("iframe setup: {e}"));
                }
            }
        }
        let _ = c.call("Runtime.runIfWaitingForDebugger", json!({}), s).await;
    }

    // ---------------------------------------------------------------- tabs

    async fn add_tab(self: &Arc<Self>, target_id: String, activate: bool) -> Result<()> {
        if self.st().tab(&target_id).is_some() {
            return Ok(());
        }
        let r = self
            .cdp
            .call("Target.attachToTarget", json!({ "targetId": target_id, "flatten": true }), None)
            .await?;
        let session_id = r["sessionId"].as_str().ok_or_else(|| anyhow!("attachToTarget: no session"))?.to_string();
        {
            let mut st = self.st();
            st.tabs.push(Tab {
                target_id: target_id.clone(),
                session_id: session_id.clone(),
                loading: false,
                screencasting: false,
                view_key: String::new(),
            });
            st.sessions.insert(session_id.clone(), target_id.clone());
        }
        let s = Some(session_id.as_str());
        let c = &self.cdp;
        let results = tokio::join!(
            c.call("Page.enable", json!({}), s),
            c.call("Runtime.enable", json!({}), s),
            c.call("Runtime.addBinding", json!({ "name": "__efbFocus" }), s),
            c.call("Runtime.addBinding", json!({ "name": "__efbSelect" }), s),
            c.call("Page.addScriptToEvaluateOnNewDocument", json!({ "source": PAGE_SCRIPT }), s),
            c.call(
                "Target.setAutoAttach",
                json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true }),
                s
            ),
        );
        for r in [results.0, results.1, results.2, results.3, results.4, results.5] {
            r?;
        }
        // The current document was created before the script was registered.
        self.spawn_call("Runtime.evaluate", json!({ "expression": PAGE_SCRIPT }), Some(session_id));
        if activate {
            self.activate(&target_id).await?;
        } else {
            self.schedule_state();
        }
        Ok(())
    }

    async fn remove_tab(self: &Arc<Self>, target_id: &str) -> Result<()> {
        let (was_active, next) = {
            let mut st = self.st();
            let Some(idx) = st.tabs.iter().position(|t| t.target_id == target_id) else { return Ok(()) };
            st.tabs.remove(idx);
            if let Some(d) = &st.pending_dialog {
                if st.sessions.get(d).map(String::as_str) == Some(target_id) {
                    st.pending_dialog = None;
                }
            }
            st.sessions.retain(|_, owner| owner != target_id);
            let was_active = st.active.as_deref() == Some(target_id);
            if was_active {
                st.active = None;
            }
            let next = st.tabs.get(idx.saturating_sub(1)).map(|t| t.target_id.clone());
            (was_active, next)
        };
        if was_active {
            match next {
                Some(id) => self.activate(&id).await?,
                None => {
                    // Never leave the browser without a tab.
                    let id = self.create_target().await?;
                    self.add_tab(id, true).await?;
                    self.navigate(&self.cfg.home);
                }
            }
        }
        self.schedule_state();
        Ok(())
    }

    async fn activate(self: &Arc<Self>, target_id: &str) -> Result<()> {
        let prev = self.st().active.clone();
        if let Some(prev) = prev.filter(|p| p != target_id) {
            self.stop_screencast(&prev).await;
        }
        let streaming = {
            let mut st = self.st();
            st.active = Some(target_id.to_string());
            st.select_ctx = None;
            st.streaming
        };
        self.on_focus_report(&json!({ "editable": false }));
        self.apply_viewport(target_id).await?;
        if streaming {
            self.start_screencast(target_id).await?;
        }
        self.schedule_state();
        Ok(())
    }

    async fn close_tab(&self) -> Result<()> {
        let target = {
            let st = self.st();
            if st.tabs.len() <= 1 {
                return Ok(());
            }
            st.active.clone()
        };
        if let Some(id) = target {
            self.cdp.call("Target.closeTarget", json!({ "targetId": id }), None).await?;
        }
        Ok(())
    }

    // ---------------------------------------------------------------- viewport & streaming

    /// Apply EFB view settings. Restarts the screencast when the geometry changed.
    async fn configure(self: &Arc<Self>, v: ViewConfig) -> Result<()> {
        let (prev_mobile, mobile, active, streaming) = {
            let mut st = self.st();
            let prev_mobile = st.view.mobile;
            if let Some(w) = v.width.filter(|w| *w > 0.0) {
                st.view.width = w.min(4096.0).round() as u32;
            }
            if let Some(h) = v.height.filter(|h| *h > 0.0) {
                st.view.height = h.min(4096.0).round() as u32;
            }
            if let Some(z) = v.zoom.filter(|z| *z > 0.0) {
                st.view.zoom = z.clamp(0.25, 4.0);
            }
            if let Some(m) = v.mobile {
                st.view.mobile = m;
            }
            if let Some(q) = v.quality.filter(|q| *q > 0.0) {
                st.quality = q.round().clamp(10.0, 100.0) as u32;
            }
            (prev_mobile, st.view.mobile, st.active.clone(), st.streaming)
        };
        let Some(active) = active else { return Ok(()) };
        let changed = self.apply_viewport(&active).await?;
        if changed && streaming {
            self.stop_screencast(&active).await;
            self.start_screencast(&active).await?;
        }
        if prev_mobile != mobile {
            // A new user agent only applies on the next load.
            let session = self.st().tab(&active).map(|t| t.session_id.clone());
            if let Some(session) = session {
                self.spawn_call("Page.reload", json!({}), Some(session));
            }
        }
        self.schedule_state();
        Ok(())
    }

    async fn apply_viewport(&self, target_id: &str) -> Result<bool> {
        let (session, w, h, zoom, mobile) = {
            let mut st = self.st();
            let (w, h) = st.css_size();
            let (zoom, mobile, quality) = (st.view.zoom, st.view.mobile, st.quality);
            let key = format!("{w}x{h}@{zoom}/{mobile}/{quality}");
            let Some(tab) = st.tab_mut(target_id) else { return Ok(false) };
            if tab.view_key == key {
                return Ok(false);
            }
            tab.view_key = key;
            (tab.session_id.clone(), w, h, zoom, mobile)
        };
        let s = Some(session.as_str());
        let ua = self.user_agent(mobile);
        let results = tokio::join!(
            self.cdp.call("Emulation.setUserAgentOverride", json!({ "userAgent": ua }), s),
            self.cdp.call(
                "Emulation.setDeviceMetricsOverride",
                json!({
                    "width": w,
                    "height": h,
                    "deviceScaleFactor": zoom,
                    "mobile": mobile,
                    "screenOrientation": { "angle": 0, "type": "portraitPrimary" },
                }),
                s
            ),
            self.cdp.call("Emulation.setTouchEmulationEnabled", json!({ "enabled": false }), s),
        );
        results.0?;
        results.1?;
        results.2?;
        Ok(true)
    }

    async fn set_streaming(&self, on: bool) -> Result<()> {
        let active = {
            let mut st = self.st();
            if st.streaming == on {
                return Ok(());
            }
            st.streaming = on;
            st.active.clone()
        };
        let Some(active) = active else { return Ok(()) };
        if on {
            self.start_screencast(&active).await
        } else {
            self.stop_screencast(&active).await;
            Ok(())
        }
    }

    async fn start_screencast(&self, target_id: &str) -> Result<()> {
        let (session, params) = {
            let mut st = self.st();
            let params = json!({
                "format": "jpeg",
                "quality": st.quality,
                "maxWidth": st.view.width,
                "maxHeight": st.view.height,
                "everyNthFrame": 1,
            });
            let Some(tab) = st.tab_mut(target_id) else { return Ok(()) };
            if tab.screencasting {
                return Ok(());
            }
            tab.screencasting = true;
            (tab.session_id.clone(), params)
        };
        self.cdp.call("Page.startScreencast", params, Some(&session)).await?;
        Ok(())
    }

    async fn stop_screencast(&self, target_id: &str) {
        let session = {
            let mut st = self.st();
            let Some(tab) = st.tab_mut(target_id) else { return };
            if !tab.screencasting {
                return;
            }
            tab.screencasting = false;
            tab.session_id.clone()
        };
        let _ = self.cdp.call("Page.stopScreencast", json!({}), Some(&session)).await;
    }

    // ---------------------------------------------------------------- state

    fn schedule_state(self: &Arc<Self>) {
        let gen = {
            let mut st = self.st();
            st.state_gen += 1;
            st.state_gen
        };
        let me = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if me.st().state_gen == gen {
                me.refresh_state().await;
            }
        });
    }

    async fn refresh_state(&self) {
        let _guard = self.refresh_lock.lock().await;
        let Some((target_id, session)) = self.active_session() else { return };
        let s = Some(session.as_str());
        let (title, hist, info) = tokio::join!(
            self.cdp.call_timeout(
                "Runtime.evaluate",
                json!({ "expression": "document.title", "returnByValue": true }),
                s,
                STATE_TIMEOUT
            ),
            self.cdp.call_timeout("Page.getNavigationHistory", json!({}), s, STATE_TIMEOUT),
            self.cdp.call_timeout("Target.getTargetInfo", json!({ "targetId": target_id }), None, STATE_TIMEOUT),
        );
        let title = title
            .ok()
            .and_then(|v| v["result"]["value"].as_str().map(str::to_string))
            .unwrap_or_default();
        let url = info
            .ok()
            .and_then(|v| v["targetInfo"]["url"].as_str().map(str::to_string))
            .unwrap_or_default();
        let (can_back, can_forward) = match hist {
            Ok(h) => {
                let entries = h["entries"].as_array().cloned().unwrap_or_default();
                let cur = h["currentIndex"].as_u64().unwrap_or(0) as usize;
                (
                    entries.iter().take(cur).any(|e| e["url"] != "about:blank"),
                    cur + 1 < entries.len(),
                )
            }
            Err(_) => (false, false),
        };
        let state = {
            let mut st = self.st();
            if st.active.as_deref() != Some(target_id.as_str()) {
                return;
            }
            let Some(idx) = st.tabs.iter().position(|t| t.target_id == target_id) else { return };
            let state = json!({
                "url": url,
                "title": title,
                "loading": st.tabs[idx].loading,
                "canGoBack": can_back,
                "canGoForward": can_forward,
                "tabs": st.tabs.len(),
                "tabIndex": idx,
                "mobile": st.view.mobile,
                "zoom": st.view.zoom,
            });
            let text = state.to_string();
            if text == st.last_state_json {
                return;
            }
            st.last_state_json = text;
            st.last_state = Some(state.clone());
            state
        };
        self.emit(HostEvent::State(state));
    }

    fn on_focus_report(self: &Arc<Self>, info: &Value) {
        let editable = info["editable"].as_bool().unwrap_or(false);
        let next = json!({
            "editable": editable,
            "inputType": info["inputType"].as_str().unwrap_or_default(),
            "value": info["value"].as_str().unwrap_or_default(),
        });
        let gen = {
            let mut st = self.st();
            st.focus_gen += 1;
            st.focus_gen
        };
        if editable {
            self.apply_focus(next);
        } else {
            // focusout is followed by focusin when moving between fields: debounce the "blur" case.
            let me = self.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(120)).await;
                if me.st().focus_gen == gen {
                    me.apply_focus(next);
                }
            });
        }
    }

    fn apply_focus(&self, next: Value) {
        let text = next.to_string();
        {
            let mut st = self.st();
            if text == st.last_focus_json {
                return;
            }
            st.last_focus_json = text;
        }
        self.emit(HostEvent::Focus(next));
    }

    // ---------------------------------------------------------------- navigation

    fn navigate(self: &Arc<Self>, input: &str) {
        let Some(url) = normalize_url(input, &self.cfg.search) else { return };
        let Some((target_id, session)) = self.active_session() else { return };
        if let Some(t) = self.st().tab_mut(&target_id) {
            t.loading = true;
        }
        self.schedule_state();
        let me = self.clone();
        tokio::spawn(async move {
            let res = me
                .cdp
                .call_timeout("Page.navigate", json!({ "url": url }), Some(&session), Duration::from_secs(60))
                .await;
            let error = match res {
                Ok(r) => r["errorText"].as_str().filter(|e| !e.is_empty()).map(|e| format!("{e} at {url}")),
                Err(e) => Some(e.to_string()),
            };
            if let Some(msg) = error {
                if !msg.contains("ERR_ABORTED") && !msg.contains("connection closed") {
                    me.emit(HostEvent::Notice { level: "error", message: msg });
                }
            }
        });
    }

    async fn history_go(&self, delta: i64) -> Result<()> {
        let Some((_, session)) = self.active_session() else { return Ok(()) };
        let hist = self.cdp.call("Page.getNavigationHistory", json!({}), Some(&session)).await?;
        let idx = hist["currentIndex"].as_i64().unwrap_or(0) + delta;
        if idx < 0 {
            return Ok(());
        }
        if let Some(entry) = hist["entries"].get(idx as usize) {
            self.cdp
                .call("Page.navigateToHistoryEntry", json!({ "entryId": entry["id"] }), Some(&session))
                .await?;
        }
        Ok(())
    }

    async fn reload(&self) -> Result<()> {
        let Some((_, session)) = self.active_session() else { return Ok(()) };
        self.cdp.call("Page.reload", json!({ "ignoreCache": false }), Some(&session)).await?;
        Ok(())
    }

    async fn stop(self: &Arc<Self>) -> Result<()> {
        let Some((target_id, session)) = self.active_session() else { return Ok(()) };
        self.cdp.call("Page.stopLoading", json!({}), Some(&session)).await?;
        if let Some(t) = self.st().tab_mut(&target_id) {
            t.loading = false;
        }
        self.schedule_state();
        Ok(())
    }

    // ---------------------------------------------------------------- input

    /// Normalized (0..1) view coordinates to CSS pixels.
    fn to_css(&self, x: f64, y: f64) -> (f64, f64) {
        let (w, h) = self.st().css_size();
        let c = |v: f64| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
        (c(x) * w, c(y) * h)
    }

    async fn dispatch_mouse(&self, session: &str, params: Value) -> Result<()> {
        self.cdp.call("Input.dispatchMouseEvent", params, Some(session)).await?;
        Ok(())
    }

    async fn mouse(&self, m: MouseInput) -> Result<()> {
        let Some((_, session)) = self.active_session() else { return Ok(()) };
        let (x, y) = self.to_css(m.x, m.y);
        let button = static_button(&m.button);
        let ev = |kind: &str, btn: &str, buttons: u32, count: i64| {
            json!({
                "type": kind, "x": x, "y": y, "button": btn, "buttons": buttons,
                "clickCount": count, "modifiers": m.modifiers,
            })
        };
        match m.action.as_str() {
            "move" => {
                let held = self.st().mouse_button;
                self.dispatch_mouse(&session, ev("mouseMoved", held, button_mask(held), 0)).await?;
            }
            "down" => {
                self.st().mouse_button = button;
                self.dispatch_mouse(&session, ev("mouseMoved", "none", 0, 0)).await?;
                self.dispatch_mouse(&session, ev("mousePressed", button, button_mask(button), m.click_count)).await?;
            }
            "up" => {
                self.st().mouse_button = "none";
                self.dispatch_mouse(&session, ev("mouseReleased", button, 0, m.click_count)).await?;
            }
            "click" => {
                self.dispatch_mouse(&session, ev("mouseMoved", "none", 0, 0)).await?;
                self.dispatch_mouse(&session, ev("mousePressed", button, button_mask(button), m.click_count)).await?;
                self.dispatch_mouse(&session, ev("mouseReleased", button, 0, m.click_count)).await?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Wheel: dx/dy in CSS pixels. Scroll (drag): ndx/ndy as a fraction of the view size.
    async fn wheel(&self, wh: WheelInput) -> Result<()> {
        let Some((_, session)) = self.active_session() else { return Ok(()) };
        let (x, y) = self.to_css(wh.x, wh.y);
        let (w, h) = self.st().css_size();
        let dx = wh.ndx.map(|n| n * w).unwrap_or(wh.dx);
        let dy = wh.ndy.map(|n| n * h).unwrap_or(wh.dy);
        if !(dx.is_finite() && dy.is_finite()) || (dx == 0.0 && dy == 0.0) {
            return Ok(());
        }
        let params = json!({ "type": "mouseWheel", "x": x, "y": y, "deltaX": dx, "deltaY": dy, "modifiers": 0 });
        self.dispatch_mouse(&session, params).await
    }

    async fn key_down(&self, session: &str, def: &KeyDef, modifiers: u32) -> Result<()> {
        let params = json!({
            "type": if def.text.is_empty() { "rawKeyDown" } else { "keyDown" },
            "modifiers": modifiers,
            "windowsVirtualKeyCode": def.key_code,
            "code": def.code,
            "key": def.key,
            "text": def.text,
            "unmodifiedText": def.text,
            "autoRepeat": false,
            "location": def.location,
            "isKeypad": def.location == 3,
        });
        self.cdp.call("Input.dispatchKeyEvent", params, Some(session)).await?;
        Ok(())
    }

    async fn key_up(&self, session: &str, def: &KeyDef, modifiers: u32) -> Result<()> {
        let params = json!({
            "type": "keyUp",
            "modifiers": modifiers,
            "key": def.key,
            "windowsVirtualKeyCode": def.key_code,
            "code": def.code,
            "location": def.location,
        });
        self.cdp.call("Input.dispatchKeyEvent", params, Some(session)).await?;
        Ok(())
    }

    async fn insert_text(&self, session: &str, text: &str) -> Result<()> {
        self.cdp.call("Input.insertText", json!({ "text": text }), Some(session)).await?;
        Ok(())
    }

    async fn key(&self, key: &str, modifiers: u32) -> Result<()> {
        let Some((_, session)) = self.active_session() else { return Ok(()) };
        if key.is_empty() {
            return Ok(());
        }
        let mut held = 0;
        let mut pressed = Vec::new();
        for (_, name, bit) in keys::MODIFIERS {
            if modifiers & bit != 0 {
                held |= bit;
                if let Some(def) = keys::describe(name, held) {
                    self.key_down(&session, &def, held).await?;
                    pressed.push((def, bit));
                }
            }
        }
        let result = match keys::describe(key, held) {
            Some(def) => match self.key_down(&session, &def, held).await {
                Ok(()) => self.key_up(&session, &def, held).await,
                Err(e) => Err(e),
            },
            // Unknown key name: fall back to sending it as text if printable.
            None if key.chars().count() == 1 => self.insert_text(&session, key).await,
            None => {
                self.log(format!("unknown key {key}"));
                Ok(())
            }
        };
        for (def, bit) in pressed.into_iter().rev() {
            held &= !bit;
            let _ = self.key_up(&session, &def, held).await;
        }
        result
    }

    async fn text(&self, text: &str) -> Result<()> {
        let Some((_, session)) = self.active_session() else { return Ok(()) };
        if text.is_empty() {
            return Ok(());
        }
        if text.chars().count() > 64 {
            return self.insert_text(&session, text).await;
        }
        for c in text.chars() {
            let s = c.to_string();
            match keys::describe(&s, 0) {
                Some(def) => {
                    self.key_down(&session, &def, 0).await?;
                    self.key_up(&session, &def, 0).await?;
                }
                None => self.insert_text(&session, &s).await?,
            }
        }
        Ok(())
    }

    async fn select_option(&self, index: i64) -> Result<()> {
        let ctx = self.st().select_ctx.take();
        let (session, context_id) = match ctx {
            Some(c) => c,
            None => match self.active_session() {
                Some((_, s)) => (s, None),
                None => return Ok(()),
            },
        };
        let mut params = json!({
            "expression": format!("window.__efbApplySelect ? window.__efbApplySelect({index}) : false"),
            "returnByValue": true,
        });
        if let Some(id) = context_id {
            params["contextId"] = id.into();
        }
        self.cdp.call("Runtime.evaluate", params, Some(&session)).await?;
        Ok(())
    }

    /// Answer the open JavaScript dialog. Not queued: commands to a page block while it has a dialog.
    pub async fn dialog_response(&self, accept: bool, text: Option<String>) {
        let Some(session) = self.st().pending_dialog.take() else { return };
        self.emit(HostEvent::Dialog(Value::Null));
        let mut params = json!({ "accept": accept });
        if let (true, Some(t)) = (accept, text) {
            params["promptText"] = t.into();
        }
        if let Err(e) = self.cdp.call("Page.handleJavaScriptDialog", params, Some(&session)).await {
            self.log(e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEARCH: &str = "https://www.google.com/search?q=%s";

    #[test]
    fn urls() {
        let n = |s| normalize_url(s, SEARCH).unwrap();
        assert_eq!(n("https://a.com/x"), "https://a.com/x");
        assert_eq!(n("about:blank"), "about:blank");
        assert_eq!(n("localhost:3000/x"), "http://localhost:3000/x");
        assert_eq!(n("192.168.1.2"), "http://192.168.1.2");
        assert_eq!(n("butr.cc"), "https://butr.cc");
        assert_eq!(n("example.com/a?b=c"), "https://example.com/a?b=c");
        assert_eq!(n("metar KJFK"), "https://www.google.com/search?q=metar%20KJFK");
        assert_eq!(n("café & co"), "https://www.google.com/search?q=caf%C3%A9%20%26%20co");
        assert!(normalize_url("   ", SEARCH).is_none());
    }
}

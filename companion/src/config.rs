use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Config {
    /// Interface to listen on. Use 0.0.0.0 to allow connections from other machines.
    pub host: String,
    pub port: u16,
    /// Page opened at startup and by the Home button.
    pub home: String,
    /// Search URL used when the address bar does not contain a URL. `%s` is replaced by the query.
    pub search: String,
    pub browser_path: Option<PathBuf>,
    /// Persistent profile so cookies / logins survive restarts.
    pub profile_dir: PathBuf,
    /// `--show` opens a visible browser window, useful for debugging.
    pub headless: bool,
    /// Internal render resolution multiplier (sharpness at zoom levels up to this value).
    pub render_scale: f64,
    pub verbose: bool,
    /// Started by the sim (EXE.xml): no console, log to a file, quit when the sim exits.
    pub exit_with_sim: bool,
}

pub const USAGE: &str = "\
Usage: efb-browser-companion [options]
       efb-browser-companion --install     Install the EFB app and start the companion with MSFS 2024
       efb-browser-companion --uninstall   Undo --install

  --port <n>             WebSocket / HTTP port (EFB_BROWSER_PORT, default 8737)
  --host <addr>          Interface to listen on (EFB_BROWSER_HOST, default 127.0.0.1)
  --home <url>           Home page (EFB_BROWSER_HOME, default https://butr.cc)
  --search <url>         Search URL with %s (EFB_BROWSER_SEARCH)
  --browser <exe>        Chromium executable (EFB_BROWSER_EXE, default Edge then Chrome)
  --profile <dir>        Browser profile directory (EFB_BROWSER_PROFILE)
  --render-scale <n>     Internal render DPI, 1-4 (EFB_BROWSER_RENDER_SCALE, default 2)
  --show                 Show the browser window (EFB_BROWSER_SHOW=true)
  --verbose              Log failed commands (EFB_BROWSER_VERBOSE=true)
  --exit-with-sim        Run in the background (log file) and quit when MSFS 2024 exits
";

/// Parse `--key value`, `--key=value` and `--flag` command line arguments.
fn parse_args(argv: &[String]) -> HashMap<String, Option<String>> {
    let mut out = HashMap::new();
    let mut i = 0;
    while i < argv.len() {
        let a = &argv[i];
        i += 1;
        let Some(key) = a.strip_prefix("--") else { continue };
        if let Some((k, v)) = key.split_once('=') {
            out.insert(k.to_string(), Some(v.to_string()));
            continue;
        }
        match argv.get(i) {
            Some(next) if !next.starts_with("--") => {
                out.insert(key.to_string(), Some(next.clone()));
                i += 1;
            }
            _ => {
                out.insert(key.to_string(), None);
            }
        }
    }
    out
}

/// Locate a Chromium based browser installed on this machine.
pub fn find_browser() -> Option<PathBuf> {
    let var = |k: &str| env::var_os(k).map(PathBuf::from);
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(p) = var("ProgramFiles(x86)") {
        candidates.push(p.join(r"Microsoft\Edge\Application\msedge.exe"));
    }
    if let Some(p) = var("ProgramFiles") {
        candidates.push(p.join(r"Microsoft\Edge\Application\msedge.exe"));
        candidates.push(p.join(r"Google\Chrome\Application\chrome.exe"));
    }
    if let Some(p) = var("ProgramFiles(x86)") {
        candidates.push(p.join(r"Google\Chrome\Application\chrome.exe"));
    }
    if let Some(p) = var("LOCALAPPDATA") {
        candidates.push(p.join(r"Google\Chrome\Application\chrome.exe"));
    }
    if let Some(p) = var("ProgramFiles") {
        candidates.push(p.join(r"BraveSoftware\Brave-Browser\Application\brave.exe"));
    }
    for p in [
        "/usr/bin/google-chrome",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    ] {
        candidates.push(PathBuf::from(p));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// Per-user data folder (`%LOCALAPPDATA%\EFB Browser Companion`): profile and logs.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = env::var_os("LOCALAPPDATA") {
        return PathBuf::from(dir).join("EFB Browser Companion");
    }
    env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn default_profile() -> PathBuf {
    data_dir().join("profile")
}

fn absolute(p: PathBuf) -> PathBuf {
    if p.is_absolute() {
        p
    } else {
        env::current_dir().map(|d| d.join(&p)).unwrap_or(p)
    }
}

impl Config {
    /// Returns `None` when `--help` was requested.
    pub fn from_env() -> Option<Config> {
        let argv: Vec<String> = env::args().skip(1).collect();
        let args = parse_args(&argv);
        if args.contains_key("help") || argv.iter().any(|a| a == "-h") {
            return None;
        }
        let arg = |k: &str| args.get(k).cloned().flatten();
        let flag = |k: &str, env_key: &str| {
            args.contains_key(k) || env::var(env_key).map(|v| v == "true").unwrap_or(false)
        };
        let env_str = |k: &str| env::var(k).ok().filter(|v| !v.is_empty());
        let get = |k: &str, env_key: &str| arg(k).or_else(|| env_str(env_key));

        let port = get("port", "EFB_BROWSER_PORT")
            .and_then(|v| v.parse().ok())
            .unwrap_or(8737);
        let render_scale = get("render-scale", "EFB_BROWSER_RENDER_SCALE")
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite())
            .unwrap_or(2.0)
            .clamp(1.0, 4.0);

        Some(Config {
            host: get("host", "EFB_BROWSER_HOST").unwrap_or_else(|| "127.0.0.1".into()),
            port,
            home: get("home", "EFB_BROWSER_HOME").unwrap_or_else(|| "https://butr.cc".into()),
            search: get("search", "EFB_BROWSER_SEARCH")
                .unwrap_or_else(|| "https://www.google.com/search?q=%s".into()),
            browser_path: get("browser", "EFB_BROWSER_EXE").map(PathBuf::from).or_else(find_browser),
            profile_dir: absolute(get("profile", "EFB_BROWSER_PROFILE").map(PathBuf::from).unwrap_or_else(default_profile)),
            headless: !flag("show", "EFB_BROWSER_SHOW"),
            render_scale,
            verbose: flag("verbose", "EFB_BROWSER_VERBOSE"),
            exit_with_sim: args.contains_key("exit-with-sim"),
        })
    }
}

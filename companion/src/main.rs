// GUI subsystem: no console window when the sim starts us. Interactive runs attach to (or open) a
// console themselves, see win::setup_console.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod cdp;
mod config;
mod host;
mod install;
mod keys;
mod server;
mod win;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use tokio::net::TcpListener;
use tokio::sync::watch;

use config::{data_dir, Config, USAGE};
use server::{App, VERSION};

/// Resolves on Ctrl+C, when the console window is closed (Windows), or when `quit` is set.
async fn quit_signal(mut quit: watch::Receiver<bool>) {
    let requested = async move {
        let _ = quit.wait_for(|q| *q).await;
    };
    #[cfg(windows)]
    {
        use tokio::signal::windows;
        match (windows::ctrl_close(), windows::ctrl_break()) {
            (Ok(mut close), Ok(mut brk)) => tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = close.recv() => {}
                _ = brk.recv() => {}
                _ = requested => {}
            },
            _ => tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = requested => {}
            },
        }
    }
    #[cfg(not(windows))]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
                _ = requested => {}
            },
            Err(_) => tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = requested => {}
            },
        }
    }
}

/// Quit once the sim has exited (or never showed up).
async fn watch_sim(quit: watch::Sender<bool>) {
    // Override for testing without closing the sim.
    let process = std::env::var("EFB_BROWSER_SIM_PROCESS").unwrap_or_else(|_| install::SIM_PROCESS.into());
    let started = Instant::now();
    let mut seen = false;
    let mut missing = 0;
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let name = process.clone();
        let running = tokio::task::spawn_blocking(move || !win::find_processes(&name).is_empty())
            .await
            .unwrap_or(true);
        if running {
            seen = true;
            missing = 0;
        } else if seen {
            missing += 1;
            if missing >= 2 {
                println!("{process} exited.");
                break;
            }
        } else if started.elapsed() > Duration::from_secs(300) {
            println!("{process} is not running.");
            break;
        }
    }
    let _ = quit.send(true);
}

struct Session {
    /// We opened a console window that closes with us: wait for a key before exiting on errors.
    owns_console: bool,
}

impl Session {
    async fn exit(&self, app: &App, code: i32) -> ! {
        println!("Shutting down...");
        app.shutdown().await;
        if code != 0 && self.owns_console {
            println!("Press Enter to close this window.");
            let _ = std::io::stdin().read_line(&mut String::new());
        }
        std::process::exit(code);
    }
}

async fn run(cfg: Arc<Config>, session: Session) {
    println!("EFB Browser Companion v{VERSION}");
    println!(
        "  browser : {}",
        cfg.browser_path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "(not found)".into())
    );
    println!("  profile : {}", cfg.profile_dir.display());
    println!("  home    : {}", cfg.home);

    let (app, events) = App::new(cfg.clone());
    tokio::spawn(app.clone().run_events(events));
    let (quit_tx, quit_rx) = watch::channel(false);
    if cfg.exit_with_sim {
        tokio::spawn(watch_sim(quit_tx.clone()));
    }

    // Claim the port first: a second copy must not start a browser on the same profile.
    let listener = match TcpListener::bind((cfg.host.as_str(), cfg.port)).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Cannot listen on port {}: {e}. Is the companion already running?", cfg.port);
            session.exit(&app, 1).await;
        }
    };

    tokio::select! {
        res = app.start_browser() => {
            if let Err(e) = res {
                eprintln!("Fatal: {e:#}");
                session.exit(&app, 1).await;
            }
        }
        _ = quit_signal(quit_rx.clone()) => session.exit(&app, 0).await,
    }

    let viewer_host = if cfg.host == "0.0.0.0" { "127.0.0.1" } else { cfg.host.as_str() };
    println!("\nListening on ws://{}:{}", cfg.host, cfg.port);
    println!("Test viewer: http://{viewer_host}:{}/", cfg.port);
    if cfg.exit_with_sim {
        println!("Running in the background, will quit when MSFS 2024 exits.\n");
    } else {
        println!("Leave this window open while using the Browser app on the EFB. Ctrl+C to quit.\n");
    }

    let router = Router::new().fallback(server::handle).with_state(app.clone());
    let serve = axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>());
    tokio::select! {
        res = serve => {
            if let Err(e) = res {
                eprintln!("Server error: {e}");
                session.exit(&app, 1).await;
            }
        }
        _ = quit_signal(quit_rx) => {}
    }
    session.exit(&app, 0).await;
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);

    if has("--install") || has("--uninstall") {
        win::attach_parent_console();
        let result = if has("--install") { install::install() } else { install::uninstall() };
        std::process::exit(if result.is_ok() { 0 } else { 1 });
    }

    let Some(cfg) = Config::from_env() else {
        win::attach_parent_console();
        print!("{USAGE}");
        return;
    };

    let owns_console = if cfg.exit_with_sim {
        let dir = data_dir();
        let _ = std::fs::create_dir_all(&dir);
        win::redirect_output(&dir.join("companion.log"));
        false
    } else {
        win::setup_console("EFB Browser Companion")
    };
    // The browser is started by us: make sure it never outlives this process.
    win::kill_children_on_exit();

    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
    runtime.block_on(run(Arc::new(cfg), Session { owns_console }));
}

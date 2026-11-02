//! Keel binary. Headed startup opens the window after the process model.
//! Headless serves the engine. `status` prints the engine state without taking the lock.
//! `daemon` controls the background launchd service on macOS.
//!
//! # Linux Headless Service
//!
//! On Linux, Keel runs as a systemd user service. A reference unit file is
//! provided at `scripts/linux-headless.service`. To install it manually:
//! ```text
//! mkdir -p ~/.config/systemd/user
//! cp scripts/linux-headless.service ~/.config/systemd/user/keel.service
//! systemctl --user daemon-reload
//! systemctl --user enable --now keel.service
//! ```
//! Keel does not ship an automated Linux installer or package manager script;
//! the unit file serves as documentation-as-a-unit.

mod mcp_browser;
mod mcp_ui;
mod daemon;
mod log;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use clap::{Parser, Subcommand};
use keel_engine::Engine;
use keel_rpc::{
    BootConfig, DEFAULT_IPC_PORT, ENGINE_ALREADY_RUNNING, ENGINE_LOCK_HELD, HELLO_BUDGET,
    IPC_BIND_FAILED, Identity, Mode, Outcome, RETRY_FOR, ServerGuard, boot, status_report,
};
use keel_store::{DocsStore, Layout};

#[derive(Parser)]
#[command(name = "keel", version, about = "Keel")]
struct Cli {
    #[arg(short = 'd', long = "data-dir", global = true, value_name = "PATH")]
    data_dir: Option<PathBuf>,
    /// When `KEEL_LOG` is unset, select a debug filter.
    #[arg(short = 'v', global = true)]
    verbose: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Engine only. Serves the local profile.
    Headless,
    /// Print the scope (always local), and the engine's device id and run count when one is up.
    Status,
    /// Manage the background launchd daemon (macOS).
    Daemon {
        #[command(subcommand)]
        action: daemon::DaemonAction,
    },
    /// Stdio MCP for the browser pane (registered with Keel's Pi).
    McpBrowser {
        /// Unix socket the engine serves (`{data_dir}/browser.sock`).
        #[arg(long)]
        socket: PathBuf,
    },
    /// Stdio MCP for chat cards (injected into harnesses). The agent calls a
    /// tool; this prints the card spec. The engine stores it. No socket.
    McpUi,
}

struct Held {
    engine: Option<Engine>,
    guard: Option<ServerGuard>,
}

fn main() {
    let code = run();
    if code != 0 {
        std::process::exit(code);
    }
}

fn run() -> i32 {
    keel_rpc::set_build(env!("KEEL_BUILD").parse().unwrap_or(0));
    let cli = Cli::parse();
    let data_dir = match data_dir(cli.data_dir) {
        Ok(dir) => dir,
        Err(code) => return code,
    };
    init_tracing(cli.verbose, &data_dir);
    let port = match ipc_port() {
        Ok(port) => port,
        Err(code) => return code,
    };
    match cli.command {
        Some(Command::Status) => {
            print!("{}", status_report(&data_dir, port, HELLO_BUDGET));
            0
        }
        Some(Command::Headless) => headless(data_dir, port),
        Some(Command::Daemon { action }) => daemon::handle_daemon(action, &data_dir, port),
        Some(Command::McpBrowser { socket }) => mcp_browser::run(socket),
        Some(Command::McpUi) => mcp_ui::run(),
        None => headed(data_dir, port),
    }
}

fn headed(data_dir: PathBuf, port: u16) -> i32 {
    let held = match acquire(data_dir.clone(), port, Mode::Headed) {
        Ok(held) => held,
        Err(err) => return err.code(),
    };
    let engine = held.engine;
    keel_ui::run(data_dir, held.guard, port, move || {
        if let Some(engine) = &engine {
            engine.flush();
        }
    });
    0
}

fn headless(data_dir: PathBuf, port: u16) -> i32 {
    let held = match acquire(data_dir, port, Mode::Headless) {
        Ok(held) => held,
        Err(Elsewhere) => return daemon::engine_elsewhere_exit_code(),
        Err(Failed(code)) => return code,
    };
    let Some(guard) = held.guard else {
        eprintln!("{ENGINE_ALREADY_RUNNING}");
        return daemon::engine_elsewhere_exit_code();
    };
    let engine = held.engine;
    // SIGINT or SIGTERM (launchd/systemd stop), then flush and drop engine and guard.
    guard.wait_for_shutdown_signal();
    if let Some(engine) = &engine {
        engine.flush();
    }
    0
}

/// Why `acquire` did not produce an engine.
enum AcquireError {
    /// Another engine owns the lock or the port.
    Elsewhere,
    Failed(i32),
}
use AcquireError::{Elsewhere, Failed};

impl AcquireError {
    fn code(self) -> i32 {
        match self {
            Elsewhere => 1,
            Failed(code) => code,
        }
    }
}

fn acquire(data_dir: PathBuf, port: u16, mode: Mode) -> Result<Held, AcquireError> {
    let store = Arc::new(Mutex::new(None));
    let open_slot = Arc::clone(&store);
    let ident_slot = Arc::new(Mutex::new(None));
    let ident_for_open = Arc::clone(&ident_slot);
    let open_dir = data_dir.clone();
    let config = BootConfig {
        data_dir,
        port,
        mode,
        hello_budget: HELLO_BUDGET,
        retry_for: RETRY_FOR,
    };
    match boot(&config, move || {
        let identity = open_engine_store(&open_dir, &open_slot)?;
        *ident_for_open.lock().expect("identity") = Some(identity.clone());
        Ok(identity)
    }) {
        Ok(Outcome::Attach(_)) => Ok(Held {
            engine: None,
            guard: None,
        }),
        Ok(Outcome::Engine(guard)) => {
            let opened = store.lock().expect("store").take().expect("store");
            let identity = ident_slot
                .lock()
                .expect("identity")
                .take()
                .expect("identity");
            // Inside the engine runtime: boot spawns the repo watcher and its
            // status forwarder there (outside it, both were silently skipped).
            let engine = {
                let _runtime = guard.handle().enter();
                Engine::local(opened, identity.device_id.clone(), identity.scope.clone())
            };
            guard.install_host(Arc::new(engine.clone()));
            Ok(Held {
                engine: Some(engine),
                guard: Some(guard),
            })
        }
        Ok(Outcome::EngineLockHeld) => {
            eprintln!("{ENGINE_LOCK_HELD}");
            Err(Elsewhere)
        }
        Ok(Outcome::EngineAlreadyRunning) => {
            eprintln!("{ENGINE_ALREADY_RUNNING}");
            Err(Elsewhere)
        }
        Ok(Outcome::BindFailed) => {
            eprintln!("{IPC_BIND_FAILED}");
            Err(Failed(1))
        }
        Err(err) => {
            eprintln!("{err}");
            Err(Failed(1))
        }
    }
}

/// Keel is local-only: the scope is always `local`.
fn open_engine_store(
    data_dir: &std::path::Path,
    slot: &Arc<Mutex<Option<DocsStore>>>,
) -> Result<Identity, String> {
    let layout = Layout::open(data_dir).map_err(|err| err.to_string())?;
    let device_id = layout.device_id().to_string();
    let store = DocsStore::open(layout.profile_dir()).map_err(|err| err.to_string())?;
    *slot.lock().expect("store slot") = Some(store);
    Ok(Identity { device_id, scope: "local".to_string() })
}

/// `-v` without `KEEL_LOG`.
const VERBOSE_LOG: &str = "debug,loro_internal=warn,loro=warn,loro_common=warn,rustls=info,ureq=info,tungstenite=info,tokio_tungstenite=info,hyper=info,mio=info,notify=info";

fn init_tracing(verbose: bool, data_dir: &Path) {
    let directive = match std::env::var("KEEL_LOG") {
        Ok(value) => value,
        // Debug for Keel, not for the libraries: loro logs a block dump on
        // every snapshot export at INFO, and rustls/ureq/tungstenite chatter.
        Err(_) if verbose => VERBOSE_LOG.to_string(),
        Err(_) => "info,keel_ui=warn,loro_internal=warn,loro=warn".to_string(),
    };
    let filter = tracing_subscriber::EnvFilter::try_new(directive)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,keel_ui=warn,loro_internal=warn,loro=warn"));
    let log_dir = data_dir.join("logs");
    let writer = log::RotatingLogWriter::new(&log_dir, "keel.log");
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(false)
        .try_init();
}

fn data_dir(flag: Option<PathBuf>) -> Result<PathBuf, i32> {
    if let Some(path) = flag {
        return Ok(path);
    }
    if let Some(path) = std::env::var_os("KEEL_DATA_DIR") {
        return Ok(PathBuf::from(path));
    }
    #[cfg(target_os = "macos")]
    {
        let Some(home) = std::env::var_os("HOME") else {
            eprintln!("KEEL_DATA_DIR is unset and this platform has no default data directory");
            return Err(1);
        };
        Ok(PathBuf::from(home).join("Library/Application Support/Keel"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        eprintln!("KEEL_DATA_DIR is unset and this platform has no default data directory");
        Err(1)
    }
}

fn ipc_port() -> Result<u16, i32> {
    match std::env::var("KEEL_IPC_PORT") {
        Err(_) => Ok(DEFAULT_IPC_PORT),
        Ok(text) => text.parse::<u16>().map_err(|_| {
            eprintln!("KEEL_IPC_PORT is not a port");
            1
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_engine_store_creates_the_local_database() {
        let dir = std::env::temp_dir().join(format!(
            "keel-open-store-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let slot = Arc::new(Mutex::new(None));
        let identity = open_engine_store(&dir, &slot).expect("store");
        assert_eq!(identity.scope, "local");
        assert_eq!(identity.device_id.len(), 36);
        assert!(dir.join("profiles/local/store.sqlite").is_file());
        assert!(slot.lock().expect("slot").is_some());
        drop(slot);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

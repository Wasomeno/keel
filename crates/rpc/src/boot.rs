//! Startup. Probe, then flock, then the store callback, then bind.

use std::future::Future;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio::io::DuplexStream;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::client;
use crate::codec::FrameReader;
use crate::error::BootError;
use crate::host::EngineHost;
use crate::lock::{self, EngineLock, LockResult};
use crate::serve::{self, EngineState};
use crate::token;

/// Device and scope the lock holder publishes on `hello`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub device_id: String,
    pub scope: String,
}

/// A `hello` result with `proto` 1.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct HelloOk {
    pub device_id: String,
    pub scope: String,
    pub proto: u32,
    /// The engine's build stamp; 0 from an engine built before stamps.
    #[serde(default)]
    pub build: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Headed,
    Headless,
}

#[derive(Debug, Clone)]
pub struct BootConfig {
    pub data_dir: PathBuf,
    pub port: u16,
    pub mode: Mode,
    pub hello_budget: Duration,
    pub retry_for: Duration,
}

pub enum Outcome {
    /// Headed process found an engine. It does not hold the lock.
    Attach(HelloOk),
    /// This process holds the lock and serves the duplex. The socket is up when bind worked.
    Engine(ServerGuard),
    /// Headless found an engine on the first `hello`.
    EngineAlreadyRunning,
    /// The lock is held and this process did not get `hello-ok`.
    EngineLockHeld,
    /// Headless won the lock, opened the store, and could not bind.
    BindFailed,
}

/// Owns the engine runtime, the lock file, and the in-process client half.
///
/// Field order is drop order: the runtime and the host (engine, store) go
/// first, `engine.lock` is released last.
pub struct ServerGuard {
    runtime: tokio::runtime::Runtime,
    shutdown: watch::Sender<bool>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    client: Mutex<Option<(DuplexStream, FrameReader)>>,
    state: Arc<EngineState>,
    sock_path: Option<PathBuf>,
    /// Held so dropping the guard releases `engine.lock`. Must stay the last field.
    _lock: EngineLock,
}

impl ServerGuard {
    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        self.runtime.block_on(future)
    }

    pub fn handle(&self) -> tokio::runtime::Handle {
        self.runtime.handle().clone()
    }

    pub fn sock_path(&self) -> Option<&PathBuf> {
        self.sock_path.as_ref()
    }

    pub fn take_duplex(&self) -> Option<DuplexStream> {
        self.client.lock().expect("duplex").take().map(|(io, _)| io)
    }

    /// Install the engine and run its supervisor on this runtime.
    pub fn install_host(&self, host: Arc<dyn EngineHost>) {
        let task = {
            let host = Arc::clone(&host);
            self.runtime.spawn(async move {
                host.supervise().await;
            })
        };
        *self.state.host.write().expect("host") = Some(host);
        self.tasks.lock().expect("tasks").push(task);
    }

    /// Write one JSON envelope and return the raw reply frame, header included.
    pub fn duplex_exchange(&self, json: &str) -> std::io::Result<Vec<u8>> {
        self.duplex_write(json)?;
        self.duplex_read()
    }

    pub fn duplex_write(&self, json: &str) -> std::io::Result<()> {
        let mut guard = self.client.lock().expect("duplex");
        let (io, _) = guard.as_mut().ok_or_else(taken)?;
        self.runtime.block_on(async {
            crate::codec::write_frame(io, json.as_bytes())
                .await
                .map_err(std::io::Error::from)
        })
    }

    pub fn duplex_read(&self) -> std::io::Result<Vec<u8>> {
        let mut guard = self.client.lock().expect("duplex");
        let (io, frames) = guard.as_mut().ok_or_else(taken)?;
        self.runtime
            .block_on(frames.next_raw(io))
            .map_err(std::io::Error::from)
    }

    /// Cancel-safe: a timeout mid-frame keeps the partial bytes for the next read.
    pub fn duplex_read_timeout(&self, timeout: Duration) -> std::io::Result<Vec<u8>> {
        let mut guard = self.client.lock().expect("duplex");
        let (io, frames) = guard.as_mut().ok_or_else(taken)?;
        self.runtime.block_on(async {
            match tokio::time::timeout(timeout, frames.next_raw(io)).await {
                Ok(frame) => frame.map_err(std::io::Error::from),
                Err(_) => Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "duplex read timed out",
                )),
            }
        })
    }

    /// Block until SIGINT or SIGTERM (launchd and systemd stop with SIGTERM).
    /// The caller then drops the engine and this guard, which flushes and
    /// removes `keel.sock`.
    pub fn wait_for_shutdown_signal(&self) {
        self.runtime.block_on(async {
            #[cfg(unix)]
            {
                use tokio::signal::unix::{SignalKind, signal};
                let mut term = signal(SignalKind::terminate()).ok();
                let term = async {
                    match term.as_mut() {
                        Some(term) => {
                            term.recv().await;
                        }
                        None => std::future::pending::<()>().await,
                    }
                };
                tokio::select! {
                    _ = ctrl_c_or_pending() => tracing::info!(target: "keel_rpc", "SIGINT, shutting down"),
                    _ = term => tracing::info!(target: "keel_rpc", "SIGTERM, shutting down"),
                }
            }
            #[cfg(not(unix))]
            ctrl_c_or_pending().await;
        });
    }

    /// Old name. Same as [`Self::wait_for_shutdown_signal`]; also returns on SIGTERM.
    pub fn wait_for_ctrl_c(&self) {
        self.wait_for_shutdown_signal();
    }
}

async fn ctrl_c_or_pending() {
    if tokio::signal::ctrl_c().await.is_err() {
        std::future::pending::<()>().await;
    }
}

fn taken() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::NotConnected, "duplex already taken")
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        for task in self.tasks.lock().expect("tasks").iter() {
            task.abort();
        }
        if let Some(ref path) = self.sock_path {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub fn boot<F>(config: &BootConfig, open_store: F) -> Result<Outcome, BootError>
where
    F: FnOnce() -> Result<Identity, String>,
{
    let probe_rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    if let Some(hello) = probe_once(&probe_rt, config) {
        // An engine from an earlier install (the daemon, or a window that
        // never quit) would keep serving its old code to this newer app:
        // stop it and serve this build instead. Never a newer engine.
        let stale = config.mode == Mode::Headed && crate::build() > hello.build;
        if !(stale && stop_engine(config)) {
            return Ok(match config.mode {
                Mode::Headed => Outcome::Attach(hello),
                Mode::Headless => Outcome::EngineAlreadyRunning,
            });
        }
        tracing::info!(target: "keel_rpc", old = hello.build, new = crate::build(), "replaced an older engine");
    }
    match lock::try_lock(&config.data_dir.join("engine.lock"))? {
        LockResult::Held => {
            let outcome = if config.mode == Mode::Headless {
                Outcome::EngineLockHeld
            } else {
                retry_hello(&probe_rt, config)
            };
            drop(probe_rt);
            Ok(outcome)
        }
        LockResult::Acquired(lock) => {
            drop(probe_rt);
            let identity = open_store().map_err(BootError::Store)?;
            start_engine(config, identity, lock)
        }
    }
}

/// Stop the engine serving `config.port` (it holds `engine.lock`) and wait
/// for the lock to come free. Only a `keel` process is signalled, and only
/// with SIGTERM: the engine flushes and exits cleanly, and the daemon's
/// launchd job does not restart a clean exit. False when it can't be done.
#[cfg(unix)]
fn stop_engine(config: &BootConfig) -> bool {
    let run = |cmd: &str, args: &[&str]| {
        std::process::Command::new(cmd).args(args).output().ok().map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let port = format!("-iTCP:{}", config.port);
    let Some(pid) = run("/usr/sbin/lsof", &["-nP", "-t", &port, "-sTCP:LISTEN"])
        .and_then(|out| out.lines().next().and_then(|line| line.trim().parse::<i32>().ok()))
        .filter(|pid| *pid > 1 && *pid != std::process::id() as i32)
    else {
        return false;
    };
    let is_keel = run("/bin/ps", &["-o", "comm=", "-p", &pid.to_string()])
        .is_some_and(|comm| comm.trim().rsplit('/').next() == Some("keel"));
    if !is_keel {
        return false;
    }
    // SAFETY: a plain signal to a process of this user, checked to be Keel.
    if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
        return false;
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Ok(LockResult::Acquired(lock)) = lock::try_lock(&config.data_dir.join("engine.lock")) {
            drop(lock);
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

#[cfg(not(unix))]
fn stop_engine(_config: &BootConfig) -> bool {
    false
}

fn probe_once(runtime: &tokio::runtime::Runtime, config: &BootConfig) -> Option<HelloOk> {
    let token = token::load_token(&config.data_dir).unwrap_or_default();
    runtime.block_on(client::probe_hello(
        config.port,
        config.hello_budget,
        &token,
    ))
}

fn retry_hello(runtime: &tokio::runtime::Runtime, config: &BootConfig) -> Outcome {
    let deadline = Instant::now() + config.retry_for;
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Outcome::EngineLockHeld;
        }
        let slice = config
            .hello_budget
            .min(deadline.saturating_duration_since(now));
        if slice.is_zero() {
            return Outcome::EngineLockHeld;
        }
        let began = Instant::now();
        let token = token::load_token(&config.data_dir).unwrap_or_default();
        if let Some(hello) = runtime.block_on(client::probe_hello(config.port, slice, &token)) {
            return Outcome::Attach(hello);
        }
        if began.elapsed() < Duration::from_millis(15) {
            let pause =
                Duration::from_millis(20).min(deadline.saturating_duration_since(Instant::now()));
            if !pause.is_zero() {
                std::thread::sleep(pause);
            }
        }
    }
}

fn start_engine(
    config: &BootConfig,
    identity: Identity,
    lock: EngineLock,
) -> Result<Outcome, BootError> {
    let token = token::ensure_token(&config.data_dir)?;
    let state = Arc::new(EngineState {
        identity,
        token,
        host: std::sync::RwLock::new(None),
    });
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .thread_name("keel-engine")
        .enable_all()
        .build()?;
    let (shutdown, shutdown_rx) = watch::channel(false);
    let (client, server) = tokio::io::duplex(64 * 1024);
    let duplex = runtime.spawn(serve::serve_duplex(
        server,
        Arc::clone(&state),
        shutdown_rx.clone(),
    ));
    #[cfg(unix)]
    let (unix_task, sock_path) = {
        let sock_path = config.data_dir.join("keel.sock");
        let _guard = runtime.enter();
        match bind_private_socket(&sock_path) {
            Ok(listener) => {
                tracing::info!(target: "keel_rpc", path = %sock_path.display(), "unix socket bound");
                let task = runtime.spawn(serve::serve_unix(
                    listener,
                    Arc::clone(&state),
                    shutdown_rx.clone(),
                ));
                (Some(task), Some(sock_path))
            }
            Err(e) => {
                tracing::warn!(target: "keel_rpc", "failed to bind unix socket: {e}");
                (None, None)
            }
        }
    };
    #[cfg(not(unix))]
    let (unix_task, sock_path) = (None::<tokio::task::JoinHandle<()>>, None::<PathBuf>);

    match runtime.block_on(TcpListener::bind((Ipv4Addr::LOCALHOST, config.port))) {
        Ok(listener) => {
            tracing::info!(target: "keel_rpc", port = config.port, "loopback bound");
            let socket = runtime.spawn(serve::serve_ws(listener, Arc::clone(&state), shutdown_rx));
            let mut tasks = vec![duplex, socket];
            if let Some(ut) = unix_task {
                tasks.push(ut);
            }
            Ok(Outcome::Engine(ServerGuard {
                runtime,
                shutdown,
                tasks: Mutex::new(tasks),
                client: Mutex::new(Some((client, FrameReader::default()))),
                state,
                sock_path,
                _lock: lock,
            }))
        }
        Err(_) => {
            tracing::error!(target: "keel_rpc", "ipc bind failed");
            if config.mode == Mode::Headless {
                let _ = shutdown.send(true);
                duplex.abort();
                if let Some(ut) = unix_task {
                    ut.abort();
                }
                if let Some(ref path) = sock_path {
                    let _ = std::fs::remove_file(path);
                }
                drop(duplex);
                drop(shutdown_rx);
                // Engine state first, the lock last.
                drop(runtime);
                drop(state);
                drop(lock);
                Ok(Outcome::BindFailed)
            } else {
                drop(shutdown_rx);
                let mut tasks = vec![duplex];
                if let Some(ut) = unix_task {
                    tasks.push(ut);
                }
                Ok(Outcome::Engine(ServerGuard {
                    runtime,
                    shutdown,
                    tasks: Mutex::new(tasks),
                    client: Mutex::new(Some((client, FrameReader::default()))),
                    state,
                    sock_path,
                    _lock: lock,
                }))
            }
        }
    }
}

/// Bind `path` so it is never reachable with loose permissions: bind inside a
/// fresh `0700` directory, chmod the socket `0600`, then rename it into place.
#[cfg(unix)]
fn bind_private_socket(path: &std::path::Path) -> std::io::Result<tokio::net::UnixListener> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    let parent = path.parent().unwrap_or(std::path::Path::new("."));
    let staging = parent.join(format!(".keel-sock.{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::DirBuilder::new().mode(0o700).create(&staging)?;
    let result = (|| {
        let staged = staging.join("keel.sock");
        let listener = tokio::net::UnixListener::bind(&staged)?;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o600))?;
        // We hold the engine lock, so a socket left here is stale.
        let _ = std::fs::remove_file(path);
        std::fs::rename(&staged, path)?;
        Ok(listener)
    })();
    let _ = std::fs::remove_dir_all(&staging);
    result
}

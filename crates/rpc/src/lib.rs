//! Loopback RPC. One JSON envelope on an in-process duplex and on `127.0.0.1`.
//!
//! `hello` is the first method. The duplex presents an empty token. A loopback
//! socket presents `{data_dir}/ipc.token`. With no engine host the server
//! answers `hello` and `engine_status`. The binary installs a host for the rest.

mod boot;
mod client;
mod codec;
mod dispatch;
mod error;
mod host;
mod lock;
mod serve;
mod status;
mod token;

pub use boot::{BootConfig, HelloOk, Identity, Mode, Outcome, ServerGuard, boot};
pub use client::{BranchInfo, RpcClient};
pub use error::BootError;
pub use host::{EngineHost, Outbound};
pub use lock::{EngineLock, LockResult, try_lock};
pub use status::status_report;

#[cfg(test)]
mod tests;

/// Loopback port. `KEEL_IPC_PORT` overrides it in the binary.
pub const DEFAULT_IPC_PORT: u16 = 47821;

/// Budget for one WebSocket `hello`, including connect.
pub const HELLO_BUDGET: std::time::Duration = std::time::Duration::from_millis(300);

/// A socket peer must finish `hello` (handshake included) within this, or it is closed.
pub const HELLO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// How long a headed loser retries `hello` after it sees the lock held.
pub const RETRY_FOR: std::time::Duration = std::time::Duration::from_secs(2);

static BUILD: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

/// Set this binary's build stamp, once, before [`boot`]. The engine reports
/// it on `hello`; a newer app replaces an older engine it finds running.
pub fn set_build(build: u64) {
    let _ = BUILD.set(build);
}

/// This binary's build stamp; 0 when unset (tests, tools).
pub fn build() -> u64 {
    BUILD.get().copied().unwrap_or(0)
}

/// Protocol carried on `hello`.
pub const PROTO_VERSION: u32 = 1;

/// Duplex JSON body cap. Matches `PayloadTooLarge`.
pub const MAX_FRAME_BYTES: usize = 1_048_576;

/// Stderr when the lock is already held.
pub const ENGINE_LOCK_HELD: &str = "EngineLockHeld";

/// Stderr when headless finds an engine that already answered `hello`.
pub const ENGINE_ALREADY_RUNNING: &str = "EngineAlreadyRunning";

/// Stderr and tracing message when the loopback bind fails.
pub const IPC_BIND_FAILED: &str = "ipc bind failed";

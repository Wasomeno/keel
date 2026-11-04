//! Keel's macOS updater, as specified in `UPDATER.md`.
//!
//! The pure parts (manifest rules, archive entry checks, the bundle gate,
//! the daemon plist compare) are unit tested. The IO parts are blocking and
//! run on a background thread the window owns.

mod check_file;
mod fetch;
mod gate;
mod install;
mod manifest;
mod stage;
mod tar;

use std::path::PathBuf;
use std::sync::OnceLock;

pub use check_file::{check_due, mark_checked};
pub use fetch::{download, fetch_manifest, public_key_usable, verify_signature};
pub use gate::{Gate, GateError, bundle_id_of, gate, gate_exe};
pub use install::{Daemon, Installed, daemon_runs, install, spawn_waiter};
pub use manifest::{
    Asset, Check, MAX_ARCHIVE, Offer, allowed_url, evaluate, manifest_url,
};
pub use stage::{Ready, cleanup_stale, codesign_ok, find_ready, new_work_dir, stage};
pub use tar::{check_link, check_names};

/// Feed for releases. `KEEL_UPDATE_URL` overrides it (tests only).
pub const DEFAULT_MANIFEST_URL: &str =
    "https://github.com/Wasomeno/keel/releases/latest/download/latest.json";

/// The release bundle id. A debug bundle (`com.keel.dev.debug`) never updates.
pub const BUNDLE_ID: &str = "com.keel.dev";

/// Status of the window's state machine (`UPDATER.md`, "Check, download,
/// install").
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Idle,
    Checking,
    UpToDate,
    Available { version: String },
    Downloading { version: String, fraction: Option<f32> },
    Ready { version: String },
    Installing,
    Failed { message: String },
}

/// The short messages the UI shows. One place, so copy stays consistent.
pub mod msg {
    pub const UNREACHABLE: &str = "Could not reach the update server.";
    pub const BAD_SIGNATURE: &str = "The update signature is not valid.";
    pub const NO_BUILD: &str = "No build for this Mac.";
    pub const NO_KEY: &str = "This build has no update key, so it cannot verify updates.";
    pub const NOT_INSTALLED: &str = "Updates are available in the installed Keel app.";
    pub const MOVE_APP: &str = "Move Keel to the Applications folder to install updates.";
    pub const BAD_ARCHIVE: &str = "The update archive is not valid.";
    pub const BAD_APP_SIGNATURE: &str = "The downloaded Keel did not pass its code signature check.";
    pub const REDOWNLOAD: &str = "Keel could not be replaced. Download the release again from GitHub.";
}

/// Set once at startup by the app (`apps/keel`), which owns these facts.
#[derive(Debug, Clone)]
pub struct Config {
    /// `apps/keel/update-key.pub`, the minisign public key file text.
    pub public_key: &'static str,
    /// `daemon::launch_agent_plist_path()`, when it resolves.
    pub plist_path: Option<PathBuf>,
    /// The launchd label, `sh.keel.engine`.
    pub plist_label: &'static str,
}

static CONFIG: OnceLock<Config> = OnceLock::new();

/// First call wins; later calls are ignored.
pub fn init(config: Config) {
    let _ = CONFIG.set(config);
}

pub fn config() -> Option<&'static Config> {
    CONFIG.get()
}

/// Errors carry the user-facing sentence; `detail` goes to the log only.
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateError {
    pub message: String,
    pub detail: String,
}

impl UpdateError {
    pub fn new(message: &str, detail: impl Into<String>) -> Self {
        Self { message: message.to_string(), detail: detail.into() }
    }
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.detail.is_empty() {
            f.write_str(&self.message)
        } else {
            write!(f, "{} ({})", self.message, self.detail)
        }
    }
}

impl std::error::Error for UpdateError {}

pub type Result<T> = std::result::Result<T, UpdateError>;

/// `darwin-aarch64` or `darwin-x86_64`.
pub fn arch_key() -> String {
    format!("darwin-{}", std::env::consts::ARCH)
}

/// The running version, the workspace `Cargo.toml` `version`.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

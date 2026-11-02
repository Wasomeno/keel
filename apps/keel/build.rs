//! `KEEL_BUILD`: this build's stamp (Unix seconds), so a new app can tell an
//! older engine still running from an earlier install (see `keel_rpc::boot`).
//! Rerun when any of Keel's sources change, not only this crate's.

use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    println!("cargo:rustc-env=KEEL_BUILD={now}");
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=../../crates");
}

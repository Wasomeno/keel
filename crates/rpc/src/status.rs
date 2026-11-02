//! `keel status`. Keel is local-only, so the scope is always `local`; when
//! an engine answers, its device id and run count are printed too.
//!
//! This path does not take `engine.lock` and does not create device files.

use std::path::Path;
use std::time::Duration;

use crate::client;
use crate::token;

pub fn status_report(data_dir: &Path, port: u16, budget: Duration) -> String {
    let token = token::load_token(data_dir).unwrap_or_default();
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return down();
    };
    if let Some(snapshot) = runtime.block_on(client::probe_status(port, budget, &token)) {
        return format!(
            "scope: {}\ndevice_id: {}\nlive_runs: {}\n",
            snapshot.scope, snapshot.device_id, snapshot.live_runs
        );
    }
    down()
}

fn down() -> String {
    "scope: local\n".to_string()
}

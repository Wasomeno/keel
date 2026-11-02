//! Optional engine plugged in by the binary. Absent until `install_host`.

use std::future::Future;
use std::pin::Pin;

use keel_proto::RpcRequest;
use tokio::sync::mpsc;

/// Push half of one connection. Stream items are JSON envelopes with no `id`.
#[derive(Clone)]
pub struct Outbound {
    tx: mpsc::UnboundedSender<String>,
}

impl Outbound {
    pub fn pair() -> (Self, mpsc::UnboundedReceiver<String>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { tx }, rx)
    }

    pub fn push(&self, json: &str) {
        let _ = self.tx.send(json.to_string());
    }

    /// The connection that owned this stream has gone.
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}

/// Methods the engine owns. `hello` stays in the RPC crate.
pub trait EngineHost: Send + Sync {
    fn live_runs(&self) -> u64;
    fn last_watchdog(&self) -> Option<String>;

    /// `None` leaves the method unknown. `Some` is the full JSON envelope.
    fn handle(&self, request: &RpcRequest, outbound: &Outbound) -> Option<String>;

    /// Runs on the engine runtime until the host is dropped.
    fn supervise(&self) -> Pin<Box<dyn Future<Output = ()> + Send>>;
}

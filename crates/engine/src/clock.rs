//! Time source. Tests advance a manual clock instead of sleeping.

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub trait Clock: Send + Sync {
    fn now_ms(&self) -> i64;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as i64)
            .unwrap_or(0)
    }
}

pub struct ManualClock {
    ms: AtomicI64,
}

impl ManualClock {
    pub fn new(ms: i64) -> Self {
        Self {
            ms: AtomicI64::new(ms),
        }
    }

    pub fn set(&self, ms: i64) {
        self.ms.store(ms, Ordering::Relaxed);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> i64 {
        self.ms.load(Ordering::Relaxed)
    }
}

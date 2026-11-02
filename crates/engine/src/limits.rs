//! Engine constants from the session document section of the spec.

use keel_doc::ChatRow;

pub const STREAM_COMMIT_MS: i64 = 120;
pub const SNAPSHOT_FLUSH_MS: i64 = 5_000;
/// Retention, compaction and the journal/screenshot sweep run at most this
/// often from the flush (the 5 s snapshot only writes changed docs).
pub const MAINTENANCE_MS: i64 = 10 * 60 * 1000;
pub const COMPACTION_BYTES: usize = 8 * 1024 * 1024;
pub const RETAIN_DAYS: i64 = 30;
pub const RETAIN_MS: i64 = RETAIN_DAYS * 24 * 60 * 60 * 1000;
pub const STALL_WATCHDOG_MS: i64 = 10 * 60 * 1000;
pub const INTERRUPT_DEADLINE_MS: i64 = 15_000;
/// A live run quiet this long (a slow tool) refreshes `last_seen_at`, well
/// inside `keel_proto::STALE_AFTER_MS`, so it never reads as stale.
pub const HEARTBEAT_MS: i64 = 15_000;
pub const WARM_DAYS: i64 = 14;
pub const WARM_MS: i64 = WARM_DAYS * 24 * 60 * 60 * 1000;
pub const WARM_CAP: usize = 30;
pub const MAX_LIVE_RUNS: usize = 4;
pub const JOURNAL_SYNC_EVERY: u64 = 50;
pub const JOURNAL_SYNC_MS: i64 = 1_000;

/// Sessions whose `last_message_at` is inside 14 days, at most the 30 newest.
pub fn select_warm(chats: &[ChatRow], now: i64) -> Vec<String> {
    let earliest = now.saturating_sub(WARM_MS);
    let mut rows: Vec<&ChatRow> = chats
        .iter()
        .filter(|chat| !chat.archived && chat.last_message_at >= earliest)
        .collect();
    rows.sort_by(|left, right| {
        right
            .last_message_at
            .cmp(&left.last_message_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    rows.truncate(WARM_CAP);
    rows.into_iter().map(|chat| chat.id.clone()).collect()
}

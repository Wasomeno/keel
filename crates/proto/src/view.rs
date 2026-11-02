//! Sidebar order. Pure functions so a later frontend cannot invent a second sort.

use serde::{Deserialize, Serialize};

/// Stored chat-index status. `unknown` is not stored; see [`display_status`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatStatus {
    Idle,
    Running,
    AwaitingInput,
    Aborted,
}

/// Status the sidebar shows. `Unknown` is derived and is never written to the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayStatus {
    Idle,
    Running,
    AwaitingInput,
    Aborted,
    Unknown,
}

/// A running chat last seen longer ago than this is shown as [`DisplayStatus::Unknown`].
/// The comparison is strict: an age of exactly this many milliseconds stays `running`.
pub const STALE_AFTER_MS: i64 = 45_000;

/// Fields the attention sort, space grouping, and staleness rule read.
///
/// `last_message_at` and `last_seen_at` are unix milliseconds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttentionChat {
    pub id: String,
    pub space_id: String,
    pub status: ChatStatus,
    pub last_message_at: i64,
    pub last_seen_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceGroup {
    pub space_id: String,
    pub chats: Vec<AttentionChat>,
}

/// `running` whose `last_seen_at` is older than [`STALE_AFTER_MS`] is `unknown`.
/// Other statuses are unchanged. `now` and `last_seen_at` are unix milliseconds.
pub fn display_status(status: ChatStatus, last_seen_at: i64, now: i64) -> DisplayStatus {
    let stale = status == ChatStatus::Running
        && now
            .checked_sub(last_seen_at)
            .is_some_and(|age| age > STALE_AFTER_MS);
    if stale {
        return DisplayStatus::Unknown;
    }
    match status {
        ChatStatus::Idle => DisplayStatus::Idle,
        ChatStatus::Running => DisplayStatus::Running,
        ChatStatus::AwaitingInput => DisplayStatus::AwaitingInput,
        ChatStatus::Aborted => DisplayStatus::Aborted,
    }
}

/// Attention order: `awaiting_input`, then live `running`, then everything else.
/// Within a band, `last_message_at` descending, then `id` ascending.
///
/// A stale `running` chat is ordered as [`DisplayStatus::Unknown`], in the same
/// band as `idle` and `aborted`. Those two are not their own bands.
pub fn attention_sort(chats: &mut [AttentionChat], now: i64) {
    chats.sort_by(|a, b| {
        band(display_status(a.status, a.last_seen_at, now))
            .cmp(&band(display_status(b.status, b.last_seen_at, now)))
            .then(b.last_message_at.cmp(&a.last_message_at))
            .then(a.id.cmp(&b.id))
    });
}

/// Groups by `space_id` in first-seen order. Order inside a group is the input order.
/// Pass an attention-sorted slice to keep that order inside each space.
pub fn group_by_space(chats: &[AttentionChat]) -> Vec<SpaceGroup> {
    let mut groups: Vec<SpaceGroup> = Vec::new();
    for chat in chats {
        if let Some(group) = groups
            .iter_mut()
            .find(|group| group.space_id == chat.space_id)
        {
            group.chats.push(chat.clone());
        } else {
            groups.push(SpaceGroup {
                space_id: chat.space_id.clone(),
                chats: vec![chat.clone()],
            });
        }
    }
    groups
}

fn band(status: DisplayStatus) -> u8 {
    match status {
        DisplayStatus::AwaitingInput => 0,
        DisplayStatus::Running => 1,
        DisplayStatus::Idle | DisplayStatus::Aborted | DisplayStatus::Unknown => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(
        id: &str,
        space_id: &str,
        status: ChatStatus,
        last_message_at: i64,
        last_seen_at: i64,
    ) -> AttentionChat {
        AttentionChat {
            id: id.to_string(),
            space_id: space_id.to_string(),
            status,
            last_message_at,
            last_seen_at,
        }
    }

    #[test]
    fn running_becomes_unknown_only_after_45s() {
        let seen = 1_000_000;
        assert_eq!(
            display_status(ChatStatus::Running, seen, seen + STALE_AFTER_MS),
            DisplayStatus::Running
        );
        assert_eq!(
            display_status(ChatStatus::Running, seen, seen + STALE_AFTER_MS + 1),
            DisplayStatus::Unknown
        );
        assert_eq!(
            display_status(ChatStatus::Running, seen, seen - 1),
            DisplayStatus::Running
        );
        assert_eq!(
            display_status(ChatStatus::Idle, seen, seen + STALE_AFTER_MS + 1),
            DisplayStatus::Idle
        );
        assert_eq!(
            display_status(ChatStatus::AwaitingInput, seen, seen + STALE_AFTER_MS + 1),
            DisplayStatus::AwaitingInput
        );
        assert_eq!(
            display_status(ChatStatus::Aborted, seen, seen + STALE_AFTER_MS + 1),
            DisplayStatus::Aborted
        );
    }

    #[test]
    fn attention_order_is_waiting_then_live_then_the_rest_by_recency() {
        let now = 1_000_000;
        let mut chats = vec![
            chat("aborted", "s", ChatStatus::Aborted, 70, 0),
            chat("idle", "s", ChatStatus::Idle, 80, 0),
            chat(
                "stale",
                "s",
                ChatStatus::Running,
                100,
                now - (STALE_AFTER_MS + 1),
            ),
            chat("run-old", "s", ChatStatus::Running, 40, now),
            chat("run-new", "s", ChatStatus::Running, 50, now),
            chat("wait", "s", ChatStatus::AwaitingInput, 10, now),
            chat("tie-b", "s", ChatStatus::Idle, 80, 0),
            chat("tie-a", "s", ChatStatus::Idle, 80, 0),
        ];
        attention_sort(&mut chats, now);
        let ids: Vec<_> = chats.iter().map(|chat| chat.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "wait", "run-new", "run-old", "stale", "idle", "tie-a", "tie-b", "aborted"
            ]
        );
    }

    #[test]
    fn spaces_group_in_first_seen_order_without_resorting() {
        let chats = vec![
            chat("1", "alpha", ChatStatus::AwaitingInput, 3, 0),
            chat("2", "beta", ChatStatus::Running, 2, 0),
            chat("3", "alpha", ChatStatus::Idle, 1, 0),
        ];
        let groups = group_by_space(&chats);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].space_id, "alpha");
        assert_eq!(
            groups[0]
                .chats
                .iter()
                .map(|chat| chat.id.as_str())
                .collect::<Vec<_>>(),
            ["1", "3"]
        );
        assert_eq!(groups[1].space_id, "beta");
        assert_eq!(groups[1].chats[0].id, "2");
    }
}

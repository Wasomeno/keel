//! Notifications: which chat events alert, the unread badge, and the sound
//! debounce. Pure state; the macOS calls are in [`crate::platform_notify`].

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use keel_proto::{BadgeStyle, ChatStatus, NotifySettings};

/// A chat event worth telling you about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyEvent {
    /// The agent finished its turn.
    Finished,
    /// The agent asks a question or for permission.
    NeedsInput,
    /// The turn ended in an error (or was cut off).
    Error,
}

impl NotifyEvent {
    /// A registry status change, as an event. `None` for anything else
    /// (a turn starting, a status seen for the first time).
    pub fn from_status_change(before: ChatStatus, after: ChatStatus) -> Option<Self> {
        match (before, after) {
            (a, b) if a == b => None,
            (_, ChatStatus::AwaitingInput) => Some(Self::NeedsInput),
            (ChatStatus::Running, ChatStatus::Idle) => Some(Self::Finished),
            (ChatStatus::Running | ChatStatus::AwaitingInput, ChatStatus::Aborted) => Some(Self::Error),
            _ => None,
        }
    }

    pub fn enabled(self, settings: &NotifySettings) -> bool {
        match self {
            Self::Finished => settings.on_finished,
            Self::NeedsInput => settings.on_needs_input,
            Self::Error => settings.on_error,
        }
    }

    /// Banner body.
    pub fn message(self) -> &'static str {
        match self {
            Self::Finished => "Finished",
            Self::NeedsInput => "Needs your input",
            Self::Error => "Stopped with an error",
        }
    }
}

/// Alert only when you aren't already looking at the chat: another chat is
/// open, or Keel isn't the focused window.
pub fn should_alert(event: NotifyEvent, settings: &NotifySettings, viewing_chat: bool, window_active: bool) -> bool {
    event.enabled(settings) && !(viewing_chat && window_active)
}

/// The sidebar row's mark for one chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowBadge {
    Dot,
    Count(u32),
}

/// Chats with alerts you haven't seen, and how many each.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Unread {
    chats: BTreeMap<String, (u32, NotifyEvent)>,
}

impl Unread {
    pub fn mark(&mut self, chat_id: &str, event: NotifyEvent) {
        let entry = self.chats.entry(chat_id.to_string()).or_insert((0, event));
        entry.0 += 1;
        entry.1 = event;
    }

    /// True when the chat had unread alerts.
    pub fn mark_read(&mut self, chat_id: &str) -> bool {
        self.chats.remove(chat_id).is_some()
    }

    /// Keep only chats that still exist (deleted or archived ones go).
    pub fn retain(&mut self, mut keep: impl FnMut(&str) -> bool) -> bool {
        let before = self.chats.len();
        self.chats.retain(|id, _| keep(id));
        before != self.chats.len()
    }

    pub fn chat_count(&self) -> usize {
        self.chats.len()
    }

    pub fn latest(&self, chat_id: &str) -> Option<NotifyEvent> {
        self.chats.get(chat_id).map(|(_, event)| *event)
    }

    /// The Dock badge label: the number of chats needing you, a dot, or
    /// nothing (also when nothing is unread).
    pub fn dock_label(&self, style: BadgeStyle) -> Option<String> {
        let n = self.chats.len();
        match style {
            _ if n == 0 => None,
            BadgeStyle::Count => Some(n.to_string()),
            BadgeStyle::Dot => Some("•".to_string()),
            BadgeStyle::None => None,
        }
    }

    pub fn row_badge(&self, chat_id: &str, style: BadgeStyle) -> Option<RowBadge> {
        let (count, _) = self.chats.get(chat_id)?;
        match style {
            BadgeStyle::Count => Some(RowBadge::Count(*count)),
            BadgeStyle::Dot => Some(RowBadge::Dot),
            BadgeStyle::None => None,
        }
    }
}

/// A burst of events (several chats finishing together) plays one sound.
#[derive(Debug, Default)]
pub struct SoundGate {
    last: Option<Instant>,
}

impl SoundGate {
    pub const WINDOW: Duration = Duration::from_millis(1500);

    /// True when a sound may play now; it then counts as played.
    pub fn allow(&mut self, now: Instant) -> bool {
        if self.last.is_some_and(|at| now.saturating_duration_since(at) < Self::WINDOW) {
            return false;
        }
        self.last = Some(now);
        true
    }
}

/// Should the sound play for an alert, given where you are.
pub fn should_play_sound(settings: &NotifySettings, app_active: bool) -> bool {
    !matches!(settings.sound, keel_proto::SoundChoice::None) && settings.volume > 0. && !(settings.sound_in_background_only && app_active)
}

/// The banner's payload key for the chat it is about.
pub const BANNER_CHAT_KEY: &str = "chat_id";

/// What a banner carries: the chat it is about.
pub fn banner_payload(chat_id: &str) -> [(&'static str, String); 1] {
    [(BANNER_CHAT_KEY, chat_id.to_string())]
}

/// The chat a clicked banner (via `lookup` into its payload) or a
/// `keel://open-chat?id=…` / `keel://chat/…` link points at.
pub fn chat_from_payload(lookup: impl Fn(&str) -> Option<String>) -> Option<String> {
    lookup(BANNER_CHAT_KEY).map(|id| id.trim().to_string()).filter(|id| !id.is_empty())
}

pub fn chat_from_url(url: &str) -> Option<String> {
    let rest = url.strip_prefix("keel://")?;
    let id = if let Some(query) = rest.strip_prefix("open-chat?") {
        query.split('&').find_map(|pair| pair.strip_prefix("id="))?
    } else {
        rest.strip_prefix("chat/")?
    };
    let id = id.trim_end_matches('/');
    (!id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')).then(|| id.to_string())
}

/// What clicking a banner does: Keel comes to the front and, when the chat
/// is still around, opens it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BannerAction {
    OpenChat(String),
    /// The chat is gone (deleted, or never known): just bring Keel forward.
    Activate,
}

pub fn banner_action(chat_id: &str, known: impl Fn(&str) -> bool) -> BannerAction {
    if known(chat_id) { BannerAction::OpenChat(chat_id.to_string()) } else { BannerAction::Activate }
}

#[cfg(test)]
mod tests {
    #[test]
    fn banner_click_routes_to_its_chat() {
        use super::{BannerAction, banner_action, banner_payload, chat_from_payload, chat_from_url};
        let payload = banner_payload("chat-1");
        let lookup = |key: &str| payload.iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone());
        let id = chat_from_payload(lookup).unwrap();
        assert_eq!(banner_action(&id, |c| c == "chat-1"), BannerAction::OpenChat("chat-1".into()));
        assert_eq!(banner_action(&id, |_| false), BannerAction::Activate);
        assert_eq!(chat_from_payload(|_| Some("  ".into())), None);
        assert_eq!(chat_from_payload(|_| None), None);
        assert_eq!(chat_from_url("keel://open-chat?id=ab-12"), Some("ab-12".into()));
        assert_eq!(chat_from_url("keel://open-chat?x=1&id=ab"), Some("ab".into()));
        assert_eq!(chat_from_url("keel://chat/ab_1/"), Some("ab_1".into()));
        assert_eq!(chat_from_url("keel://chat/a b"), None);
        assert_eq!(chat_from_url("https://chat/ab"), None);
    }

    use super::*;
    use keel_proto::SoundChoice;

    #[test]
    fn status_changes_map_to_events() {
        use ChatStatus::*;
        assert_eq!(NotifyEvent::from_status_change(Running, Idle), Some(NotifyEvent::Finished));
        assert_eq!(NotifyEvent::from_status_change(Running, AwaitingInput), Some(NotifyEvent::NeedsInput));
        assert_eq!(NotifyEvent::from_status_change(Idle, AwaitingInput), Some(NotifyEvent::NeedsInput));
        assert_eq!(NotifyEvent::from_status_change(Running, Aborted), Some(NotifyEvent::Error));
        assert_eq!(NotifyEvent::from_status_change(Idle, Running), None);
        assert_eq!(NotifyEvent::from_status_change(Running, Running), None);
        assert_eq!(NotifyEvent::from_status_change(Idle, Aborted), None);
        assert_eq!(NotifyEvent::from_status_change(AwaitingInput, Running), None);
    }

    #[test]
    fn alerts_skip_the_chat_you_are_looking_at() {
        let mut s = NotifySettings::default();
        assert!(!should_alert(NotifyEvent::Finished, &s, true, true));
        assert!(should_alert(NotifyEvent::Finished, &s, true, false), "Keel in the background");
        assert!(should_alert(NotifyEvent::Finished, &s, false, true), "another chat open");
        s.on_finished = false;
        assert!(!should_alert(NotifyEvent::Finished, &s, false, false));
        assert!(should_alert(NotifyEvent::Error, &s, false, false));
    }

    #[test]
    fn badge_counts_chats_and_clears_on_read() {
        let mut u = Unread::default();
        assert_eq!(u.dock_label(BadgeStyle::Count), None);
        assert_eq!(u.dock_label(BadgeStyle::Dot), None);
        u.mark("a", NotifyEvent::Finished);
        u.mark("a", NotifyEvent::NeedsInput);
        u.mark("b", NotifyEvent::Error);
        assert_eq!(u.dock_label(BadgeStyle::Count).as_deref(), Some("2"));
        assert_eq!(u.dock_label(BadgeStyle::Dot).as_deref(), Some("•"));
        assert_eq!(u.dock_label(BadgeStyle::None), None);
        assert_eq!(u.row_badge("a", BadgeStyle::Count), Some(RowBadge::Count(2)));
        assert_eq!(u.row_badge("a", BadgeStyle::Dot), Some(RowBadge::Dot));
        assert_eq!(u.row_badge("a", BadgeStyle::None), None);
        assert_eq!(u.latest("a"), Some(NotifyEvent::NeedsInput));
        assert!(u.mark_read("a"));
        assert!(!u.mark_read("a"));
        assert_eq!(u.row_badge("a", BadgeStyle::Count), None);
        assert_eq!(u.dock_label(BadgeStyle::Count).as_deref(), Some("1"));
        assert!(u.retain(|id| id != "b"));
        assert_eq!(u.dock_label(BadgeStyle::Count), None);
    }

    #[test]
    fn a_burst_plays_one_sound() {
        let mut gate = SoundGate::default();
        let t0 = Instant::now();
        assert!(gate.allow(t0));
        assert!(!gate.allow(t0 + Duration::from_millis(200)));
        assert!(!gate.allow(t0 + Duration::from_millis(1400)));
        assert!(gate.allow(t0 + Duration::from_millis(1600)));
    }

    #[test]
    fn sound_respects_background_only_and_none() {
        let mut s = NotifySettings::default();
        assert!(!should_play_sound(&s, true), "default: background only");
        assert!(should_play_sound(&s, false));
        s.sound_in_background_only = false;
        assert!(should_play_sound(&s, true));
        s.sound = SoundChoice::None;
        assert!(!should_play_sound(&s, false));
        s.sound = SoundChoice::System("Ping".into());
        s.volume = 0.;
        assert!(!should_play_sound(&s, false));
    }
}

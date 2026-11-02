//! `ui-settings.json` schema and helpers.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// A harness's last model, thinking effort, and permission mode.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HarnessPrefs {
    #[serde(default)]
    pub model: String,
    /// `model`'s display name, so the chip reads right before the model list loads.
    #[serde(default)]
    pub model_name: String,
    #[serde(default)]
    pub effort: String,
    /// [`crate::PermissionMode`] name; a string so an unknown one reads as Ask
    /// instead of failing the whole file.
    #[serde(default)]
    pub permission: String,
}

/// `ui-settings.json`. Absent `theme` follows the system appearance.
/// Absent `reduced_motion` follows the OS setting.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UiSettings {
    #[serde(rename = "openTabs", default)]
    pub open_tabs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<ThemeChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reduced_motion: Option<bool>,
    /// Sidebar width in logical pixels; absent is the default width.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidebar_width: Option<f32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sidebar_collapsed: bool,
    /// Composer drafts keyed by chat id. Device-local, not a document field.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub drafts: BTreeMap<String, String>,
    /// UI zoom factor (⌘+ / ⌘-); absent is 1.0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zoom: Option<f32>,
    /// What was last picked per harness id, for a new chat on that harness.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub harness_prefs: BTreeMap<String, HarnessPrefs>,
    /// The model menu's "Recent": the last few model ids picked per
    /// harness id, newest first.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub recent_models: BTreeMap<String, Vec<String>>,
    /// The model menu's open provider sections per harness id. Absent: only
    /// the current model's provider starts open.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub open_model_providers: BTreeMap<String, Vec<String>>,
    /// Settings → Notifications. Absent (older files) is every default.
    #[serde(default, skip_serializing_if = "NotifySettings::is_default")]
    pub notifications: NotifySettings,
    /// Settings → Background: a picture imported into the data dir, by file
    /// name under `backgrounds/`. Absent is the built-in landscape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    /// How much the background is darkened, 0 (none) to [`MAX_BACKGROUND_DIM`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_dim: Option<f32>,
    /// Settings → Background → Chat glow: the picture's faint, blurred glow
    /// at the foot of a chat, under the composer. On unless turned off.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub chat_glow_off: bool,
    /// Settings → Connections → Dotify: remind me when Keel has been busy
    /// for 15 minutes on a workday and I'm not clocked in. Off by default.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dotify_nudge: bool,
    /// Sidebar space sections folded shut on this device, by space id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collapsed_spaces: Vec<String>,
    /// Settings → Browser: the only sites agents may use in the browser
    /// pane (`example.com`, `localhost:3000`, …). Empty allows every site.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub browser_allowed_sites: Vec<String>,
    /// Chats whose agents have the browser turned off (the pane's toggle).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub browser_off_chats: Vec<String>,
}

/// Strongest background dim the slider offers.
pub const MAX_BACKGROUND_DIM: f32 = 0.8;

/// What the Dock icon and sidebar rows show for chats that need you.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BadgeStyle {
    /// How many chats need attention.
    #[default]
    Count,
    /// A dot, no number.
    Dot,
    None,
}

/// The alert sound.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SoundChoice {
    None,
    /// A macOS sound from `/System/Library/Sounds`, by name ("Glass").
    System(String),
    /// A file imported into the data dir, by file name under `sounds/`.
    Custom(String),
    /// [`DEFAULT_SOUND`].
    #[default]
    Default,
}

/// The sound `SoundChoice::Default` plays.
pub const DEFAULT_SOUND: &str = "Glass";

/// Settings → Notifications. Every field has a default, so a file that has
/// only some of them (or none) still loads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotifySettings {
    /// An agent finished its turn.
    pub on_finished: bool,
    /// An agent asks a question or for permission.
    pub on_needs_input: bool,
    /// A turn ended in an error (or was cut off).
    pub on_error: bool,
    pub badge: BadgeStyle,
    /// Bounce the Dock icon once when an agent needs input.
    pub bounce: bool,
    /// A macOS notification banner.
    pub banner: bool,
    pub sound: SoundChoice,
    /// 0 to 1.
    pub volume: f32,
    /// Sound only while Keel isn't the focused app.
    pub sound_in_background_only: bool,
}

impl Default for NotifySettings {
    fn default() -> Self {
        Self {
            on_finished: true,
            on_needs_input: true,
            on_error: true,
            badge: BadgeStyle::Count,
            bounce: true,
            banner: false,
            sound: SoundChoice::Default,
            volume: 0.7,
            sound_in_background_only: true,
        }
    }
}

impl NotifySettings {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Explicit theme in `ui-settings.json`. System appearance is the absent value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeChoice {
    Light,
    Dark,
}

impl UiSettings {
    pub fn read_from(path: &Path) -> Result<Self, std::io::Error> {
        let text = fs::read_to_string(path)?;
        serde_json::from_str(&text)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    pub fn write_to(&self, path: &Path) -> Result<(), std::io::Error> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, format!("{text}\n"))?;
        fs::rename(&tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_from_before_notifications_still_load_with_defaults() {
        let old = r#"{"openTabs":["a"],"sidebar_collapsed":true,"drafts":{"a":"hi"},"hidden_harnesses":["codex"]}"#;
        let settings: UiSettings = serde_json::from_str(old).unwrap();
        assert_eq!(settings.open_tabs, vec!["a"]);
        assert_eq!(settings.notifications, NotifySettings::default());
        assert_eq!((settings.background.as_deref(), settings.background_dim), (None, None));
        // Untouched defaults are not written back.
        let written = serde_json::to_string(&settings).unwrap();
        assert!(!written.contains("notifications") && !written.contains("background"), "{written}");
    }

    #[test]
    fn partial_notification_settings_fill_in_the_rest() {
        let settings: UiSettings = serde_json::from_str(r#"{"notifications":{"badge":"dot","on_error":false}}"#).unwrap();
        let n = &settings.notifications;
        assert_eq!((n.badge, n.on_error, n.on_finished, n.sound.clone()), (BadgeStyle::Dot, false, true, SoundChoice::Default));
    }

    #[test]
    fn every_setting_round_trips() {
        let settings = UiSettings {
            notifications: NotifySettings {
                on_finished: false,
                on_needs_input: true,
                on_error: false,
                badge: BadgeStyle::None,
                bounce: false,
                banner: true,
                sound: SoundChoice::Custom("ding.wav".into()),
                volume: 0.25,
                sound_in_background_only: false,
            },
            background: Some("bg-1.jpg".into()),
            background_dim: Some(0.4),
            dotify_nudge: true,
            ..UiSettings::default()
        };
        let text = serde_json::to_string(&settings).unwrap();
        assert_eq!(serde_json::from_str::<UiSettings>(&text).unwrap(), settings);
        for sound in [SoundChoice::None, SoundChoice::System("Ping".into()), SoundChoice::Default] {
            let text = serde_json::to_string(&sound).unwrap();
            assert_eq!(serde_json::from_str::<SoundChoice>(&text).unwrap(), sound);
        }
    }
}

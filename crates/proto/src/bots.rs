//! Persistent bots registry (`{data_dir}/bots.json`) and creature appearance model.
//!
//! Keel always ships with one built-in bot, **Dr. Keelee** (`dr-keelee`), who is the
//! sole creator of new bots. Newly created bots start with the default naked creature
//! appearance and can be customized freely from the Bot Customizer modal.

use serde::{Deserialize, Serialize};

/// File name under Keel's data directory.
pub const FILE: &str = "bots.json";

/// Built-in creator bot id. Always exists and cannot be deleted.
pub const DR_KEELEE_ID: &str = "dr-keelee";

/// Default display name for the built-in creator bot.
pub const DR_KEELEE_NAME: &str = "Dr. Keelee";

/// Default tagline for Dr. Keelee.
pub const DR_KEELEE_TAGLINE: &str = "Bot architect & lab director";

/// Creature body color theme (also tints messaging accents & studio backdrop).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotColor {
    #[default]
    Clay,
    Coral,
    Amber,
    Sage,
    Mint,
    Sky,
    Indigo,
    Plum,
}

impl BotColor {
    pub const ALL: &'static [Self] = &[
        Self::Clay,
        Self::Coral,
        Self::Amber,
        Self::Sage,
        Self::Mint,
        Self::Sky,
        Self::Indigo,
        Self::Plum,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Clay => "Clay",
            Self::Coral => "Coral",
            Self::Amber => "Amber",
            Self::Sage => "Sage",
            Self::Mint => "Mint",
            Self::Sky => "Sky",
            Self::Indigo => "Indigo",
            Self::Plum => "Plum",
        }
    }

    /// `(body_hex, shadow_hex, belly_hex, accent_rgba)`
    pub fn palette(self) -> (&'static str, &'static str, &'static str, u32) {
        match self {
            Self::Clay => ("#E9C7B0", "#CFA58B", "#F6E2D4", 0xC98862FF),
            Self::Coral => ("#F28B7A", "#D66857", "#FCC3B9", 0xE05E4BFF),
            Self::Amber => ("#F4C364", "#D9A13E", "#FCE1A6", 0xD98E24FF),
            Self::Sage => ("#92C298", "#6FA376", "#C8E4CB", 0x4E945BFF),
            Self::Mint => ("#6FD0BE", "#4BB09E", "#B8EDE3", 0x2F9E8BFF),
            Self::Sky => ("#78B7F5", "#5195DC", "#C2E0FC", 0x3482E0FF),
            Self::Indigo => ("#8D91F0", "#686CD4", "#CCCFFD", 0x5B5FE0FF),
            Self::Plum => ("#C78DE6", "#A467C7", "#E6C7F7", 0x9C57C4FF),
        }
    }

    /// Primary RGBA accent color for UI badges, rings, and message bubbles.
    pub fn accent_rgba(self) -> u32 {
        self.palette().3
    }
}

/// Creature clothing / attire.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotAttire {
    #[default]
    Naked,
    Doctor,
    Hoodie,
    Suit,
    Overalls,
    Cape,
    Sweater,
}

impl BotAttire {
    pub const ALL: &'static [Self] = &[
        Self::Naked,
        Self::Doctor,
        Self::Hoodie,
        Self::Suit,
        Self::Overalls,
        Self::Cape,
        Self::Sweater,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Naked => "Naked",
            Self::Doctor => "Doctor Coat",
            Self::Hoodie => "Hoodie",
            Self::Suit => "Suit & Tie",
            Self::Overalls => "Overalls",
            Self::Cape => "Hero Cape",
            Self::Sweater => "Sweater",
        }
    }
}

/// Creature eye style.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotEyes {
    #[default]
    Dot,
    Wide,
    Sleepy,
    Specs,
    Shades,
    Wink,
}

impl BotEyes {
    pub const ALL: &'static [Self] = &[
        Self::Dot,
        Self::Wide,
        Self::Sleepy,
        Self::Specs,
        Self::Shades,
        Self::Wink,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Dot => "Classic",
            Self::Wide => "Curious",
            Self::Sleepy => "Chill",
            Self::Specs => "Lab Specs",
            Self::Shades => "Shades",
            Self::Wink => "Wink",
        }
    }
}

/// Creature hair style.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotHair {
    #[default]
    None,
    Einstein,
    Tuft,
    Spiky,
    Bob,
    Mohawk,
    Beanie,
}

impl BotHair {
    pub const ALL: &'static [Self] = &[
        Self::None,
        Self::Einstein,
        Self::Tuft,
        Self::Spiky,
        Self::Bob,
        Self::Mohawk,
        Self::Beanie,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Einstein => "Einstein",
            Self::Tuft => "Sprout",
            Self::Spiky => "Spiky",
            Self::Bob => "Sleek Bob",
            Self::Mohawk => "Mohawk",
            Self::Beanie => "Beanie",
        }
    }
}

/// Creature mustache / facial hair style.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotMustache {
    #[default]
    None,
    Einstein,
    Handlebar,
    Chevron,
    Pencil,
    Goatee,
}

impl BotMustache {
    pub const ALL: &'static [Self] = &[
        Self::None,
        Self::Einstein,
        Self::Handlebar,
        Self::Chevron,
        Self::Pencil,
        Self::Goatee,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Einstein => "Einstein",
            Self::Handlebar => "Handlebar",
            Self::Chevron => "Chevron",
            Self::Pencil => "Pencil",
            Self::Goatee => "Goatee",
        }
    }
}

/// Creature body size / proportions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotSize {
    Small,
    #[default]
    Medium,
    Large,
}

impl BotSize {
    pub const ALL: &'static [Self] = &[Self::Small, Self::Medium, Self::Large];

    pub fn label(self) -> &'static str {
        match self {
            Self::Small => "Small",
            Self::Medium => "Medium",
            Self::Large => "Large",
        }
    }

    /// Relative SVG scale multiplier for the creature inside its canvas.
    pub fn scale(self) -> f32 {
        match self {
            Self::Small => 0.84,
            Self::Medium => 1.0,
            Self::Large => 1.13,
        }
    }
}

/// Visual appearance of a bot creature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BotAppearance {
    #[serde(default)]
    pub color: BotColor,
    #[serde(default)]
    pub attire: BotAttire,
    #[serde(default)]
    pub eyes: BotEyes,
    #[serde(default)]
    pub hair: BotHair,
    #[serde(default)]
    pub mustache: BotMustache,
    #[serde(default)]
    pub size: BotSize,
}

impl Default for BotAppearance {
    fn default() -> Self {
        Self::naked_default()
    }
}

impl BotAppearance {
    /// The default look for every newly hatched bot: a naked creature.
    pub const fn naked_default() -> Self {
        Self {
            color: BotColor::Clay,
            attire: BotAttire::Naked,
            eyes: BotEyes::Dot,
            hair: BotHair::None,
            mustache: BotMustache::None,
            size: BotSize::Medium,
        }
    }

    /// Dr. Keelee's signature look: doctor attire + Einstein hair & mustache + lab specs.
    pub const fn dr_keelee_default() -> Self {
        Self {
            color: BotColor::Mint,
            attire: BotAttire::Doctor,
            eyes: BotEyes::Specs,
            hair: BotHair::Einstein,
            mustache: BotMustache::Einstein,
            size: BotSize::Medium,
        }
    }
}

/// One bot entry persisted in `{data_dir}/bots.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bot {
    pub id: String,
    #[serde(default)]
    pub builtin: bool,
    pub name: String,
    #[serde(default)]
    pub tagline: String,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub anti_jobs: Vec<String>,
    /// The bot's continuous 1:1 DM chat id in Keel's registry.
    #[serde(default)]
    pub chat_id: Option<String>,
    #[serde(default)]
    pub appearance: BotAppearance,
    #[serde(default)]
    pub created_at: u64,
}

impl Bot {
    /// Whether this is the built-in Dr. Keelee bot creator.
    pub fn is_creator(&self) -> bool {
        self.builtin && self.id == DR_KEELEE_ID
    }

    /// Built-in Dr. Keelee seed entry.
    pub fn dr_keelee() -> Self {
        Self {
            id: DR_KEELEE_ID.to_string(),
            builtin: true,
            name: DR_KEELEE_NAME.to_string(),
            tagline: DR_KEELEE_TAGLINE.to_string(),
            system_prompt: String::new(),
            anti_jobs: vec![
                "never build bloated do-everything bots".to_string(),
                "never skip anti-jobs or voice definition".to_string(),
            ],
            chat_id: None,
            appearance: BotAppearance::dr_keelee_default(),
            created_at: 0,
        }
    }

    /// Effective host system prompt for a created bot (combines identity, prompt, and anti-jobs).
    pub fn effective_system_prompt(&self) -> String {
        let mut out = format!("You are {}, a specialized Keel bot.", self.name.trim());
        if !self.tagline.trim().is_empty() {
            out.push_str(&format!("\nRole: {}", self.tagline.trim()));
        }
        if !self.system_prompt.trim().is_empty() {
            out.push_str("\n\n");
            out.push_str(self.system_prompt.trim());
        }
        let anti: Vec<&str> = self
            .anti_jobs
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        if !anti.is_empty() {
            out.push_str("\n\n## Anti-jobs (never do these)\n");
            for rule in anti {
                out.push_str("- ");
                out.push_str(rule);
                out.push('\n');
            }
        }
        out
    }
}

/// Complete bots registry stored in `{data_dir}/bots.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotsStore {
    #[serde(default)]
    pub bots: Vec<Bot>,
}

impl Default for BotsStore {
    fn default() -> Self {
        let mut store = Self { bots: Vec::new() };
        store.ensure_dr_keelee();
        store
    }
}

impl BotsStore {
    /// Parse JSON from disk, always guaranteeing Dr. Keelee exists and is pinned first.
    pub fn parse(text: &str) -> Self {
        let mut store: Self = serde_json::from_str(text).unwrap_or_default();
        store.ensure_dr_keelee();
        store
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }

    /// Guarantees the built-in Dr. Keelee bot exists and is at index 0.
    pub fn ensure_dr_keelee(&mut self) {
        if let Some(pos) = self.bots.iter().position(|b| b.id == DR_KEELEE_ID) {
            self.bots[pos].builtin = true;
            if pos != 0 {
                let dr = self.bots.remove(pos);
                self.bots.insert(0, dr);
            }
        } else {
            self.bots.insert(0, Bot::dr_keelee());
        }
    }

    pub fn by_id(&self, id: &str) -> Option<&Bot> {
        self.bots.iter().find(|b| b.id == id)
    }

    pub fn by_id_mut(&mut self, id: &str) -> Option<&mut Bot> {
        self.bots.iter_mut().find(|b| b.id == id)
    }

    pub fn by_chat_id(&self, chat_id: &str) -> Option<&Bot> {
        self.bots
            .iter()
            .find(|b| b.chat_id.as_deref() == Some(chat_id))
    }

    pub fn is_bot_chat(&self, chat_id: &str) -> bool {
        self.by_chat_id(chat_id).is_some()
    }

    /// All chat ids owned by bots (used to keep bot DMs out of regular Spaces/Chats).
    pub fn chat_ids(&self) -> impl Iterator<Item = &str> {
        self.bots.iter().filter_map(|b| b.chat_id.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_store_always_has_dr_keelee_first() {
        let store = BotsStore::default();
        assert_eq!(store.bots.len(), 1);
        let dr = &store.bots[0];
        assert_eq!(dr.id, DR_KEELEE_ID);
        assert_eq!(dr.name, DR_KEELEE_NAME);
        assert!(dr.is_creator());
        assert_eq!(dr.appearance.attire, BotAttire::Doctor);
        assert_eq!(dr.appearance.hair, BotHair::Einstein);
        assert_eq!(dr.appearance.mustache, BotMustache::Einstein);
    }

    #[test]
    fn newly_hatched_bot_defaults_to_naked_creature() {
        let look = BotAppearance::naked_default();
        assert_eq!(look.attire, BotAttire::Naked);
        assert_eq!(look.hair, BotHair::None);
        assert_eq!(look.mustache, BotMustache::None);
        assert_eq!(look.eyes, BotEyes::Dot);
        assert_eq!(look.size, BotSize::Medium);
    }

    #[test]
    fn parse_repairs_missing_or_reordered_dr_keelee() {
        let raw = r#"{"bots":[{"id":"scout","name":"Scout"}]}"#;
        let store = BotsStore::parse(raw);
        assert_eq!(store.bots.len(), 2);
        assert_eq!(store.bots[0].id, DR_KEELEE_ID);
        assert_eq!(store.bots[1].id, "scout");
        assert_eq!(store.bots[1].appearance, BotAppearance::naked_default());
    }
}

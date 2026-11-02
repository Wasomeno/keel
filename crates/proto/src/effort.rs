//! Thinking-effort levels that Keel no longer offers, and where a saved
//! choice of one lands now.

/// The old "poteto" effort (one stop past a model's highest level, plus a
/// prompt preamble). Removed; sessions and prefs may still carry it.
pub const RETIRED_POTETO: &str = "poteto";

/// What a saved `level` means today, given the model's `levels`. A live
/// level is kept as is. The retired poteto ran at `high` (else the model's
/// highest thinking level), so it lands there; with no thinking level to
/// land on it is the harness default (`""`).
pub fn migrate_effort(level: &str, levels: &[String]) -> String {
    if level != RETIRED_POTETO {
        return level.to_string();
    }
    levels
        .iter()
        .find(|l| *l == "high")
        .or_else(|| levels.iter().rev().find(|l| !matches!(l.as_str(), "off" | "none" | RETIRED_POTETO)))
        .cloned()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn levels(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn saved_poteto_lands_on_high_then_the_top_then_default() {
        assert_eq!(migrate_effort("poteto", &levels(&["low", "medium", "high", "xhigh"])), "high");
        assert_eq!(migrate_effort("poteto", &levels(&["minimal", "low", "medium"])), "medium");
        assert_eq!(migrate_effort("poteto", &levels(&["off"])), "");
        assert_eq!(migrate_effort("poteto", &[]), "");
        assert_eq!(migrate_effort("low", &levels(&["low", "high"])), "low");
        assert_eq!(migrate_effort("", &levels(&["low"])), "");
    }
}

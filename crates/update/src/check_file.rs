//! `update-check.json` beside `ui-settings.json`: when the last automatic
//! check got a parsed response. Not part of `UiSettings`.

use std::path::Path;

const FILE: &str = "update-check.json";
const DAY: u64 = 24 * 60 * 60;

#[derive(serde::Serialize, serde::Deserialize)]
struct CheckFile {
    checked_at: u64,
}

/// True when the file is missing, unreadable, or older than a day.
pub fn check_due(data_dir: &Path, now: u64) -> bool {
    let Ok(text) = std::fs::read_to_string(data_dir.join(FILE)) else {
        return true;
    };
    match serde_json::from_str::<CheckFile>(&text) {
        Ok(f) => now.saturating_sub(f.checked_at) >= DAY || f.checked_at > now,
        Err(_) => true,
    }
}

/// Written only after a response parsed, so an offline launch retries.
pub fn mark_checked(data_dir: &Path, now: u64) {
    let body = serde_json::to_string(&CheckFile { checked_at: now }).unwrap_or_default();
    let tmp = data_dir.join(format!(".{FILE}.tmp"));
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, data_dir.join(FILE));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn due_when_missing_garbled_or_a_day_old() {
        let dir = std::env::temp_dir().join(format!("keel-update-test-check-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(check_due(&dir, 1_000_000));
        std::fs::write(dir.join(FILE), "{").unwrap();
        assert!(check_due(&dir, 1_000_000));
        mark_checked(&dir, 1_000_000);
        assert!(!check_due(&dir, 1_000_000 + DAY - 1));
        assert!(check_due(&dir, 1_000_000 + DAY));
        // A clock that went backwards checks again.
        assert!(check_due(&dir, 10));
        let _ = std::fs::remove_dir_all(dir);
    }
}

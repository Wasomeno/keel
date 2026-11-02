//! The session timer: a start / pause stopwatch that lives in the header or
//! snapped to a corner of the chat pane. Stored as wall-clock start + the
//! time banked by earlier runs (never a tick count), so it stays exact
//! through lag, sleep and restarts. `session-timer.json` in the data dir.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Where the pill rests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimerDock {
    /// Beside the header's navigation pill.
    #[default]
    Header,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl TimerDock {
    pub fn is_left(self) -> bool {
        matches!(self, TimerDock::TopLeft | TimerDock::BottomLeft)
    }
    pub fn is_bottom(self) -> bool {
        matches!(self, TimerDock::BottomLeft | TimerDock::BottomRight)
    }
    /// The corner of `area` (left, top, right, bottom) nearest `(x, y)`;
    /// above the area (the header) docks.
    pub fn for_drop(x: f32, y: f32, area: Option<(f32, f32, f32, f32)>) -> TimerDock {
        let Some((left, top, right, bottom)) = area else {
            return TimerDock::Header;
        };
        if y < top {
            return TimerDock::Header;
        }
        let left_half = x < (left + right) / 2.;
        let top_half = y < (top + bottom) / 2.;
        match (top_half, left_half) {
            (true, true) => TimerDock::TopLeft,
            (true, false) => TimerDock::TopRight,
            (false, true) => TimerDock::BottomLeft,
            (false, false) => TimerDock::BottomRight,
        }
    }
}

/// What the pill shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerPhase {
    Idle,
    Running,
    Paused,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTimer {
    /// Wall-clock ms the current run started; `None` while stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<i64>,
    /// Time banked by earlier runs, ms.
    #[serde(default)]
    pub accumulated_ms: i64,
    #[serde(default)]
    pub dock: TimerDock,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
}

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as i64
}

impl SessionTimer {
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("session-timer.json")
    }

    pub fn load(data_dir: &Path) -> Self {
        std::fs::read_to_string(Self::path(data_dir))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, data_dir: &Path) {
        let path = Self::path(data_dir);
        let Ok(text) = serde_json::to_string(self) else {
            return;
        };
        let _ = std::fs::create_dir_all(data_dir);
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, format!("{text}\n")).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    pub fn running(&self) -> bool {
        self.started_at_ms.is_some()
    }

    pub fn phase(&self) -> TimerPhase {
        if self.running() {
            TimerPhase::Running
        } else if self.accumulated_ms > 0 {
            TimerPhase::Paused
        } else {
            TimerPhase::Idle
        }
    }

    /// Elapsed ms at wall-clock `now`. A clock set backwards never counts down.
    pub fn elapsed_ms(&self, now: i64) -> i64 {
        self.accumulated_ms + self.started_at_ms.map_or(0, |start| (now - start).max(0))
    }

    pub fn start(&mut self, now: i64) {
        if self.started_at_ms.is_none() {
            self.started_at_ms = Some(now);
        }
    }

    pub fn pause(&mut self, now: i64) {
        if self.started_at_ms.is_some() {
            self.accumulated_ms = self.elapsed_ms(now);
            self.started_at_ms = None;
        }
    }

    pub fn toggle(&mut self, now: i64) {
        if self.running() { self.pause(now) } else { self.start(now) }
    }

    pub fn reset(&mut self) {
        self.started_at_ms = None;
        self.accumulated_ms = 0;
    }

    /// Ms until the shown seconds next change (always in 1..=1000).
    pub fn ms_to_next_second(&self, now: i64) -> u64 {
        (1000 - self.elapsed_ms(now).rem_euclid(1000)) as u64
    }
}

/// `05:09`, `59:59`, `1:02:34`. Whole seconds; no milliseconds.
pub fn clock_label(elapsed_ms: i64) -> String {
    let secs = elapsed_ms.max(0) / 1000;
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m:02}:{s:02}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_rolls_into_hours() {
        assert_eq!(clock_label(0), "00:00");
        assert_eq!(clock_label(5_999), "00:05");
        assert_eq!(clock_label(754_000), "12:34");
        assert_eq!(clock_label(3_754_000), "1:02:34");
    }

    #[test]
    fn start_pause_banks_time_and_reset_clears() {
        let mut t = SessionTimer::default();
        assert_eq!(t.phase(), TimerPhase::Idle);
        t.start(1_000);
        assert_eq!(t.phase(), TimerPhase::Running);
        assert_eq!(t.elapsed_ms(4_500), 3_500);
        t.pause(4_500);
        assert_eq!(t.phase(), TimerPhase::Paused);
        assert_eq!(t.elapsed_ms(99_000), 3_500);
        t.toggle(10_000);
        assert_eq!(t.elapsed_ms(11_000), 4_500);
        // Clock stepped backwards: no negative run.
        assert_eq!(t.elapsed_ms(9_000), 3_500);
        assert_eq!(t.ms_to_next_second(11_000), 500);
        t.reset();
        assert_eq!(t.phase(), TimerPhase::Idle);
        assert_eq!(t.elapsed_ms(50_000), 0);
    }

    #[test]
    fn drop_picks_header_or_nearest_corner() {
        let area = Some((0., 40., 800., 640.));
        assert_eq!(TimerDock::for_drop(300., 20., area), TimerDock::Header);
        assert_eq!(TimerDock::for_drop(100., 100., area), TimerDock::TopLeft);
        assert_eq!(TimerDock::for_drop(700., 100., area), TimerDock::TopRight);
        assert_eq!(TimerDock::for_drop(100., 600., area), TimerDock::BottomLeft);
        assert_eq!(TimerDock::for_drop(700., 600., area), TimerDock::BottomRight);
        assert_eq!(TimerDock::for_drop(700., 600., None), TimerDock::Header);
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("keel-timer-{}", uuid::Uuid::new_v4()));
        let t = SessionTimer { started_at_ms: Some(42), accumulated_ms: 7, dock: TimerDock::BottomLeft, hidden: true };
        t.save(&dir);
        assert_eq!(SessionTimer::load(&dir), t);
        let _ = std::fs::remove_dir_all(&dir);
        // Missing or unreadable: a fresh idle timer in the header.
        assert_eq!(SessionTimer::load(&dir), SessionTimer::default());
    }
}

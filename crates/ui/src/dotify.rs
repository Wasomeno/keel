//! Dotify in the UI, apart from gpui: the local UTC offset, what a clock-in
//! starts with, the small file that remembers projects and last-seen stats,
//! the popover's state machine and project list, search highlighting, the
//! two-click confirm, what the pill looks like, and every motion curve of
//! `docs/dotify-ui-spec.md` as a pure function of time (reduced-motion
//! aware). Wire types and sync rules are `keel_proto::dotify`; the HTTP is
//! the engine's.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use keel_proto::dotify::{
    DotifyError, Entry, Pending, Project, Summary, Tracker, local_day_start, local_weekday,
};
use serde::{Deserialize, Serialize};

pub use crate::approval::{Look, bezier, in_out, out_quint, span};

/// Seconds east of UTC right now, from the system time zone (WIB: 25 200).
pub fn local_offset_s() -> i64 {
    #[cfg(unix)]
    {
        // SAFETY: `time` and `localtime_r` only write into the locals given.
        unsafe {
            let now = libc::time(std::ptr::null_mut());
            let mut tm: libc::tm = std::mem::zeroed();
            if !libc::localtime_r(&now, &mut tm).is_null() {
                return tm.tm_gmtoff as i64;
            }
        }
    }
    0
}

/// Mon–Fri, 09:00–18:00 local: when an idle pill may show the reminder badge.
pub fn in_work_hours(now_ms: i64, offset_s: i64) -> bool {
    let mins = (now_ms - local_day_start(now_ms, offset_s)) / 60_000;
    local_weekday(now_ms, offset_s) <= 5 && (9 * 60..18 * 60).contains(&mins)
}

/// A clock-in description's starting text, and whether it came from the branch.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Prefill {
    pub text: String,
    pub from_branch: bool,
}

/// The chat's branch when it says something (not `main` / `master` /
/// `develop`), else the chat's title.
pub fn prefill_description(title: &str, branch: &str) -> Prefill {
    let branch = branch.trim();
    let generic = matches!(
        branch,
        "" | "main" | "master" | "develop" | "dev" | "HEAD" | "trunk"
    );
    if !generic {
        return Prefill {
            text: branch.to_string(),
            from_branch: true,
        };
    }
    let title = title.trim();
    let text = if title.is_empty() || title.eq_ignore_ascii_case("new chat") {
        String::new()
    } else {
        title.to_string()
    };
    Prefill {
        text,
        from_branch: false,
    }
}

// ── Memory ─────────────────────────────────────────────────────────────────

/// Stats as last shown, to animate the bars only when something moved.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct StatsSeen {
    /// Local midnight (epoch ms) of the day they were shown.
    pub day_ms: i64,
    pub today_h: f64,
    pub week_h: f64,
    pub period_h: f64,
}

/// `dotify.json` in the data dir: the last project per folder, the last
/// three used, the last reminder and the last-seen stats. Never the token.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Memory {
    #[serde(default)]
    pub last_project: BTreeMap<String, Project>,
    /// Used when the folder has none yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub any_project: Option<Project>,
    /// Newest first, at most [`RECENT`], no repeats.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent: Vec<Project>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_nudge_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats_seen: Option<StatsSeen>,
    /// Repo (`host/group/project`, else the folder) → the project last
    /// clocked in from it: learned on clock-in and on Switch.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub repo_project: BTreeMap<String, Project>,
    /// Repos "Don't ask" was picked for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repo_ignored: Vec<String>,
    /// When a switch was last suggested per repo (cooldown).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub switch_asked: BTreeMap<String, i64>,
}

pub const RECENT: usize = 3;
/// Minutes of agent work in one repo before a switch is suggested.
pub const SWITCH_AFTER_MS: i64 = 5 * 60_000;
/// How long "Not now" (or an ignored suggestion) keeps a repo quiet.
pub const SWITCH_COOLDOWN_MS: i64 = 2 * 3_600_000;

impl Memory {
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("dotify.json")
    }

    pub fn load(data_dir: &Path) -> Self {
        std::fs::read_to_string(Self::path(data_dir))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, data_dir: &Path) {
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::create_dir_all(data_dir);
            let path = Self::path(data_dir);
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }

    pub fn project_for(&self, folder: Option<&str>) -> Option<Project> {
        folder
            .and_then(|f| self.last_project.get(f).cloned())
            .or_else(|| self.any_project.clone())
    }

    /// A clock-in with `project` (from `folder`): last for the folder, and
    /// to the front of Recent.
    pub fn remember(&mut self, folder: Option<&str>, project: &Project) {
        if let Some(folder) = folder.filter(|f| !f.is_empty()) {
            self.last_project
                .insert(folder.to_string(), project.clone());
        }
        self.any_project = Some(project.clone());
        self.recent.retain(|p| p.id != project.id);
        self.recent.insert(0, project.clone());
        self.recent.truncate(RECENT);
    }

    /// Link `repo` to `project` (a clock-in or Switch from it).
    pub fn map_repo(&mut self, repo: Option<&str>, project: &Project) {
        if let Some(repo) = repo.filter(|r| !r.is_empty()) {
            self.repo_project.insert(repo.to_string(), project.clone());
            self.repo_ignored.retain(|r| r != repo);
        }
    }

    /// The project to suggest switching to: Keel has worked in `repo` for
    /// [`SWITCH_AFTER_MS`] while clocked in to another project, the repo
    /// is linked, not ignored, and not asked about within the cooldown.
    pub fn switch_suggestion(
        &self,
        tracker: &Tracker,
        repo: &str,
        work_since: i64,
        now: i64,
    ) -> Option<Project> {
        let Tracker::Running(entry) = tracker else {
            return None;
        };
        let target = self.repo_project.get(repo)?;
        if entry.project.as_ref().is_some_and(|p| p.id == target.id)
            || self.repo_ignored.iter().any(|r| r == repo)
        {
            return None;
        }
        if now - work_since < SWITCH_AFTER_MS
            || self
                .switch_asked
                .get(repo)
                .is_some_and(|at| now - at < SWITCH_COOLDOWN_MS)
        {
            return None;
        }
        Some(target.clone())
    }
}

/// A repo's mapping key: `host/group/project` from its `origin` remote,
/// else the folder itself.
pub fn repo_key(folder: &str, origin: &str) -> String {
    match crate::mr::parse_remote(origin) {
        Some((host, path)) => format!("{}/{}", host.trim_start_matches("https://"), path),
        None => folder.to_string(),
    }
}

/// A key as the suggestion names it: `group/project`, or the folder's name.
pub fn repo_label(key: &str) -> &str {
    if key.starts_with('/') {
        key.trim_end_matches('/').rsplit('/').next().unwrap_or(key)
    } else {
        key.split_once('/').map_or(key, |(_, path)| path)
    }
}

/// Whether the stats bars fill from zero on this open: the first open of
/// the day, or a value changed (to a tenth of an hour) since last shown.
pub fn stats_animate(seen: Option<&StatsSeen>, now: &StatsSeen) -> bool {
    let Some(seen) = seen else { return true };
    let moved = |a: f64, b: f64| ((a * 10.).round() - (b * 10.).round()).abs() >= 1.;
    seen.day_ms != now.day_ms
        || moved(seen.today_h, now.today_h)
        || moved(seen.week_h, now.week_h)
        || moved(seen.period_h, now.period_h)
}

/// One stats column: value, its target if Dotify gives one, and the bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stat {
    pub value_h: f64,
    pub target_h: Option<f64>,
}

impl Stat {
    /// Bar fill 0..=1 (None: no target, no bar).
    pub fn fill(&self) -> Option<f32> {
        self.target_h
            .filter(|t| *t > 0.)
            .map(|t| (self.value_h / t).clamp(0., 1.) as f32)
    }

    /// Hours over the target.
    pub fn over_h(&self) -> Option<f64> {
        self.target_h
            .filter(|t| *t > 0. && self.value_h > *t)
            .map(|t| self.value_h - t)
    }
}

/// Today (value only: Dotify has no daily target), Week, Period.
pub fn stats(today_h: Option<f64>, summary: Option<&Summary>) -> [Option<Stat>; 3] {
    [
        today_h.map(|v| Stat {
            value_h: v,
            target_h: None,
        }),
        summary.map(|s| Stat {
            value_h: s.week_h,
            target_h: Some(s.week_normal_h),
        }),
        summary.map(|s| Stat {
            value_h: s.period_h,
            target_h: Some(s.period_normal_h),
        }),
    ]
}

// ── Project list ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Favorites,
    Recent,
    All,
    Results,
}

impl Section {
    pub fn label(self) -> &'static str {
        match self {
            Section::Favorites => "FAVORITES",
            Section::Recent => "RECENT",
            Section::All => "ALL PROJECTS",
            Section::Results => "",
        }
    }
}

/// A row: its section and project.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub section: Section,
    pub project: Project,
}

/// Favorites, then Recent (not already a favorite), then everything else.
/// Search results are one flat list.
pub fn list_rows(
    favorites: &[Project],
    recent: &[Project],
    all: &[Project],
    results: Option<&[Project]>,
) -> Vec<Row> {
    if let Some(results) = results {
        return results
            .iter()
            .map(|p| Row {
                section: Section::Results,
                project: p.clone(),
            })
            .collect();
    }
    let mut rows: Vec<Row> = Vec::new();
    let push = |section: Section, p: &Project, rows: &mut Vec<Row>| {
        if !rows.iter().any(|r| r.project.id == p.id) {
            rows.push(Row {
                section,
                project: p.clone(),
            });
        }
    };
    for p in favorites {
        push(Section::Favorites, p, &mut rows);
    }
    for p in recent.iter().take(RECENT) {
        push(Section::Recent, p, &mut rows);
    }
    for p in all {
        push(Section::All, p, &mut rows);
    }
    rows
}

/// ↑ / ↓ through `len` rows, starting from nothing highlighted; clamps.
pub fn step_highlight(current: Option<usize>, len: usize, down: bool) -> Option<usize> {
    if len == 0 {
        return None;
    }
    Some(match (current, down) {
        (None, true) => 0,
        (None, false) => len - 1,
        (Some(i), true) => (i + 1).min(len - 1),
        (Some(i), false) => i.saturating_sub(1),
    })
}

/// Byte ranges of `name` that match `query`, case-insensitively: every
/// occurrence of the whole query, else its characters in order.
pub fn match_ranges(name: &str, query: &str) -> Vec<(usize, usize)> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }
    let lower = name.to_lowercase();
    let q = query.to_lowercase();
    // Lowercasing can change byte lengths (rare); only trust it when it doesn't.
    if lower.len() == name.len() {
        let mut out = Vec::new();
        let mut from = 0;
        while let Some(at) = lower[from..].find(&q) {
            out.push((from + at, from + at + q.len()));
            from += at + q.len();
        }
        if !out.is_empty() {
            return out;
        }
    }
    // Subsequence: each query char at its next match.
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut want = q.chars().filter(|c| !c.is_whitespace()).peekable();
    for (i, ch) in name.char_indices() {
        let Some(&w) = want.peek() else { break };
        if ch.to_lowercase().eq(w.to_lowercase()) {
            let end = i + ch.len_utf8();
            match out.last_mut() {
                Some(last) if last.1 == i => last.1 = end,
                _ => out.push((i, end)),
            }
            want.next();
        }
    }
    if want.peek().is_some() {
        Vec::new()
    } else {
        out
    }
}

// ── Two-click confirm ──────────────────────────────────────────────────────

pub const CONFIRM_MS: i64 = 4000;

/// First click arms for [`CONFIRM_MS`]; a second inside it confirms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Confirm {
    armed_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmStep {
    Armed,
    Confirmed,
}

impl Confirm {
    pub fn click(&mut self, now: i64) -> ConfirmStep {
        if self.armed(now) {
            self.armed_ms = None;
            ConfirmStep::Confirmed
        } else {
            self.armed_ms = Some(now);
            ConfirmStep::Armed
        }
    }

    pub fn armed(&self, now: i64) -> bool {
        self.armed_ms.is_some_and(|at| now - at < CONFIRM_MS)
    }

    /// The countdown line: 1 → 0 over the window (static 1 under reduced motion).
    pub fn remaining(&self, now: i64, reduced: bool) -> f32 {
        match self.armed_ms {
            Some(at) if now - at < CONFIRM_MS => {
                if reduced {
                    1.
                } else {
                    1. - (now - at) as f32 / CONFIRM_MS as f32
                }
            }
            _ => 0.,
        }
    }

    pub fn armed_at(&self) -> Option<i64> {
        self.armed_ms
    }

    pub fn cancel(&mut self) {
        self.armed_ms = None;
    }
}

// ── Popover views ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Idle,
    ClockedIn,
    /// Just clocked out: the result card.
    Result,
    /// Editing the entry just stopped.
    Edit,
}

/// What the popover shows. The result stays until the popover closes; the
/// next open shows idle (or clocked-in, when Dotify says so).
#[derive(Debug, Clone, PartialEq)]
pub struct Flow {
    pub view: View,
    pub result: Option<Entry>,
}

impl Default for Flow {
    fn default() -> Self {
        Flow {
            view: View::Idle,
            result: None,
        }
    }
}

impl Flow {
    fn base(tracker: &Tracker) -> View {
        if matches!(tracker, Tracker::Running(_)) {
            View::ClockedIn
        } else {
            View::Idle
        }
    }

    pub fn open(&mut self, tracker: &Tracker) {
        self.result = None;
        self.view = Self::base(tracker);
    }

    pub fn close(&mut self) {
        self.result = None;
        self.view = View::Idle;
    }

    /// Dotify changed underneath (sync, an action). Result and edit hold.
    pub fn tracker_changed(&mut self, tracker: &Tracker) {
        if matches!(self.view, View::Idle | View::ClockedIn) {
            self.view = Self::base(tracker);
        }
    }

    pub fn clocked_out(&mut self, entry: Entry) {
        self.result = Some(entry);
        self.view = View::Result;
    }

    /// Edit is only offered for an unlocked result.
    pub fn edit(&mut self) -> bool {
        if self
            .result
            .as_ref()
            .is_some_and(|e| !e.locked && e.end_ms.is_some())
        {
            self.view = View::Edit;
            true
        } else {
            false
        }
    }

    pub fn cancel_edit(&mut self) {
        if self.view == View::Edit {
            self.view = View::Result;
        }
    }

    pub fn saved(&mut self, entry: Entry) {
        self.result = Some(entry);
        self.view = View::Result;
    }

    pub fn done(&mut self, tracker: &Tracker) {
        self.result = None;
        self.view = Self::base(tracker);
    }
}

// ── The pill ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dot {
    Green,
    Amber,
    /// Offline: grey, no breathing.
    Grey,
    /// Token expired: red, no breathing.
    Red,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Badge {
    Amber,
    Grey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PillLook {
    /// No Dotify connection: "Connect Dotify" (opens Settings).
    Connect,
    /// Not clocked in: stopwatch + "Clock in", maybe a badge.
    Idle { badge: Option<Badge> },
    /// Not clocked in and the token expired: "Reconnect Dotify".
    Reconnect,
    /// Clocked in: dot, time (dimmed when stale), project.
    Running { dot: Dot, dim: bool, overtime: bool },
    /// A clock-in / clock-out in flight: spinner + label, clicks ignored.
    Pending { label: &'static str },
}

impl PillLook {
    pub fn breathes(&self) -> bool {
        matches!(
            self,
            PillLook::Running {
                dot: Dot::Green | Dot::Amber,
                ..
            }
        )
    }
}

pub fn pill_look(
    tracker: &Tracker,
    pending: Option<&Pending>,
    error: Option<&DotifyError>,
    overtime: bool,
    remind_badge: bool,
) -> PillLook {
    if let Some(p) = pending {
        return PillLook::Pending {
            label: if matches!(p, Pending::ClockIn { .. }) {
                "Clocking in…"
            } else {
                "Clocking out…"
            },
        };
    }
    let offline = matches!(
        error,
        Some(DotifyError::Offline(_) | DotifyError::Server(_))
    );
    let expired = matches!(error, Some(DotifyError::Auth(_)));
    match tracker {
        Tracker::Running(_) => PillLook::Running {
            dot: if expired {
                Dot::Red
            } else if offline {
                Dot::Grey
            } else if overtime {
                Dot::Amber
            } else {
                Dot::Green
            },
            dim: expired || offline,
            overtime: overtime && !expired && !offline,
        },
        _ if expired => PillLook::Reconnect,
        _ => PillLook::Idle {
            badge: if offline {
                Some(Badge::Grey)
            } else if remind_badge {
                Some(Badge::Amber)
            } else {
                None
            },
        },
    }
}

// ── Motion (spec §4) ───────────────────────────────────────────────────────

/// `cubic-bezier(0.4, 0, 1, 1)`.
pub fn ease_in(t: f32) -> f32 {
    bezier(0.4, 0., 1., 1., t)
}

/// A value easing from `from` to `to` over `ms` on `curve`, started at a
/// timestamp (ms, any clock). Retargeting mid-way starts from where it is.
#[derive(Debug, Clone, Copy)]
pub struct Tween {
    pub from: f32,
    pub to: f32,
    start: f64,
    ms: f32,
    curve: fn(f32) -> f32,
}

impl Tween {
    pub fn settled(v: f32) -> Self {
        Tween {
            from: v,
            to: v,
            start: 0.,
            ms: 0.,
            curve: out_quint,
        }
    }

    pub fn value(&self, now: f64) -> f32 {
        if self.ms <= 0. {
            return self.to;
        }
        let t = (((now - self.start) as f32) / self.ms).clamp(0., 1.);
        self.from + (self.to - self.from) * (self.curve)(t)
    }

    pub fn moving(&self, now: f64) -> bool {
        self.ms > 0. && ((now - self.start) as f32) < self.ms
    }

    /// Head for `to`; `animate: false` snaps. True when a move started.
    pub fn retarget(
        &mut self,
        to: f32,
        now: f64,
        ms: f32,
        curve: fn(f32) -> f32,
        animate: bool,
    ) -> bool {
        if (to - self.to).abs() < 0.5 {
            return false;
        }
        if !animate || ms <= 0. {
            *self = Tween::settled(to);
            return false;
        }
        *self = Tween {
            from: self.value(now),
            to,
            start: now,
            ms,
            curve,
        };
        true
    }
}

// One entry timeline: every piece of the popover reads its look from the
// same `ms` since open, so nothing runs on its own clock.

/// Each entry piece's duration.
pub const ENTER_MS: f32 = 180.;
/// The first section starts this long after open.
pub const SECTION_START_MS: f32 = 30.;
pub const SECTION_STAGGER_MS: f32 = 35.;
pub const ROW_STAGGER_MS: f32 = 18.;
/// Rows past this many share the last row's timing (they're below the fold).
pub const STAGGERED_ROWS: usize = 8;
/// The list's section index (its rows carry their own timing).
pub const LIST_SECTION: usize = 2;
/// Everything has landed by now (rows are the last).
pub const ENTRY_DONE_MS: f32 = SECTION_START_MS
    + SECTION_STAGGER_MS * LIST_SECTION as f32
    + ROW_STAGGER_MS * STAGGERED_ROWS as f32
    + ENTER_MS;

fn rise(ms: f32, delay: f32, len: f32, px: f32) -> Look {
    // Opacity and translate share one eased progress.
    let e = out_quint(span(ms, delay, len));
    Look {
        opacity: e,
        dy: px * (1. - e),
    }
}

/// The popover opening as every menu does (`view::menu_motion`): a fade
/// while it drifts 4 px out from the pill's side (`up`: it opens above),
/// over [`crate::motion::FAST`] eased out. Reduced motion cuts.
pub fn popover_enter(ms: f32, up: bool, reduced: bool) -> Look {
    if reduced {
        return Look {
            opacity: 1.,
            dy: 0.,
        };
    }
    let e = crate::motion::ease_out(span(ms, 0., POPOVER_EXIT_MS));
    Look {
        opacity: e,
        dy: if up { 4. } else { -4. } * (1. - e),
    }
}

pub const POPOVER_EXIT_MS: f32 = crate::motion::FAST.as_millis() as f32;

/// Closing: the entrance backwards, eased in, drifting back toward the pill.
/// Reduced motion cuts (the caller ends the close at once).
pub fn popover_exit(ms: f32, up: bool) -> Look {
    let e = crate::motion::ease_in(span(ms, 0., POPOVER_EXIT_MS));
    Look {
        opacity: 1. - e,
        dy: if up { 4. } else { -4. } * e,
    }
}

/// When section `i` starts (header, search, list, description, footer, stats).
pub fn section_delay(i: usize) -> f32 {
    SECTION_START_MS + SECTION_STAGGER_MS * i as f32
}

/// Section `i`: 180 ms, rising 6 px, 35 ms apart. Transparent from frame 1.
pub fn section_enter(ms: f32, i: usize, reduced: bool) -> Look {
    if reduced {
        return Look {
            opacity: span(ms, 0., 120.),
            dy: 0.,
        };
    }
    rise(ms, section_delay(i), ENTER_MS, 6.)
}

/// List item `i` (rows and section labels, in order): 180 ms, rising 4 px,
/// 18 ms apart from the list's slot; items past 8 move with the 8th.
pub fn row_enter(ms: f32, i: usize, reduced: bool) -> Look {
    if reduced {
        return Look {
            opacity: span(ms, 0., 120.),
            dy: 0.,
        };
    }
    let i = i.min(STAGGERED_ROWS);
    rise(
        ms,
        section_delay(LIST_SECTION) + ROW_STAGGER_MS * i as f32,
        ENTER_MS,
        4.,
    )
}

/// The new view after a switch: 200 ms from 40 ms, rising 4 px.
pub fn view_enter(ms: f32, reduced: bool) -> Look {
    if reduced {
        return Look {
            opacity: span(ms, 0., 120.),
            dy: 0.,
        };
    }
    rise(ms, 40., 200., 4.)
}

/// The header's "Synced HH:MM": the text on screen and a crossfade only
/// when it actually changes. Opening snaps it (no motion); a change while
/// the entry plays waits for it to finish.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SyncLabel {
    pub shown: String,
    pub prev: String,
    /// Crossfade start (ms, any clock).
    pub changed: Option<f64>,
}

pub const LABEL_FADE_MS: f32 = 160.;

impl SyncLabel {
    /// On open: show the current text at once, no crossfade.
    pub fn snap(&mut self, want: &str) {
        self.shown = want.to_string();
        self.prev.clear();
        self.changed = None;
    }

    /// A frame wants `want`; true when a crossfade starts.
    pub fn update(&mut self, want: &str, now: f64, entering: bool) -> bool {
        if want == self.shown || entering {
            return false;
        }
        self.prev = std::mem::replace(&mut self.shown, want.to_string());
        self.changed = Some(now);
        true
    }

    /// (old text, new text) opacity at `now`.
    pub fn opacities(&self, now: f64, reduced: bool) -> (f32, f32) {
        let Some(at) = self.changed else {
            return (0., 1.);
        };
        let ms = (now - at) as f32;
        let x = if reduced {
            span(ms, 0., 120.)
        } else {
            in_out(span(ms, 0., LABEL_FADE_MS))
        };
        (1. - x, x)
    }

    pub fn fading(&self, now: f64) -> bool {
        self.changed
            .is_some_and(|at| ((now - at) as f32) < LABEL_FADE_MS)
    }
}

/// A plain crossfade-in over `len` ms (capped to 120 under reduced motion).
pub fn fade(ms: f32, delay: f32, len: f32, reduced: bool) -> f32 {
    if reduced {
        span(ms, 0., len.min(120.))
    } else {
        out_quint(span(ms, delay, len))
    }
}

/// Stats bar `i` (Today, Week, Period): 0 → `to` over 500 ms OUT, 60 ms
/// apart from 200 ms after open — only when `animate`; else already full.
pub fn bar_fill(ms: f32, i: usize, to: f32, animate: bool, reduced: bool) -> f32 {
    if !animate || reduced {
        return to;
    }
    to * out_quint(span(ms, 200. + 60. * i as f32, 500.))
}

/// The spinner's angle (turns) at `ms` for a `period` ms turn.
pub fn spinner_turns(ms: f32, period: f32) -> f32 {
    (ms / period).rem_euclid(1.)
}

/// A pending wait shows its spinner only after 150 ms.
pub fn spinner_visible(wait_ms: f32) -> bool {
    wait_ms > 150.
}

/// Entering overtime: tint 0 → 1 over 400 ms INOUT, and a glow ring whose
/// alpha falls from 0.25 to 0 over 700 ms. No glow under reduced motion.
pub fn overtime_in(ms: f32, reduced: bool) -> (f32, f32) {
    if reduced {
        return (span(ms, 0., 120.), 0.);
    }
    (in_out(span(ms, 0., 400.)), 0.25 * (1. - span(ms, 0., 700.)))
}

/// Dot breathing at `ms` since it started: opacity .45 ↔ 1 over 1.8 s
/// INOUT, from 600 ms after the run began. 1 when not breathing.
pub fn breathe(ms: f32, reduced: bool) -> f32 {
    if reduced || ms < 600. {
        return 1.;
    }
    let phase = ((ms - 600.) / 1800.).rem_euclid(1.);
    let tri = if phase < 0.5 {
        phase * 2.
    } else {
        2. - phase * 2.
    };
    1. - 0.55 * in_out(tri)
}

pub const RECEIPT_HOLD_MS: f32 = 4000.;

/// The receipt pill at `ms` of its shown time (hover-paused time excluded):
/// in 180 OUT rising from -4, hold, out 140 IN. None once gone.
pub fn receipt_look(ms: f32, reduced: bool) -> Option<Look> {
    let enter = 180.;
    let out_at = enter + RECEIPT_HOLD_MS;
    if ms >= out_at + 140. {
        return None;
    }
    if reduced {
        let o = if ms < out_at {
            span(ms, 0., 120.)
        } else {
            1. - span(ms, out_at, 120.)
        };
        return Some(Look { opacity: o, dy: 0. });
    }
    if ms < out_at {
        let e = out_quint(span(ms, 0., enter));
        Some(Look {
            opacity: e,
            dy: -4. * (1. - e),
        })
    } else {
        Some(Look {
            opacity: 1. - ease_in(span(ms, out_at, 140.)),
            dy: 0.,
        })
    }
}

/// An error banner opening: (height fraction 180 OUT, opacity 140 from 40).
pub fn banner_open(ms: f32, reduced: bool) -> (f32, f32) {
    if reduced {
        return (1., span(ms, 0., 120.));
    }
    (
        out_quint(span(ms, 0., 180.)),
        out_quint(span(ms, 40., 140.)),
    )
}

/// A border flash on error: red alpha 0 → .30 in 100, back out over 500.
pub fn error_flash(ms: f32) -> f32 {
    if ms < 100. {
        0.3 * span(ms, 0., 100.)
    } else {
        0.3 * (1. - out_quint(span(ms, 100., 500.)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(id: i64, name: &str) -> Project {
        Project::new(id, name)
    }

    fn running() -> Tracker {
        Tracker::Running(Entry {
            id: 1,
            start_ms: 0,
            ..Default::default()
        })
    }

    #[test]
    fn description_prefers_a_real_branch() {
        assert_eq!(
            prefill_description("Fix login", "feat/login-refresh"),
            Prefill {
                text: "feat/login-refresh".into(),
                from_branch: true
            }
        );
        assert_eq!(
            prefill_description("Fix login", "main"),
            Prefill {
                text: "Fix login".into(),
                from_branch: false
            }
        );
        assert_eq!(prefill_description("New chat", "").text, "");
    }

    #[test]
    fn memory_remembers_per_folder_recent_three_and_round_trips() {
        let dir = std::env::temp_dir().join(format!("keel-dotify-mem-{}", uuid::Uuid::new_v4()));
        let mut m = Memory::default();
        assert_eq!(m.project_for(Some("/a")), None);
        m.remember(Some("/a"), &p(1, "A"));
        m.remember(Some("/b"), &p(2, "B"));
        m.remember(Some("/a"), &p(3, "C"));
        m.remember(None, &p(2, "B"));
        m.remember(None, &p(4, "D"));
        assert_eq!(
            m.recent.iter().map(|p| p.id).collect::<Vec<_>>(),
            vec![4, 2, 3],
            "newest first, no repeats, three"
        );
        assert_eq!(m.project_for(Some("/a")), Some(p(3, "C")));
        assert_eq!(m.project_for(Some("/zz")), Some(p(4, "D")));
        m.stats_seen = Some(StatsSeen {
            day_ms: 5,
            today_h: 1.,
            week_h: 2.,
            period_h: 3.,
        });
        m.save(&dir);
        assert_eq!(Memory::load(&dir), m);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn switch_is_suggested_only_for_a_linked_repo_worked_in_long_enough() {
        let repo = repo_key("/w/resona", "git@gitlab.dot.co.id:resona/web-revamp.git");
        assert_eq!(repo, "gitlab.dot.co.id/resona/web-revamp");
        assert_eq!(repo_key("/w/scratch", ""), "/w/scratch");
        assert_eq!(repo_label(&repo), "resona/web-revamp");
        assert_eq!(repo_label("/w/scratch/"), "scratch");

        let running = |id| {
            Tracker::Running(Entry {
                project: Some(p(id, "x")),
                start_ms: 0,
                ..Default::default()
            })
        };
        let min = 60_000;
        let mut m = Memory::default();
        let now = 10 * 3_600_000;
        assert_eq!(
            m.switch_suggestion(&running(222), &repo, now - 10 * min, now),
            None,
            "unlinked repo: no guess"
        );
        m.map_repo(Some(&repo), &p(178, "Resona - Web Revamp"));
        assert_eq!(
            m.switch_suggestion(&running(222), &repo, now - 10 * min, now),
            Some(p(178, "Resona - Web Revamp"))
        );
        assert_eq!(
            m.switch_suggestion(&running(178), &repo, now - 10 * min, now),
            None,
            "already on it"
        );
        assert_eq!(
            m.switch_suggestion(&Tracker::Idle, &repo, now - 10 * min, now),
            None,
            "idle is the clock-in nudge's job"
        );
        assert_eq!(
            m.switch_suggestion(&running(222), &repo, now - 4 * min, now),
            None,
            "under five minutes"
        );
        m.switch_asked.insert(repo.clone(), now - 30 * min);
        assert_eq!(
            m.switch_suggestion(&running(222), &repo, now - 10 * min, now),
            None,
            "cooling down"
        );
        assert!(
            m.switch_suggestion(&running(222), &repo, now - 10 * min, now + 2 * 3_600_000)
                .is_some(),
            "asks again later"
        );
        m.switch_asked.clear();
        m.repo_ignored.push(repo.clone());
        assert_eq!(
            m.switch_suggestion(&running(222), &repo, now - 10 * min, now),
            None,
            "don't ask"
        );
        m.map_repo(Some(&repo), &p(203, "Resona Maintenance"));
        assert!(
            m.repo_ignored.is_empty(),
            "a new clock-in from the repo re-enables it"
        );
        assert_eq!(m.repo_project[&repo].id, 203, "the latest clock-in wins");

        let dir = std::env::temp_dir().join(format!("keel-dotify-map-{}", uuid::Uuid::new_v4()));
        m.save(&dir);
        assert_eq!(Memory::load(&dir), m);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_sections_favorites_recent_all_without_repeats() {
        let rows = list_rows(
            &[p(1, "A")],
            &[p(1, "A"), p(2, "B")],
            &[p(1, "A"), p(2, "B"), p(3, "C")],
            None,
        );
        let got: Vec<(Section, i64)> = rows.iter().map(|r| (r.section, r.project.id)).collect();
        assert_eq!(
            got,
            vec![
                (Section::Favorites, 1),
                (Section::Recent, 2),
                (Section::All, 3)
            ]
        );
        let flat = list_rows(&[p(1, "A")], &[], &[], Some(&[p(9, "Z")]));
        assert_eq!(flat[0].section, Section::Results);
        assert_eq!(step_highlight(None, 3, true), Some(0));
        assert_eq!(step_highlight(Some(2), 3, true), Some(2));
        assert_eq!(step_highlight(Some(0), 3, false), Some(0));
        assert_eq!(step_highlight(None, 0, true), None);
    }

    #[test]
    fn search_highlight_ranges() {
        assert_eq!(match_ranges("Boilerplate - BE", "be"), vec![(14, 16)]);
        assert_eq!(match_ranges("Dotify Dot", "dot"), vec![(0, 3), (7, 10)]);
        // Not a substring: characters in order.
        assert_eq!(
            match_ranges("DLTX Solutions", "dxs"),
            vec![(0, 1), (3, 4), (5, 6)]
        );
        assert_eq!(match_ranges("Keel", "zz"), Vec::<(usize, usize)>::new());
        assert_eq!(match_ranges("Keel", "  "), Vec::<(usize, usize)>::new());
    }

    #[test]
    fn confirm_arms_counts_down_and_expires() {
        let mut c = Confirm::default();
        assert_eq!(c.click(1000), ConfirmStep::Armed);
        assert!(c.armed(1000));
        assert!((c.remaining(3000, false) - 0.5).abs() < 1e-3);
        assert_eq!(c.remaining(3000, true), 1., "reduced motion: static line");
        assert_eq!(c.click(4999), ConfirmStep::Confirmed);
        assert!(!c.armed(5000));
        // Timeout: the next click arms again.
        assert_eq!(c.click(10_000), ConfirmStep::Armed);
        assert_eq!(c.remaining(14_000, false), 0.);
        assert_eq!(c.click(14_000), ConfirmStep::Armed);
        c.cancel();
        assert!(!c.armed(14_001));
    }

    #[test]
    fn flow_result_stays_until_close_and_next_open_is_fresh() {
        let mut f = Flow::default();
        f.open(&running());
        assert_eq!(f.view, View::ClockedIn);
        let stopped = Entry {
            id: 7,
            start_ms: 0,
            end_ms: Some(10),
            ..Default::default()
        };
        f.clocked_out(stopped.clone());
        assert_eq!(f.view, View::Result);
        f.tracker_changed(&Tracker::Idle);
        assert_eq!(f.view, View::Result, "a sync doesn't take the result away");
        assert!(f.edit());
        assert_eq!(f.view, View::Edit);
        f.cancel_edit();
        assert_eq!(f.view, View::Result);
        f.close();
        f.open(&Tracker::Idle);
        assert_eq!((f.view, f.result.is_none()), (View::Idle, true));
        // Locked entries can't be edited.
        f.clocked_out(Entry {
            locked: true,
            end_ms: Some(1),
            ..stopped
        });
        assert!(!f.edit());
        f.done(&Tracker::Idle);
        assert_eq!(f.view, View::Idle);
    }

    #[test]
    fn pill_states() {
        let off = DotifyError::Offline("x".into());
        let auth = DotifyError::Auth("x".into());
        assert_eq!(
            pill_look(&Tracker::Idle, None, None, false, false),
            PillLook::Idle { badge: None }
        );
        assert_eq!(
            pill_look(&Tracker::Idle, None, None, false, true),
            PillLook::Idle {
                badge: Some(Badge::Amber)
            }
        );
        assert_eq!(
            pill_look(&Tracker::Idle, None, Some(&off), false, true),
            PillLook::Idle {
                badge: Some(Badge::Grey)
            }
        );
        assert_eq!(
            pill_look(&Tracker::Idle, None, Some(&auth), false, false),
            PillLook::Reconnect
        );
        assert_eq!(
            pill_look(&running(), None, None, true, false),
            PillLook::Running {
                dot: Dot::Amber,
                dim: false,
                overtime: true
            }
        );
        assert_eq!(
            pill_look(&running(), None, Some(&off), true, false),
            PillLook::Running {
                dot: Dot::Grey,
                dim: true,
                overtime: false
            }
        );
        assert_eq!(
            pill_look(&running(), None, Some(&auth), false, false),
            PillLook::Running {
                dot: Dot::Red,
                dim: true,
                overtime: false
            }
        );
        assert_eq!(
            pill_look(&running(), Some(&Pending::ClockOut), None, false, false),
            PillLook::Pending {
                label: "Clocking out…"
            }
        );
        assert!(pill_look(&running(), None, None, false, false).breathes());
        assert!(!pill_look(&running(), None, Some(&off), false, false).breathes());
    }

    #[test]
    fn stats_animate_only_on_a_new_day_or_a_change() {
        let seen = StatsSeen {
            day_ms: 100,
            today_h: 2.0,
            week_h: 30.0,
            period_h: 100.0,
        };
        assert!(stats_animate(None, &seen));
        assert!(
            !stats_animate(
                Some(&seen),
                &StatsSeen {
                    today_h: 2.04,
                    ..seen
                }
            ),
            "sub-tenth drift doesn't count"
        );
        assert!(stats_animate(
            Some(&seen),
            &StatsSeen {
                week_h: 30.5,
                ..seen
            }
        ));
        assert!(stats_animate(
            Some(&seen),
            &StatsSeen {
                day_ms: 200,
                ..seen
            }
        ));
        // A bar starts empty only when animating.
        assert_eq!(bar_fill(0., 0, 0.8, true, false), 0.);
        assert_eq!(bar_fill(0., 0, 0.8, false, false), 0.8);
        assert!((bar_fill(5000., 2, 0.8, true, false) - 0.8).abs() < 1e-4);
        let s = stats(
            Some(3.),
            Some(&Summary {
                week_h: 84.,
                week_normal_h: 40.,
                period_h: 20.,
                period_normal_h: 160.,
            }),
        );
        assert_eq!(
            s[0].unwrap().fill(),
            None,
            "Today has no target: value only"
        );
        assert_eq!(s[1].unwrap().fill(), Some(1.));
        assert_eq!(s[1].unwrap().over_h(), Some(44.));
        assert_eq!(s[2].unwrap().over_h(), None);
    }

    #[test]
    fn reduced_motion_lands_at_once() {
        for ms in [0., 10., 50.] {
            assert_eq!(section_enter(ms, 3, true).dy, 0.);
            assert_eq!(row_enter(ms, 2, true).dy, 0.);
            assert!(section_enter(ms, 3, true).opacity == span(ms, 0., 120.));
            assert_eq!(
                popover_enter(ms, false, true),
                Look {
                    opacity: 1.,
                    dy: 0.
                }
            );
            assert_eq!(view_enter(ms, true).dy, 0.);
            assert_eq!(breathe(ms + 5000., true), 1.);
            assert_eq!(bar_fill(ms, 1, 0.4, true, true), 0.4);
            assert_eq!(overtime_in(ms, true).1, 0., "no glow");
            assert_eq!(banner_open(ms, true).0, 1., "no height tween");
        }
        // Reduced fades never run past 120 ms.
        assert_eq!(fade(120., 0., 400., true), 1.);
        assert_eq!(receipt_look(10., true).unwrap().dy, 0.);
    }

    #[test]
    fn full_motion_curves_start_and_land() {
        // Same as the menus: 4 px from the pill's side, flipped when opening above.
        assert_eq!(
            popover_enter(0., false, false),
            Look {
                opacity: 0.,
                dy: -4.
            }
        );
        assert_eq!(
            popover_enter(0., true, false),
            Look {
                opacity: 0.,
                dy: 4.
            }
        );
        assert_eq!(
            popover_enter(POPOVER_EXIT_MS, false, false),
            Look {
                opacity: 1.,
                dy: 0.
            }
        );
        assert_eq!(
            popover_exit(POPOVER_EXIT_MS, false),
            Look {
                opacity: 0.,
                dy: -4.
            }
        );
        assert_eq!(
            popover_exit(POPOVER_EXIT_MS, true),
            Look {
                opacity: 0.,
                dy: 4.
            }
        );
        assert_eq!(
            section_enter(0., 2, false),
            Look {
                opacity: 0.,
                dy: 6.
            }
        );
        assert_eq!(
            section_enter(section_delay(2) + ENTER_MS, 2, false).opacity,
            1.
        );
        // Nothing pops in at full opacity before its slot, rows past 8 included.
        assert_eq!(
            row_enter(section_delay(LIST_SECTION) - 1., 20, false).opacity,
            0.
        );
        assert_eq!(
            row_enter(ENTRY_DONE_MS, 20, false),
            Look {
                opacity: 1.,
                dy: 0.
            }
        );
        assert_eq!(row_enter(0., 0, false).dy, 4.);
        // One eased progress drives both channels.
        let l = section_enter(section_delay(1) + 60., 1, false);
        assert!((l.dy - 6. * (1. - l.opacity)).abs() < 1e-5);
        assert_eq!(breathe(100., false), 1., "breathing waits 600 ms");
        let b = breathe(600. + 900., false);
        assert!((b - 0.45).abs() < 1e-3, "trough at half a cycle: {b}");
        assert!(receipt_look(180. + RECEIPT_HOLD_MS + 141., false).is_none());
        assert!(spinner_visible(151.) && !spinner_visible(149.));
        let (tint, glow) = overtime_in(700., false);
        assert_eq!((tint, glow), (1., 0.));
    }

    #[test]
    fn sync_label_crossfades_only_on_real_changes() {
        let mut l = SyncLabel::default();
        l.snap("Synced 21:13");
        assert_eq!(l.opacities(0., false), (0., 1.), "open: no motion");
        // The open-triggered sync lands mid-entry: deferred.
        assert!(!l.update("Synced 21:14", 100., true));
        assert_eq!(l.shown, "Synced 21:13");
        // Same text after entry: nothing.
        assert!(!l.update("Synced 21:13", 500., false));
        // A new minute after entry: one crossfade.
        assert!(l.update("Synced 21:14", 500., false));
        assert!(!l.update("Synced 21:14", 520., false), "no restart");
        assert_eq!(l.opacities(500. + LABEL_FADE_MS as f64, false), (0., 1.));
        assert_eq!(l.prev, "Synced 21:13");
        assert!(!l.fading(500. + LABEL_FADE_MS as f64));
    }

    #[test]
    fn tween_retargets_from_where_it_is() {
        let mut t = Tween::settled(60.);
        assert!(!t.retarget(60.2, 0., 240., in_out, true));
        assert!(t.retarget(160., 0., 240., in_out, true));
        let mid = t.value(120.);
        assert!(mid > 60. && mid < 160.);
        assert!(t.retarget(100., 120., 160., out_quint, true));
        assert_eq!(t.value(120.), mid);
        assert_eq!(t.value(400.), 100.);
        assert!(!t.retarget(40., 400., 160., out_quint, false));
        assert_eq!(t.value(400.), 40.);
    }

    #[test]
    fn work_hours_and_offset() {
        let wib = 7 * 3600;
        // 2026-10-09 (Fri) 03:00Z = 10:00 WIB.
        let fri_10 = keel_proto::dotify::parse_utc_ms("2026-10-09T03:00:00Z").unwrap();
        assert!(in_work_hours(fri_10, wib));
        assert!(!in_work_hours(fri_10 + 9 * 3_600_000, wib), "19:00");
        assert!(!in_work_hours(fri_10 + 24 * 3_600_000, wib), "Saturday");
        let o = local_offset_s();
        assert!((-14 * 3600..=14 * 3600).contains(&o));
    }
}

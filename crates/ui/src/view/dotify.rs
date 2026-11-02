//! Dotify (clock-in / clock-out at work) in the timer pill: state, sync,
//! actions, keys and dev hooks. Rendering is `dotify_ui.rs`; the look and
//! motion follow `docs/dotify-ui-spec.md`, with every curve a pure function
//! of time in `crate::dotify`. The engine holds the token and does the HTTP.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::cell::Cell;
use std::time::{Duration, Instant};

use gpui::{Context, KeyDownEvent, Task, Window};
use keel_proto::dotify::{DotifyError, Entry, Pending, Project, Status, Summary, SyncChange, Tracker, hours_on, local_day_start, reconcile, should_nudge};

use super::KeelWindow;
use crate::dotify::{Confirm, ConfirmStep, Flow, Memory, Prefill, Row, StatsSeen, Tween, View, list_rows, local_offset_s, prefill_description, repo_key, repo_label, step_highlight};
use crate::timer::now_ms;

/// Background sync while connected.
const SYNC_EVERY: Duration = Duration::from_secs(60);
/// Coming back to the window syncs if the last one is older than this.
const FOCUS_SYNC_AFTER: Duration = Duration::from_secs(15);
/// Summary and today's entries refresh this often.
const TOTALS_EVERY: Duration = Duration::from_secs(300);
/// Typing in the project search waits this long before asking.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(250);
/// A primary button's check holds this long before the next state.
const SUCCESS_HOLD: Duration = Duration::from_millis(540);
/// Manual sync spins at least this long.
pub(super) const SPIN_MIN_MS: f32 = 400.;

/// Who we're connected as (from `list_connections`).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct DotifyConn {
    pub base: String,
    pub name: String,
    pub email: String,
}

/// A primary button (Clock in, Save, Test & connect): idle, loading,
/// success (check), or error (border flash). Width is held while busy.
#[derive(Clone, Default)]
pub(super) struct Primary {
    pub phase: PrimaryPhase,
    pub width: Rc<Cell<f32>>,
    /// Enabled-ness and when it last flipped (colors blend over 160 ms).
    pub enabled: Option<(bool, Instant)>,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) enum PrimaryPhase {
    #[default]
    Idle,
    Loading(Instant),
    Success(Instant),
    Error(Instant),
}

impl Primary {
    pub fn busy(&self) -> bool {
        matches!(self.phase, PrimaryPhase::Loading(_) | PrimaryPhase::Success(_))
    }
}

/// "Working in this repo: switch to its project?" under the header pill.
#[derive(Clone)]
pub(super) struct SwitchOffer {
    pub repo: String,
    pub folder: String,
    pub project: Project,
    pub description: String,
    pub busy: Option<Instant>,
}

/// The receipt pill under the header pill after a clock-out from the menu.
#[derive(Clone)]
pub(super) struct Receipt {
    pub text: String,
    pub entry: Entry,
    pub shown: Instant,
    pub paused: Duration,
    pub hover_since: Option<Instant>,
}

impl Receipt {
    /// Shown time, hovered time excluded.
    pub fn ms(&self) -> f32 {
        let hover = self.hover_since.map_or(Duration::ZERO, |h| h.elapsed());
        (self.shown.elapsed().saturating_sub(self.paused + hover)).as_secs_f32() * 1000.
    }
}

/// An animated width: the natural width measured as it paints, eased to.
#[derive(Clone, Default)]
pub(super) struct AnimW {
    pub tween: Option<Tween>,
    pub measured: Rc<Cell<f32>>,
    /// What the content is (look, labels). Keyed widths only follow the
    /// measurement freely right after this changes; see [`size_follows`].
    pub content_key: Option<u64>,
    /// When `content_key` last changed (`clock()` ms).
    pub key_at: f64,
}

/// Layout snaps edges to device pixels, so a box moving by fractions (the
/// header gliding with the sidebar) measures ±0.5 px wider or narrower
/// from frame to frame. Under this, a measured change is that snap, not
/// the content: it must not start a width tween.
pub(super) const SNAP_JITTER: f32 = 1.0;
/// How long after a content change the width follows every measurement
/// (the new content is measured a frame or two late).
pub(super) const KEY_SETTLE_MS: f64 = 500.;

/// Whether a measured size `delta` px off the tween's target should move it.
/// Unkeyed sizes keep the old half-pixel rule. A keyed one follows small
/// changes only while its content just changed; otherwise only a change
/// bigger than the pixel snap counts.
pub(super) fn size_follows(delta: f32, keyed: bool, since_key_ms: f64) -> bool {
    let delta = delta.abs();
    if !keyed || since_key_ms < KEY_SETTLE_MS {
        return delta >= 0.5;
    }
    delta > SNAP_JITTER
}

#[derive(Default)]
pub(super) struct DotifyUi {
    pub conn: Option<DotifyConn>,
    pub tracker: Tracker,
    pub overtime: bool,
    /// When overtime began (as seen here): tint and glow; the wall ms for "+0:24".
    pub overtime_at: Option<Instant>,
    pub overtime_wall_ms: Option<i64>,
    pub pending: Option<Pending>,
    pub pending_at: Option<Instant>,
    pub summary: Option<Summary>,
    pub today_h: Option<f64>,
    pub today_entries: Vec<Entry>,
    /// The last sync's failure (auth / offline / server), and since when.
    pub sync_error: Option<DotifyError>,
    pub error_at: Option<Instant>,
    /// Dotify refused an action (a 400): quoted exactly in a red banner.
    pub rejected: Option<String>,
    pub rejected_at: Option<Instant>,
    pub synced_at: Option<Instant>,
    pub synced_wall_ms: Option<i64>,
    /// The header's "Synced HH:MM" as shown (crossfades only on change).
    pub sync_label: crate::dotify::SyncLabel,
    totals_at: Option<Instant>,
    pub syncing: bool,
    /// A sync asked for by hand: the spinner (refresh icon, pill dot).
    pub manual_spin: Option<Instant>,
    pub manual_done: Option<Instant>,
    // ── Popover ──
    pub open: bool,
    pub open_at: Option<Instant>,
    pub close_at: Option<Instant>,
    /// Outside clicks close it; the same click landing on the pill mustn't reopen.
    closed_by_outside: Option<Instant>,
    pub flow: Flow,
    pub view_at: Option<Instant>,
    /// Hold the view (a success check showing) until then.
    hold_view_until: Option<Instant>,
    pub prefill: bool,
    pub prefill_text: Prefill,
    pub focus_search: bool,
    pub favorites: Vec<Project>,
    /// Favorites as of this open: starring doesn't move rows until next open.
    pub fav_snapshot: Vec<Project>,
    pub fav_ids: HashSet<i64>,
    pub star_flip: HashMap<i64, Instant>,
    pub all_projects: Vec<Project>,
    pub projects_loaded: bool,
    pub results: Option<Vec<Project>>,
    pub results_at: Option<Instant>,
    pub searching: bool,
    pub query: String,
    search_gen: u64,
    search_task: Option<Task<()>>,
    pub picked: Option<Project>,
    pub picked_at: Option<Instant>,
    pub highlight: Option<usize>,
    pub confirm_out: Confirm,
    pub confirm_out_w: AnimW,
    /// The right-click menu's Clock out (two clicks, too).
    pub confirm_menu: Confirm,
    pub clock_in_btn: Primary,
    pub save_btn: Primary,
    pub out_busy: Option<Instant>,
    pub receipt: Option<Receipt>,
    /// Stats bars: (since, fill from zero?) for this open.
    pub stats_anim: Option<(Instant, bool)>,
    pub bar_tweens: [Option<Tween>; 3],
    pub height: AnimW,
    // ── Pill ──
    pub pill_key: u8,
    pub pill_key_from: u8,
    pub pill_look_at: Option<Instant>,
    pub pill_w: AnimW,
    pub pill_project: String,
    pub pill_project_at: Option<Instant>,
    pub open_toggled_at: Option<Instant>,
    /// A frame is wanted (a timestamp-driven move still running).
    pub want_frame: bool,
    tick: Option<Task<()>>,
    pub memory: Memory,
    memory_loaded: bool,
    busy_since: Option<i64>,
    loop_task: Option<Task<()>>,
    /// Screenshots: a UI-only state; never sent anywhere.
    pub demo: bool,
    pub demo_settings_disconnected: bool,
    /// Projects arrived after the popover opened: rows enter from here.
    pub rows_at: Option<Instant>,
    /// Rows and highlight ranges, rebuilt only when their inputs change.
    pub rows_cache: std::cell::RefCell<Option<RowsCache>>,
    pub list_scroll: gpui::ScrollHandle,
    pub scrolled_highlight: Option<usize>,
    /// The entry timeline starts on the first painted frame (opacity 0 there).
    pub open_fresh: bool,
    /// `KEEL_DOTIFY_FRAMELOG=1`: frame times during the popover's entry.
    pub bench: Bench,
    /// The Settings page's scroll (the demo scrolls to Connections).
    pub settings_scroll: gpui::ScrollHandle,
    // ── Settings → Connections → Dotify ──
    pub connect_btn: Primary,
    pub connect_error: Option<String>,
    pub connect_error_at: Option<Instant>,
    pub token_shown: bool,
    pub test_busy: Option<Instant>,
    pub test_result: Option<(Result<String, String>, Instant)>,
    pub connected_at: Option<Instant>,
    pub disconnect_confirm: Confirm,
    pub nudge_flip_at: Option<Instant>,
    // ── Repo → project switch ──
    /// Folder → repo key (`git remote get-url origin`, once per folder).
    pub repo_keys: HashMap<String, String>,
    /// The repo Keel is working in, and since when (epoch ms).
    pub work: Option<(String, i64)>,
    pub switch: Option<SwitchOffer>,
    /// The offer last drawn, so it can fade out after it's answered.
    pub switch_shown: Option<SwitchOffer>,
}

pub(super) struct RowsCache {
    key: u64,
    pub rows: Rc<Vec<Row>>,
    /// Highlight ranges per row (empty without a query).
    pub ranges: Rc<Vec<Vec<(usize, usize)>>>,
}

/// Frame log for one popover open (env-gated).
#[derive(Default)]
pub(super) struct Bench {
    pub on: bool,
    pub last: Option<Instant>,
    /// (ms since open, frame gap ms, build ms)
    pub frames: Vec<(f32, f32, f32)>,
    pub label_fades: u32,
    pub reported: bool,
}

pub(super) fn ms_since(at: Option<Instant>) -> f32 {
    at.map_or(1e9, |a| a.elapsed().as_secs_f32() * 1000.)
}

/// Milliseconds on one process-wide clock, for [`Tween`].
pub(super) fn clock() -> f64 {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.
}

impl KeelWindow {
    pub(super) fn dotify_mode(&self) -> bool {
        self.dotify.conn.is_some()
    }

    /// Whether Dotify is visible in the UI.
    /// By default Dotify is hidden; it can be enabled by setting KEEL_DOTIFY=1
    /// or if there is already an active Dotify connection.
    pub(super) fn dotify_visible(&self) -> bool {
        if std::env::var("KEEL_DOTIFY").map_or(false, |v| v == "0" || v == "false") {
            return false;
        }
        self.dotify_mode() || std::env::var("KEEL_DOTIFY").map_or(false, |v| v == "1" || v == "true")
    }

    /// Stats as the popover shows them now (for the bars' gating).
    pub(super) fn dotify_stats_seen(&self) -> Option<StatsSeen> {
        let s = self.dotify.summary?;
        Some(StatsSeen {
            day_ms: local_day_start(now_ms(), local_offset_s()),
            today_h: self.dotify_today_h().unwrap_or(0.),
            week_h: s.week_h,
            period_h: s.period_h,
        })
    }

    // ── Connection and sync ─────────────────────────────────────────────

    /// Read the connection list; connected → sync now and every minute.
    pub(super) fn dotify_boot(&mut self, cx: &mut Context<Self>) {
        if !self.dotify_visible() && !self.dotify_mode() {
            return;
        }
        if !self.dotify.memory_loaded {
            self.dotify.memory = Memory::load(&self.state.data_dir);
            self.dotify.memory_loaded = true;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let Ok(v) = client.call("list_connections", serde_json::json!({})).await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                let conn = v["connections"].as_array().into_iter().flatten().find(|c| c["kind"] == "dotify").map(|c| DotifyConn {
                    base: c["host"].as_str().unwrap_or_default().to_string(),
                    name: c["name"].as_str().unwrap_or_default().to_string(),
                    email: c["username"].as_str().unwrap_or_default().to_string(),
                });
                this.dotify_set_conn(conn, cx);
            });
        })
        .detach();
    }

    fn dotify_set_conn(&mut self, conn: Option<DotifyConn>, cx: &mut Context<Self>) {
        let changed = self.dotify.conn != conn;
        self.dotify.conn = conn;
        if self.dotify.conn.is_none() {
            let memory = std::mem::take(&mut self.dotify.memory);
            let demo_settings_disconnected = self.dotify.demo_settings_disconnected;
            self.dotify = DotifyUi { memory, memory_loaded: true, demo_settings_disconnected, ..Default::default() };
            self.ensure_timer_tick(cx);
            cx.notify();
            return;
        }
        if changed {
            self.dotify.tracker = Tracker::Unknown;
            self.dotify.favorites.clear();
            self.dotify.all_projects.clear();
            self.dotify.projects_loaded = false;
            self.dotify.summary = None;
            self.dotify.totals_at = None;
        }
        self.dotify_sync(cx);
        if self.dotify.loop_task.is_none() {
            self.dotify.loop_task = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(SYNC_EVERY).await;
                    let alive = this.update(cx, |this, cx| {
                        if this.dotify_mode() {
                            this.dotify_sync(cx);
                            this.dotify_nudge_check();
                            this.dotify_switch_check(cx);
                        }
                    });
                    if alive.is_err() {
                        break;
                    }
                }
            }));
        }
        self.ensure_timer_tick(cx);
        self.dotify_ensure_tick(cx);
        cx.notify();
    }

    /// The window came forward (sync unless it just did) or went back.
    pub(super) fn dotify_on_activation(&mut self, cx: &mut Context<Self>) {
        if !self.dotify_visible() {
            return;
        }
        if self.window_active && self.dotify_mode() && self.dotify.synced_at.is_none_or(|at| at.elapsed() > FOCUS_SYNC_AFTER) {
            self.dotify_sync(cx);
        }
        self.dotify_ensure_tick(cx);
        cx.notify();
    }

    /// Repaints while clocked in and the window is up: on the second, or
    /// ~25 fps while the dot breathes. Nothing while hidden or unfocused;
    /// the time snaps to now on return.
    pub(super) fn dotify_ensure_tick(&mut self, cx: &mut Context<Self>) {
        if !self.dotify_visible() || !self.dotify_tick_wanted() {
            self.dotify.tick = None;
            return;
        }
        if self.dotify.tick.is_some() {
            return;
        }
        self.dotify.tick = Some(cx.spawn(async move |this, cx| {
            loop {
                let wait = this.update(cx, |this, _| {
                    if !this.dotify_tick_wanted() {
                        this.dotify.tick = None;
                        return None;
                    }
                    let breathing = !this.reduced()
                        && crate::dotify::pill_look(&this.dotify.tracker, this.dotify.pending.as_ref(), this.dotify.sync_error.as_ref(), this.dotify.overtime, false)
                            .breathes();
                    Some(if breathing {
                        40
                    } else {
                        let start = match &this.dotify.tracker {
                            Tracker::Running(e) => e.start_ms,
                            _ => 0,
                        };
                        (1000 - (now_ms() - start).rem_euclid(1000)) as u64 + 5
                    })
                });
                let Ok(Some(wait)) = wait else {
                    break;
                };
                cx.background_executor().timer(Duration::from_millis(wait)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        }));
    }

    fn dotify_tick_wanted(&self) -> bool {
        self.dotify_mode() && !self.timer.hidden && self.window_active && matches!(self.dotify.tracker, Tracker::Running(_))
    }

    /// `check-status` → the pill. Totals too when stale or the popover is open.
    pub(super) fn dotify_sync(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if self.dotify.syncing || !self.dotify_mode() {
            return;
        }
        self.dotify.syncing = true;
        let totals = self.dotify.open || self.dotify.totals_at.is_none_or(|at| at.elapsed() > TOTALS_EVERY);
        cx.spawn(async move |this, cx| {
            let res = client.call("dotify", serde_json::json!({ "op": "status" })).await;
            let _ = this.update(cx, |this, cx| {
                this.dotify.syncing = false;
                if this.dotify.manual_spin.is_some() {
                    this.dotify.manual_done = Some(Instant::now());
                }
                this.dotify.synced_at = Some(Instant::now());
                match res.and_then(|v| serde_json::from_value::<Status>(v).map_err(|e| e.to_string())) {
                    Ok(status) => {
                        if !this.dotify.demo {
                            this.dotify_set_error(None);
                            this.dotify.synced_wall_ms = Some(now_ms());
                            this.dotify_apply_status(&status);
                        }
                    }
                    Err(err) => {
                        if !this.dotify.demo {
                            let err = DotifyError::from_wire(&err);
                            tracing::info!(target: "keel_ui::dotify", kind = err_kind(&err), "sync failed");
                            this.dotify_set_error(Some(err));
                        }
                    }
                }
                this.dotify_ensure_tick(cx);
                cx.notify();
            });
            if totals {
                let _ = this.update(cx, |this, cx| this.dotify_load_totals(cx));
            }
        })
        .detach();
    }

    /// Sync by hand: the refresh icon and the pill's dot spin.
    pub(super) fn dotify_manual_sync(&mut self, cx: &mut Context<Self>) {
        self.dotify.manual_spin = Some(Instant::now());
        self.dotify.manual_done = None;
        self.dotify.totals_at = None;
        self.dotify_sync(cx);
        cx.notify();
    }

    pub(super) fn dotify_set_error(&mut self, err: Option<DotifyError>) {
        let same_kind = match (&self.dotify.sync_error, &err) {
            (Some(a), Some(b)) => std::mem::discriminant(a) == std::mem::discriminant(b),
            (None, None) => true,
            _ => false,
        };
        if !same_kind {
            self.dotify.error_at = Some(Instant::now());
        }
        self.dotify.sync_error = err;
    }

    fn dotify_apply_status(&mut self, status: &Status) {
        let (next, change) = reconcile(&self.dotify.tracker, self.dotify.pending.as_ref(), status);
        if self.dotify.pending.is_none() {
            self.dotify_set_overtime(status.overtime, change != SyncChange::None || self.dotify.overtime_at.is_some());
        }
        if change != SyncChange::None {
            tracing::info!(target: "keel_ui::dotify", ?change, running = matches!(next, Tracker::Running(_)), "dotify state");
        }
        if change == SyncChange::StoppedElsewhere {
            self.dotify.confirm_out.cancel();
        }
        self.dotify.tracker = next;
        if self.dotify.hold_view_until.is_none_or(|h| Instant::now() >= h) {
            let before = self.dotify.flow.view;
            self.dotify.flow.tracker_changed(&self.dotify.tracker);
            if before != self.dotify.flow.view {
                self.dotify.view_at = Some(Instant::now());
            }
        }
    }

    /// Overtime on/off. The tint and glow play only when it turns on while
    /// we're watching (`animate`), not on the first sync.
    fn dotify_set_overtime(&mut self, on: bool, animate: bool) {
        if on && !self.dotify.overtime {
            self.dotify.overtime_at = Some(if animate && self.dotify.tracker != Tracker::Unknown {
                Instant::now()
            } else {
                Instant::now() - Duration::from_secs(5)
            });
            if self.dotify.tracker != Tracker::Unknown {
                self.dotify.overtime_wall_ms = Some(now_ms());
            }
        }
        if !on {
            self.dotify.overtime_at = None;
            self.dotify.overtime_wall_ms = None;
        }
        self.dotify.overtime = on;
    }

    /// Week / period hours and today's entries.
    fn dotify_load_totals(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.dotify.totals_at = Some(Instant::now());
        cx.spawn(async move |this, cx| {
            let summary = client.call("dotify", serde_json::json!({ "op": "summary" })).await;
            let entries = client.call("dotify", serde_json::json!({ "op": "entries" })).await;
            let _ = this.update(cx, |this, cx| {
                if let Ok(s) = summary.and_then(|v| serde_json::from_value::<Summary>(v).map_err(|e| e.to_string())) {
                    this.dotify.summary = Some(s);
                }
                if let Ok(v) = entries {
                    let all: Vec<Entry> = serde_json::from_value(v["entries"].clone()).unwrap_or_default();
                    let today = local_day_start(now_ms(), local_offset_s());
                    this.dotify.today_entries = all.into_iter().filter(|e| e.end_ms.unwrap_or(i64::MAX) >= today).collect();
                    this.dotify.today_h = Some(hours_on(&this.dotify.today_entries, today, now_ms()));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Hours today: the finished entries plus the one running now.
    pub(super) fn dotify_today_h(&self) -> Option<f64> {
        let base = self.dotify.today_h?;
        let today = local_day_start(now_ms(), local_offset_s());
        let finished: Vec<Entry> = self.dotify.today_entries.iter().filter(|e| e.end_ms.is_some()).cloned().collect();
        let mut h = hours_on(&finished, today, now_ms());
        if let Tracker::Running(e) = &self.dotify.tracker {
            h += hours_on(std::slice::from_ref(e), today, now_ms());
        } else if finished.len() != self.dotify.today_entries.len() {
            return Some(base);
        }
        Some(h)
    }

    // ── Nudge ────────────────────────────────────────────────────────────

    fn dotify_nudge_check(&mut self) {
        let now = now_ms();
        let busy = self.state.chats.values().any(|c| c.status == keel_proto::ChatStatus::Running);
        self.dotify.busy_since = if busy { Some(self.dotify.busy_since.unwrap_or(now)) } else { None };
        if !self.state.ui_settings.dotify_nudge || self.dotify.demo {
            return;
        }
        if should_nudge(&self.dotify.tracker, self.dotify.busy_since, self.dotify.memory.last_nudge_ms, now, local_offset_s()) {
            let posted = crate::platform_notify::show_banner(
                "Not clocked in to Dotify",
                "Keel has been working for 15 minutes. Clock in from the timer pill.",
                None,
            );
            tracing::info!(target: "keel_ui::dotify", posted, "clock-in nudge");
            self.dotify.memory.last_nudge_ms = Some(now);
            self.dotify.memory.save(&self.state.data_dir);
        }
    }

    // ── Repo → project switch ────────────────────────────────────────────

    /// A folder's repo key, from its `origin` remote (cached).
    fn dotify_repo(&mut self, folder: &str) -> String {
        if let Some(key) = self.dotify.repo_keys.get(folder) {
            return key.clone();
        }
        let origin = std::process::Command::new("git")
            .args(["-C", folder, "remote", "get-url", "origin"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        let key = repo_key(folder, &origin);
        self.dotify.repo_keys.insert(folder.to_string(), key.clone());
        key
    }

    /// The chat Keel is working in: the one on screen if it's running, else
    /// any running chat.
    fn dotify_working_chat(&self) -> Option<&crate::state::ChatItem> {
        let running = |c: &&crate::state::ChatItem| c.status == keel_proto::ChatStatus::Running;
        let active = self.state.active_tab.as_ref().and_then(|id| self.state.chats.get(id)).filter(running);
        active.or_else(|| self.state.chats.values().find(running))
    }

    /// Every minute: Keel working in a repo linked to another project than
    /// the running one → offer a switch (in-app pill; a banner when Keel is
    /// in the background).
    pub(super) fn dotify_switch_check(&mut self, cx: &mut Context<Self>) {
        let now = now_ms();
        let chat = self.dotify_working_chat().map(|c| (c.space_id.clone(), c.title.clone(), c.branch.clone()));
        let folder = chat.as_ref().and_then(|(space, ..)| self.state.spaces.get(space)).map(|s| s.path.clone());
        let repo = folder.as_deref().map(|f| self.dotify_repo(f));
        self.dotify.work = match (repo, self.dotify.work.take()) {
            (Some(r), Some((w, since))) if r == w => Some((w, since)),
            (Some(r), _) => Some((r, now)),
            (None, _) => None,
        };
        // An offer goes stale once its project is running or nothing is.
        if let Some(o) = &self.dotify.switch
            && o.busy.is_none()
            && !matches!(&self.dotify.tracker, Tracker::Running(e) if e.project.as_ref().is_none_or(|p| p.id != o.project.id))
        {
            self.dotify.switch = None;
            cx.notify();
        }
        if !self.state.ui_settings.dotify_nudge || self.dotify.demo || self.dotify.switch.is_some() || self.dotify.pending.is_some() {
            return;
        }
        let (Some((repo, since)), Some((_, title, branch)), Some(folder)) = (self.dotify.work.clone(), chat, folder) else {
            return;
        };
        let Some(project) = self.dotify.memory.switch_suggestion(&self.dotify.tracker, &repo, since, now) else {
            return;
        };
        let running = match &self.dotify.tracker {
            Tracker::Running(e) => e.project.as_ref().map_or("another project".to_string(), |p| p.name.clone()),
            _ => String::new(),
        };
        let posted = !crate::platform_notify::app_active()
            && crate::platform_notify::show_banner(
                &format!("Working on {}?", project.name),
                &format!("Keel is working in {}, but Dotify is tracking {running}. Open Keel to switch.", repo_label(&repo)),
                None,
            );
        tracing::info!(target: "keel_ui::dotify", repo = %repo, from = %running, to = %project.name, posted, "switch suggested");
        self.dotify.memory.switch_asked.insert(repo.clone(), now);
        self.dotify.memory.save(&self.state.data_dir);
        let description = prefill_description(&title, &branch).text;
        self.dotify.switch = Some(SwitchOffer { repo, folder, project, description, busy: None });
        cx.notify();
    }

    /// Not now (cooldown already set) or Don't ask for this repo.
    pub(super) fn dotify_switch_dismiss(&mut self, forever: bool, cx: &mut Context<Self>) {
        let Some(o) = self.dotify.switch.take() else { return };
        if forever && !self.dotify.memory.repo_ignored.contains(&o.repo) {
            self.dotify.memory.repo_ignored.push(o.repo.clone());
            self.dotify.memory.save(&self.state.data_dir);
        }
        tracing::info!(target: "keel_ui::dotify", repo = %o.repo, forever, "switch dismissed");
        cx.notify();
    }

    /// Clock out of the running project, clock in to the repo's.
    pub(super) fn dotify_switch_accept(&mut self, cx: &mut Context<Self>) {
        let Some(o) = self.dotify.switch.as_mut().filter(|o| o.busy.is_none()) else { return };
        if self.dotify.pending.is_some() || !matches!(self.dotify.tracker, Tracker::Running(_)) {
            return;
        }
        o.busy = Some(Instant::now());
        let o = o.clone();
        let Tracker::Running(running) = self.dotify.tracker.clone() else { return };
        self.dotify.pending = Some(Pending::ClockOut);
        self.dotify.pending_at = Some(Instant::now());
        self.dotify.rejected = None;
        cx.notify();
        let stopped_at = now_ms();
        let client = self.client.clone();
        let demo = self.dotify.demo;
        cx.spawn(async move |this, cx| {
            let out: Result<Entry, String> = if demo {
                cx.background_executor().timer(Duration::from_millis(500)).await;
                Ok(Entry { end_ms: Some(stopped_at), duration_h: running.elapsed_ms(stopped_at) as f64 / 3_600_000., ..running.clone() })
            } else if let Some(c) = &client {
                c.call("dotify", serde_json::json!({ "op": "clock_out" }))
                    .await
                    .and_then(|v| serde_json::from_value::<Entry>(v["entry"].clone()).map_err(|e| e.to_string()))
            } else {
                Err("not connected".into())
            };
            let out = match out {
                Ok(e) => e,
                Err(err) => {
                    let _ = this.update(cx, |this, cx| {
                        this.dotify.pending = None;
                        this.dotify.switch = None;
                        this.dotify_fail(err);
                        this.dotify_open_popover(cx);
                    });
                    return;
                }
            };
            let _ = this.update(cx, |this, cx| {
                this.dotify.tracker = Tracker::Idle;
                this.dotify.pending = Some(Pending::ClockIn { started_ms: now_ms() });
                cx.notify();
            });
            let inn: Result<Entry, String> = if demo {
                cx.background_executor().timer(Duration::from_millis(500)).await;
                Ok(Entry { id: -2, description: o.description.clone(), project: Some(o.project.clone()), start_ms: now_ms(), ..Default::default() })
            } else if let Some(c) = &client {
                c.call("dotify", serde_json::json!({ "op": "clock_in", "project_id": o.project.id, "description": o.description }))
                    .await
                    .and_then(|v| serde_json::from_value::<Entry>(v["entry"].clone()).map_err(|e| e.to_string()))
            } else {
                Err("not connected".into())
            };
            let _ = this.update(cx, |this, cx| {
                this.dotify.switch = None;
                match inn {
                    Ok(entry) => {
                        let hours = if out.duration_h > 0. { out.duration_h } else { out.elapsed_ms(stopped_at) as f64 / 3_600_000. };
                        let from = out.project.as_ref().map_or(String::new(), |p| format!(" on {}", p.name));
                        tracing::info!(target: "keel_ui::dotify", repo = %o.repo, to = %o.project.name, "switched");
                        this.dotify_clocked_in(entry, &o.project, Some(o.folder.clone()), cx);
                        let text = format!("Switched to {} · {} logged{from}", o.project.name, keel_proto::dotify::hours_label(hours));
                        this.dotify.receipt = Some(Receipt { text, entry: out, shown: Instant::now(), paused: Duration::ZERO, hover_since: None });
                    }
                    Err(err) => {
                        // Clocked out but not back in: say so, and show the picker.
                        this.dotify.pending = None;
                        this.dotify_fail(err);
                        this.dotify_open_popover(cx);
                        if !demo {
                            this.dotify_sync(cx);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ── Popover ──────────────────────────────────────────────────────────

    pub(super) fn dotify_toggle_popover(&mut self, cx: &mut Context<Self>) {
        if self.dotify.open && self.dotify.close_at.is_none() {
            self.dotify_close_popover(cx);
        } else if self.dotify.closed_by_outside.is_some_and(|at| at.elapsed() < Duration::from_millis(350)) {
            // The press that closed it was on the pill: stay closed.
            self.dotify.closed_by_outside = None;
        } else {
            self.dotify_open_popover(cx);
        }
    }

    pub(super) fn dotify_open_popover(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        self.dotify.open = true;
        self.dotify.open_at = Some(now);
        self.dotify.close_at = None;
        self.dotify.open_toggled_at = Some(now);
        self.dotify.flow.open(&self.dotify.tracker);
        self.dotify.view_at = Some(now);
        self.dotify.prefill = true;
        self.dotify.focus_search = true;
        self.dotify.rejected = None;
        self.dotify.confirm_out.cancel();
        self.dotify.results = None;
        self.dotify.query.clear();
        self.dotify.highlight = None;
        self.dotify.fav_snapshot = self.dotify.favorites.clone();
        self.dotify.open_fresh = true;
        let label = self.dotify_sync_label_text();
        self.dotify.sync_label.snap(&label);
        self.dotify.rows_at = None;
        self.dotify.scrolled_highlight = None;
        self.dotify.list_scroll.set_offset(gpui::point(gpui::px(0.), gpui::px(0.)));
        self.dotify.bench = Bench { on: std::env::var_os("KEEL_DOTIFY_FRAMELOG").is_some(), ..Default::default() };
        self.dotify.stats_anim = None;
        self.dotify.height = AnimW::default();
        self.dotify.receipt = None;
        self.timer_ui_close_menu_only();
        self.dotify_load_projects(cx);
        self.dotify_sync(cx);
        if self.dotify.totals_at.is_none_or(|at| at.elapsed() > Duration::from_secs(30)) {
            self.dotify_load_totals(cx);
        }
        cx.notify();
    }

    /// Starts the exit (120 ms); the popover goes once it has played.
    pub(super) fn dotify_close_popover(&mut self, cx: &mut Context<Self>) {
        if !self.dotify.open || self.dotify.close_at.is_some() {
            return;
        }
        self.dotify.close_at = Some(Instant::now());
        self.dotify.open_toggled_at = Some(Instant::now());
        self.dotify.confirm_out.cancel();
        cx.notify();
    }

    pub(super) fn dotify_close_outside(&mut self, cx: &mut Context<Self>) {
        if self.dotify.open && self.dotify.close_at.is_none() {
            self.dotify.closed_by_outside = Some(Instant::now());
            self.dotify_close_popover(cx);
        }
    }

    /// The exit played: really gone.
    pub(super) fn dotify_finish_close(&mut self) {
        self.dotify.open = false;
        self.dotify.close_at = None;
        self.dotify.flow.close();
        self.dotify.rejected = None;
        self.dotify.highlight = None;
    }

    /// Favorites and all projects (for the sections), once per connection;
    /// favorites again on each open.
    fn dotify_load_projects(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let need_all = !self.dotify.projects_loaded;
        cx.spawn(async move |this, cx| {
            let favs = client.call("dotify", serde_json::json!({ "op": "favorites" })).await;
            let all = if need_all { Some(client.call("dotify", serde_json::json!({ "op": "projects" })).await) } else { None };
            let _ = this.update(cx, |this, cx| {
                if let Ok(v) = favs {
                    let list: Vec<Project> = serde_json::from_value(v["projects"].clone()).unwrap_or_default();
                    this.dotify.fav_ids = list.iter().map(|p| p.id).collect();
                    // Only refresh the sections if the popover hasn't shown rows yet.
                    if ms_since(this.dotify.open_at) < 400. || this.dotify.fav_snapshot.is_empty() {
                        this.dotify.fav_snapshot = list.clone();
                    }
                    this.dotify.favorites = list;
                }
                if let Some(Ok(v)) = all {
                    this.dotify.all_projects = serde_json::from_value(v["projects"].clone()).unwrap_or_default();
                    this.dotify.projects_loaded = true;
                    // Late rows enter on their own timeline (from now), over the skeleton.
                    if this.dotify.open && ms_since(this.dotify.open_at) > crate::dotify::section_delay(crate::dotify::LIST_SECTION) {
                        this.dotify.rows_at = Some(Instant::now());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The rows the list shows now.
    pub(super) fn dotify_rows(&self) -> Rc<Vec<Row>> {
        self.dotify_rows_and_ranges().0
    }

    /// Rows plus highlight ranges, cached on their inputs (not per frame).
    pub(super) fn dotify_rows_and_ranges(&self) -> (Rc<Vec<Row>>, Rc<Vec<Vec<(usize, usize)>>>) {
        use std::hash::{Hash, Hasher};
        let query = self.dotify.query.trim();
        let results = if query.is_empty() { None } else { self.dotify.results.as_deref() };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        query.hash(&mut h);
        let ids = |ps: &[Project], h: &mut std::collections::hash_map::DefaultHasher| {
            ps.len().hash(h);
            for p in ps {
                p.id.hash(h);
                p.name.hash(h);
            }
        };
        ids(&self.dotify.fav_snapshot, &mut h);
        ids(&self.dotify.memory.recent, &mut h);
        ids(&self.dotify.all_projects, &mut h);
        results.map(|r| ids(r, &mut h));
        let key = h.finish();
        if let Some(c) = self.dotify.rows_cache.borrow().as_ref().filter(|c| c.key == key) {
            return (c.rows.clone(), c.ranges.clone());
        }
        let rows = Rc::new(list_rows(&self.dotify.fav_snapshot, &self.dotify.memory.recent, &self.dotify.all_projects, results));
        let ranges: Rc<Vec<Vec<(usize, usize)>>> = Rc::new(rows.iter().map(|r| if query.is_empty() { Vec::new() } else { crate::dotify::match_ranges(&r.project.name, query) }).collect());
        *self.dotify.rows_cache.borrow_mut() = Some(RowsCache { key, rows: rows.clone(), ranges: ranges.clone() });
        (rows, ranges)
    }

    /// The header's sync label text.
    pub(super) fn dotify_sync_label_text(&self) -> String {
        match (self.dotify.manual_spin.is_some() && self.dotify.syncing, self.dotify.synced_wall_ms) {
            (true, _) => "Syncing…".to_string(),
            (false, Some(ms)) => format!("Synced {}", keel_proto::dotify::local_hm(ms, local_offset_s())),
            (false, None) => String::new(),
        }
    }

    /// The folder the active chat works in: where the last project is kept.
    fn dotify_folder(&self) -> Option<String> {
        let chat = self.state.active_tab.as_ref().and_then(|id| self.state.chats.get(id))?;
        self.state.spaces.get(&chat.space_id).map(|s| s.path.clone())
    }

    /// Inputs get their starting text when the popover opens (needs the window).
    pub(super) fn dotify_apply_prefill(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.dotify.prefill) {
            let editing = self.dotify.flow.view == View::Edit;
            let (prefill, picked) = match (&self.dotify.flow.result, editing) {
                (Some(e), true) => (Prefill { text: e.description.clone(), from_branch: false }, e.project.clone()),
                _ => {
                    let chat = self.state.active_tab.as_ref().and_then(|id| self.state.chats.get(id));
                    let prefill = chat.map(|c| prefill_description(&c.title, &c.branch)).unwrap_or_default();
                    (prefill, self.dotify.memory.project_for(self.dotify_folder().as_deref()))
                }
            };
            if picked != self.dotify.picked {
                self.dotify.picked_at = Some(Instant::now());
            }
            self.dotify.picked = picked;
            let text = prefill.text.clone();
            self.dotify.prefill_text = prefill;
            self.dotify_desc.update(cx, |i, cx| i.set_value(text, window, cx));
            self.dotify_search.update(cx, |i, cx| i.set_value("", window, cx));
            self.dotify.query.clear();
            self.dotify.results = None;
        }
        if std::mem::take(&mut self.dotify.focus_search) && matches!(self.dotify.flow.view, View::Idle | View::Edit) {
            self.dotify_search.update(cx, |i, cx| i.focus(window, cx));
        }
    }

    /// The search field changed: ask after a pause (empty → sections).
    pub(super) fn dotify_search_changed(&mut self, cx: &mut Context<Self>) {
        let query = self.dotify_search.read(cx).value().trim().to_string();
        if query == self.dotify.query {
            return;
        }
        self.dotify.query = query.clone();
        self.dotify.highlight = None;
        self.dotify.search_gen += 1;
        let n = self.dotify.search_gen;
        if query.is_empty() {
            self.dotify.results = None;
            self.dotify.results_at = Some(Instant::now());
            self.dotify.searching = false;
            self.dotify.search_task = None;
            cx.notify();
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        self.dotify.searching = true;
        self.dotify.search_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            let res = client.call("dotify", serde_json::json!({ "op": "projects", "search": query })).await;
            let _ = this.update(cx, |this, cx| {
                if this.dotify.search_gen != n {
                    return;
                }
                this.dotify.searching = false;
                match res {
                    Ok(v) => {
                        this.dotify.results = Some(serde_json::from_value(v["projects"].clone()).unwrap_or_default());
                        this.dotify.results_at = Some(Instant::now());
                    }
                    Err(err) => this.dotify_fail(err),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(super) fn dotify_clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dotify_search.update(cx, |i, cx| i.set_value("", window, cx));
        self.dotify_search_changed(cx);
    }

    /// An action failed: auth and offline go to their banners, a refusal
    /// is quoted as Dotify put it.
    pub(super) fn dotify_fail(&mut self, err: String) {
        match DotifyError::from_wire(&err) {
            e @ (DotifyError::Auth(_) | DotifyError::Offline(_)) => self.dotify_set_error(Some(e)),
            e => {
                self.dotify.rejected = Some(e.text().to_string());
                self.dotify.rejected_at = Some(Instant::now());
            }
        }
    }

    pub(super) fn dotify_pick(&mut self, project: Project, index: Option<usize>, cx: &mut Context<Self>) {
        if self.dotify.picked.as_ref().map(|p| p.id) != Some(project.id) {
            self.dotify.picked_at = Some(Instant::now());
        }
        self.dotify.picked = Some(project);
        if index.is_some() {
            self.dotify.highlight = index;
        }
        self.dotify.rejected = None;
        cx.notify();
    }

    /// Star / unstar. The row stays where it is until the next open.
    pub(super) fn dotify_toggle_favorite(&mut self, project: Project, cx: &mut Context<Self>) {
        let on = !self.dotify.fav_ids.contains(&project.id);
        let pc = project.clone();
        let apply = move |ui: &mut DotifyUi, on: bool| {
            if on {
                ui.fav_ids.insert(pc.id);
                if !ui.favorites.iter().any(|p| p.id == pc.id) {
                    ui.favorites.push(pc.clone());
                }
            } else {
                ui.fav_ids.remove(&pc.id);
                ui.favorites.retain(|p| p.id != pc.id);
            }
            ui.star_flip.insert(pc.id, Instant::now());
        };
        apply(&mut self.dotify, on);
        cx.notify();
        if self.dotify.demo {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let id = project.id;
        cx.spawn(async move |this, cx| {
            let res = client.call("dotify", serde_json::json!({ "op": "favorite", "project_id": id, "on": on })).await;
            let _ = this.update(cx, |this, cx| {
                if let Err(err) = res {
                    // Put the star back.
                    apply(&mut this.dotify, !on);
                    this.dotify_fail(err);
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ── Actions ──────────────────────────────────────────────────────────

    /// Clock in: the button loads, the pill says "Clocking in…"; on
    /// success a check holds, then the clocked-in view.
    pub(super) fn dotify_clock_in(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.dotify.picked.clone() else {
            return;
        };
        if self.dotify.pending.is_some() || self.dotify.clock_in_btn.busy() || matches!(self.dotify.tracker, Tracker::Running(_)) {
            return;
        }
        let description = self.dotify_desc.read(cx).value().trim().to_string();
        let now = Instant::now();
        self.dotify.pending = Some(Pending::ClockIn { started_ms: now_ms() });
        self.dotify.pending_at = Some(now);
        self.dotify.clock_in_btn.phase = super::dotify::PrimaryPhase::Loading(now);
        self.dotify.rejected = None;
        cx.notify();
        let folder = self.dotify_folder();
        if self.dotify.demo {
            let entry = Entry { id: -1, description, project: Some(project.clone()), start_ms: now_ms(), ..Default::default() };
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(700)).await;
                let _ = this.update(cx, |this, cx| this.dotify_clocked_in(entry, &project, folder, cx));
            })
            .detach();
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let res = client
                .call("dotify", serde_json::json!({ "op": "clock_in", "project_id": project.id, "description": description }))
                .await;
            let _ = this.update(cx, |this, cx| {
                match res.and_then(|v| serde_json::from_value::<Entry>(v["entry"].clone()).map_err(|e| e.to_string())) {
                    Ok(entry) => {
                        tracing::info!(target: "keel_ui::dotify", "clocked in");
                        this.dotify_clocked_in(entry, &project, folder, cx);
                    }
                    Err(err) => {
                        this.dotify.pending = None;
                        this.dotify.clock_in_btn.phase = PrimaryPhase::Error(Instant::now());
                        this.dotify_fail(err);
                        if !this.dotify.open {
                            this.dotify_open_popover(cx);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn dotify_clocked_in(&mut self, entry: Entry, project: &Project, folder: Option<String>, cx: &mut Context<Self>) {
        let now = Instant::now();
        self.dotify.pending = None;
        self.dotify.tracker = Tracker::Running(entry);
        self.dotify.clock_in_btn.phase = PrimaryPhase::Success(now);
        self.dotify.hold_view_until = Some(now + SUCCESS_HOLD);
        self.dotify.memory.remember(folder.as_deref(), project);
        let repo = folder.as_deref().map(|f| self.dotify_repo(f));
        self.dotify.memory.map_repo(repo.as_deref(), project);
        if self.dotify.switch.as_ref().is_some_and(|o| o.busy.is_none()) {
            self.dotify.switch = None;
        }
        self.dotify.memory.save(&self.state.data_dir);
        self.dotify_ensure_tick(cx);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SUCCESS_HOLD).await;
            let _ = this.update(cx, |this, cx| {
                this.dotify.hold_view_until = None;
                this.dotify.clock_in_btn.phase = PrimaryPhase::Idle;
                this.dotify.flow.tracker_changed(&this.dotify.tracker);
                this.dotify.view_at = Some(Instant::now());
                if !this.dotify.demo {
                    this.dotify_sync(cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// The clocked-in view's button: first click arms, second clocks out.
    pub(super) fn dotify_clock_out_click(&mut self, cx: &mut Context<Self>) {
        if self.dotify.pending.is_some() || self.dotify.out_busy.is_some() {
            return;
        }
        if self.dotify.confirm_out.click(now_ms()) == ConfirmStep::Confirmed {
            self.dotify_clock_out(false, cx);
        }
        cx.notify();
    }

    /// The right-click menu's Clock out (also two clicks): a receipt pill
    /// shows the result.
    pub(super) fn dotify_menu_clock_out(&mut self, cx: &mut Context<Self>) -> bool {
        if self.dotify.pending.is_some() {
            return true;
        }
        if self.dotify.confirm_menu.click(now_ms()) == ConfirmStep::Confirmed {
            self.dotify_clock_out(true, cx);
            return true;
        }
        cx.notify();
        false
    }

    pub(super) fn dotify_clock_out(&mut self, from_menu: bool, cx: &mut Context<Self>) {
        let Tracker::Running(running) = self.dotify.tracker.clone() else {
            return;
        };
        let now = Instant::now();
        self.dotify.pending = Some(Pending::ClockOut);
        self.dotify.pending_at = Some(now);
        self.dotify.out_busy = Some(now);
        self.dotify.rejected = None;
        cx.notify();
        let stopped_at = now_ms();
        if self.dotify.demo {
            let entry = Entry { end_ms: Some(stopped_at), duration_h: running.elapsed_ms(stopped_at) as f64 / 3_600_000., ..running };
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(700)).await;
                let _ = this.update(cx, |this, cx| this.dotify_clocked_out(entry, stopped_at, from_menu, cx));
            })
            .detach();
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let res = client.call("dotify", serde_json::json!({ "op": "clock_out" })).await;
            let _ = this.update(cx, |this, cx| {
                match res.and_then(|v| serde_json::from_value::<Entry>(v["entry"].clone()).map_err(|e| e.to_string())) {
                    Ok(entry) => {
                        tracing::info!(target: "keel_ui::dotify", "clocked out");
                        this.dotify_clocked_out(entry, stopped_at, from_menu, cx);
                    }
                    Err(err) => {
                        this.dotify.pending = None;
                        this.dotify.out_busy = None;
                        this.dotify_fail(err);
                        if !this.dotify.open {
                            this.dotify_open_popover(cx);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn dotify_clocked_out(&mut self, entry: Entry, stopped_at: i64, from_menu: bool, cx: &mut Context<Self>) {
        self.dotify.pending = None;
        self.dotify.out_busy = None;
        self.dotify.tracker = Tracker::Idle;
        self.dotify.totals_at = None;
        let hours = if entry.duration_h > 0. { entry.duration_h } else { entry.elapsed_ms(stopped_at) as f64 / 3_600_000. };
        let text = format!("Clocked out · {}", keel_proto::dotify::hours_label(hours));
        if from_menu || !self.dotify.open || self.dotify.close_at.is_some() {
            self.dotify.receipt = Some(Receipt { text, entry, shown: Instant::now(), paused: Duration::ZERO, hover_since: None });
        } else {
            self.dotify.flow.clocked_out(entry);
            self.dotify.view_at = Some(Instant::now());
        }
        self.dotify_ensure_tick(cx);
        if !self.dotify.demo {
            self.dotify_sync(cx);
        }
        cx.notify();
    }

    /// Edit the entry just stopped (from the result or the receipt).
    pub(super) fn dotify_edit_result(&mut self, cx: &mut Context<Self>) {
        if let Some(receipt) = self.dotify.receipt.take() {
            if !self.dotify.open {
                self.dotify_open_popover(cx);
            }
            self.dotify.flow.clocked_out(receipt.entry);
        }
        if self.dotify.flow.edit() {
            self.dotify.view_at = Some(Instant::now());
            self.dotify.prefill = true;
            self.dotify.focus_search = true;
        }
        cx.notify();
    }

    /// Save the edit: project and description (its times stay).
    pub(super) fn dotify_save_edit(&mut self, cx: &mut Context<Self>) {
        let (Some(entry), Some(project)) = (self.dotify.flow.result.clone(), self.dotify.picked.clone()) else {
            return;
        };
        let Some(end) = entry.end_ms else {
            return;
        };
        if self.dotify.save_btn.busy() {
            return;
        }
        let description = self.dotify_desc.read(cx).value().trim().to_string();
        self.dotify.save_btn.phase = PrimaryPhase::Loading(Instant::now());
        self.dotify.rejected = None;
        cx.notify();
        if self.dotify.demo {
            let saved = Entry { description, project: Some(project), ..entry };
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(600)).await;
                let _ = this.update(cx, |this, cx| this.dotify_saved(saved, cx));
            })
            .detach();
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let res = client
                .call(
                    "dotify",
                    serde_json::json!({ "op": "update_entry", "id": entry.id, "project_id": project.id, "description": description, "end_ms": end }),
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                match res.and_then(|v| serde_json::from_value::<Entry>(v["entry"].clone()).map_err(|e| e.to_string())) {
                    Ok(saved) => this.dotify_saved(saved, cx),
                    Err(err) => {
                        this.dotify.save_btn.phase = PrimaryPhase::Error(Instant::now());
                        this.dotify_fail(err);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn dotify_saved(&mut self, saved: Entry, cx: &mut Context<Self>) {
        self.dotify.save_btn.phase = PrimaryPhase::Success(Instant::now());
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SUCCESS_HOLD).await;
            let _ = this.update(cx, |this, cx| {
                this.dotify.save_btn.phase = PrimaryPhase::Idle;
                this.dotify.flow.saved(saved);
                this.dotify.view_at = Some(Instant::now());
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    // ── Keys ─────────────────────────────────────────────────────────────

    /// The popover's keys, ahead of everything else. True when used.
    pub(super) fn dotify_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.dotify.open || self.dotify.close_at.is_some() {
            return false;
        }
        let key = event.keystroke.key.as_str();
        let m = event.keystroke.modifiers;
        let view = self.dotify.flow.view;
        let picking = matches!(view, View::Idle | View::Edit);
        let in_search = gpui::Focusable::focus_handle(self.dotify_search.read(cx), cx).is_focused(window);
        match key {
            "escape" => {
                if !self.dotify.query.is_empty() {
                    self.dotify_clear_search(window, cx);
                } else if self.dotify.confirm_out.armed(now_ms()) {
                    self.dotify.confirm_out.cancel();
                } else if view == View::Edit {
                    self.dotify.flow.cancel_edit();
                    self.dotify.view_at = Some(Instant::now());
                } else {
                    self.dotify_close_popover(cx);
                }
                cx.notify();
                true
            }
            "up" | "down" if picking => {
                let len = self.dotify_rows().len();
                self.dotify.highlight = step_highlight(self.dotify.highlight, len, key == "down");
                cx.notify();
                true
            }
            "enter" if picking && in_search && !m.shift => {
                let rows = self.dotify_rows();
                match self.dotify.highlight.and_then(|i| rows.get(i).map(|r| (i, r.project.clone()))) {
                    Some((i, p)) if self.dotify.picked.as_ref().map(|q| q.id) != Some(p.id) => self.dotify_pick(p, Some(i), cx),
                    _ if view == View::Edit => self.dotify_save_edit(cx),
                    _ => self.dotify_clock_in(cx),
                }
                true
            }
            "d" if picking && m.secondary() => {
                let rows = self.dotify_rows();
                let target = self.dotify.highlight.and_then(|i| rows.get(i).map(|r| r.project.clone())).or_else(|| self.dotify.picked.clone());
                if let Some(p) = target {
                    self.dotify_toggle_favorite(p, cx);
                }
                true
            }
            "tab" if picking && in_search && !m.shift => {
                self.dotify_desc.update(cx, |i, cx| i.focus(window, cx));
                true
            }
            "/" if picking && !in_search && !gpui::Focusable::focus_handle(self.dotify_desc.read(cx), cx).is_focused(window) => {
                self.dotify_search.update(cx, |i, cx| i.focus(window, cx));
                true
            }
            _ => false,
        }
    }

    // ── Settings ─────────────────────────────────────────────────────────

    /// Check the token with Dotify and keep it on success.
    pub(super) fn dotify_connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let token = self.dotify_token.read(cx).value().trim().to_string();
        if token.is_empty() {
            self.dotify.connect_error = Some("Paste your Dotify token first".into());
            self.dotify.connect_error_at = Some(Instant::now());
            cx.notify();
            return;
        }
        if self.dotify.connect_btn.busy() {
            return;
        }
        self.dotify_token.update(cx, |i, cx| i.set_value("", window, cx));
        self.dotify_connect_token(token, cx);
    }

    pub(super) fn dotify_connect_token(&mut self, token: String, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.dotify.connect_btn.phase = PrimaryPhase::Loading(Instant::now());
        self.dotify.connect_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = client.call("connect_dotify", serde_json::json!({ "token": token })).await;
            let _ = this.update(cx, |this, cx| {
                match res {
                    Ok(_) => {
                        tracing::info!(target: "keel_ui::dotify", "dotify connected");
                        this.dotify.connect_btn.phase = PrimaryPhase::Success(Instant::now());
                        this.dotify_set_error(None);
                        cx.spawn(async move |this, cx| {
                            cx.background_executor().timer(SUCCESS_HOLD).await;
                            let _ = this.update(cx, |this, cx| {
                                this.dotify.connect_btn.phase = PrimaryPhase::Idle;
                                this.dotify.connected_at = Some(Instant::now());
                                this.dotify.demo_settings_disconnected = false;
                                this.dotify_boot(cx);
                                this.load_connections_pub(cx);
                            });
                        })
                        .detach();
                    }
                    Err(err) => {
                        tracing::info!(target: "keel_ui::dotify", "dotify connect failed");
                        this.dotify.connect_btn.phase = PrimaryPhase::Error(Instant::now());
                        this.dotify.connect_error = Some(match DotifyError::from_wire(&err) {
                            DotifyError::Auth(_) => "Dotify didn't accept this token.".to_string(),
                            DotifyError::Offline(_) => "Couldn't reach Dotify. Check your connection.".to_string(),
                            other => other.text().to_string(),
                        });
                        this.dotify.connect_error_at = Some(Instant::now());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// "Test connection": a live check-status with the saved token.
    pub(super) fn dotify_test(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if self.dotify.test_busy.is_some() {
            return;
        }
        self.dotify.test_busy = Some(Instant::now());
        self.dotify.test_result = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = client.call("dotify", serde_json::json!({ "op": "status" })).await;
            let _ = this.update(cx, |this, cx| {
                this.dotify.test_busy = None;
                let result = match res.and_then(|v| serde_json::from_value::<Status>(v).map_err(|e| e.to_string())) {
                    Ok(s) => {
                        if !this.dotify.demo {
                            this.dotify_apply_status(&s);
                        }
                        this.dotify_set_error(None);
                        Ok("Connection OK".to_string())
                    }
                    Err(err) => Err(match DotifyError::from_wire(&err) {
                        DotifyError::Auth(_) => "Dotify token expired. Paste a new one.".to_string(),
                        DotifyError::Offline(_) => "Couldn't reach Dotify".to_string(),
                        e => e.text().to_string(),
                    }),
                };
                this.dotify.test_result = Some((result, Instant::now()));
                this.dotify_ensure_tick(cx);
                cx.notify();
                // "Connection OK" leaves after 3 s.
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(Duration::from_millis(3200)).await;
                    let _ = this.update(cx, |this, cx| {
                        if this.dotify.test_result.as_ref().is_some_and(|(r, at)| r.is_ok() && at.elapsed() >= Duration::from_secs(3)) {
                            this.dotify.test_result = None;
                            cx.notify();
                        }
                    });
                })
                .detach();
            });
        })
        .detach();
    }

    /// Disconnect (second click of the two).
    pub(super) fn dotify_disconnect_click(&mut self, cx: &mut Context<Self>) {
        if self.dotify.disconnect_confirm.click(now_ms()) == ConfirmStep::Confirmed {
            self.dotify_disconnect(cx);
        }
        cx.notify();
    }

    pub(super) fn dotify_disconnect(&mut self, cx: &mut Context<Self>) {
        let Some(conn) = self.dotify.conn.clone() else {
            return;
        };
        if self.dotify.demo {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let res = client.call("disconnect", serde_json::json!({ "kind": "dotify", "host": conn.base })).await;
            let _ = this.update(cx, |this, cx| {
                match res {
                    Ok(_) => {
                        this.dotify.loop_task = None;
                        this.dotify.tick = None;
                        this.dotify_set_conn(None, cx);
                        this.dotify.connected_at = Some(Instant::now());
                        this.load_connections_pub(cx);
                    }
                    Err(err) => this.report("disconnect failed", err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn dotify_paste_token(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) {
            let text = text.trim().to_string();
            self.dotify_token.update(cx, |i, cx| i.set_value(text, window, cx));
            self.dotify.connect_error = None;
        }
        cx.notify();
    }

    pub(super) fn dotify_toggle_token_shown(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dotify.token_shown = !self.dotify.token_shown;
        let masked = !self.dotify.token_shown;
        self.dotify_token.update(cx, |i, cx| i.set_masked(masked, window, cx));
        cx.notify();
    }

    pub(super) fn dotify_toggle_nudge(&mut self, cx: &mut Context<Self>) {
        self.state.ui_settings.dotify_nudge = !self.state.ui_settings.dotify_nudge;
        self.state.save_settings();
        self.dotify.nudge_flip_at = Some(Instant::now());
        cx.notify();
    }

    // ── Dev hooks ────────────────────────────────────────────────────────

    /// `KEEL_DOTIFY_IMPORT=<file>`: connect with the token in that file (then
    /// delete it), so Keel itself saves it.
    /// `KEEL_DOTIFY_PROBE=1`: each read-only call once, counts logged.
    /// `KEEL_DEMO_DOTIFY=<state>`: screenshot states, drawn locally only
    /// (idle, popover, selected, running, running-open, overtime, confirm,
    /// result, offline, settings, settings-disconnected).
    pub(super) fn dotify_dev_hooks(&mut self, cx: &mut Context<Self>) {
        if let Ok(path) = std::env::var("KEEL_DOTIFY_IMPORT") {
            let token = std::fs::read_to_string(&path).unwrap_or_default();
            let _ = std::fs::remove_file(&path);
            let token = token.trim().to_string();
            tracing::info!(target: "keel_ui::dotify", found = !token.is_empty(), "token import");
            if !token.is_empty() {
                self.dotify_connect_token(token, cx);
            }
        }
        if std::env::var("KEEL_DOTIFY_PROBE").is_ok()
            && let Some(client) = self.client.clone()
        {
            cx.spawn(async move |_, _| {
                let call = |op: serde_json::Value| {
                    let client = client.clone();
                    async move { client.call("dotify", op).await }
                };
                match call(serde_json::json!({ "op": "status" })).await {
                    Ok(v) => tracing::info!(target: "keel_ui::dotify", running = !v["running"].is_null(), overtime = v["overtime"].as_bool().unwrap_or(false), "probe check-status ok"),
                    Err(e) => tracing::info!(target: "keel_ui::dotify", err = %e, "probe check-status failed"),
                }
                match call(serde_json::json!({ "op": "summary" })).await {
                    Ok(v) => tracing::info!(target: "keel_ui::dotify", week = %v["week_h"], week_normal = %v["week_normal_h"], period = %v["period_h"], period_normal = %v["period_normal_h"], "probe summary ok"),
                    Err(e) => tracing::info!(target: "keel_ui::dotify", err = %e, "probe summary failed"),
                }
                for (label, op) in [
                    ("favorites", serde_json::json!({ "op": "favorites" })),
                    ("projects", serde_json::json!({ "op": "projects" })),
                    ("projects?search=a", serde_json::json!({ "op": "projects", "search": "a" })),
                ] {
                    match call(op).await {
                        Ok(v) => tracing::info!(target: "keel_ui::dotify", count = v["projects"].as_array().map_or(0, |a| a.len()), "probe {label} ok"),
                        Err(e) => tracing::info!(target: "keel_ui::dotify", err = %e, "probe {label} failed"),
                    }
                }
                match call(serde_json::json!({ "op": "entries" })).await {
                    Ok(v) => {
                        let rows = v["entries"].as_array().cloned().unwrap_or_default();
                        let locked = rows.iter().filter(|e| e["locked"].as_bool() == Some(true)).count();
                        tracing::info!(target: "keel_ui::dotify", count = rows.len(), locked, "probe time-entities ok");
                    }
                    Err(e) => tracing::info!(target: "keel_ui::dotify", err = %e, "probe time-entities failed"),
                }
            })
            .detach();
        }
        let Ok(demo) = std::env::var("KEEL_DEMO_DOTIFY") else {
            return;
        };
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(4)).await;
            // Wait for the connection and the first status, so they don't land over the demo.
            for _ in 0..30 {
                let ready = this
                    .update(cx, |this, _| {
                        let ready = demo.starts_with("settings") || (this.dotify_mode() && this.dotify.tracker != Tracker::Unknown);
                        tracing::debug!(target: "keel_ui::dotify", mode = this.dotify_mode(), unknown = this.dotify.tracker == Tracker::Unknown, ready, "demo wait");
                        ready
                    })
                    .unwrap_or(true);
                if ready {
                    break;
                }
                cx.background_executor().timer(Duration::from_millis(500)).await;
            }
            if demo == "open-real" {
                // A real open (read-only calls only), closed and opened again, for the frame log.
                for step in 0..3 {
                    let _ = this.update(cx, |this, cx| match step {
                        1 => this.dotify_close_popover(cx),
                        _ => this.dotify_open_popover(cx),
                    });
                    cx.background_executor().timer(Duration::from_millis(2500)).await;
                }
                return;
            }
            let _ = this.update(cx, |this, cx| this.dotify_demo(&demo, cx));
            if demo == "switch-accept" {
                cx.background_executor().timer(Duration::from_secs(3)).await;
                let _ = this.update(cx, |this, cx| this.dotify_switch_accept(cx));
            }
            if demo == "popover" || demo == "selected" {
                for _ in 0..30 {
                    cx.background_executor().timer(Duration::from_millis(300)).await;
                    if this.update(cx, |this, _| this.dotify.projects_loaded).unwrap_or(true) {
                        break;
                    }
                }
                let _ = this.update(cx, |this, cx| this.dotify_demo_rows(&demo, cx));
            }
            if demo.starts_with("settings") {
                for _ in 0..4 {
                    cx.background_executor().timer(Duration::from_millis(400)).await;
                    let _ = this.update(cx, |this, cx| {
                        this.dotify.settings_scroll.scroll_to_bottom();
                        cx.notify();
                    });
                }
            }
            if demo == "confirm" {
                // Keep the confirm armed for the screenshot.
                for _ in 0..20 {
                    cx.background_executor().timer(Duration::from_millis(3000)).await;
                    let _ = this.update(cx, |this, cx| {
                        this.dotify.confirm_out = Confirm::default();
                        this.dotify.confirm_out.click(now_ms());
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    fn dotify_demo(&mut self, demo: &str, cx: &mut Context<Self>) {
        self.timer.hidden = false;
        let fake = |this: &Self, ago_s: i64| {
            let project = this.dotify.all_projects.get(3).cloned().unwrap_or(Project::new(0, "Keel"));
            Entry {
                id: -1,
                description: "feat/dotify-timer · pill and popover polish".into(),
                project: Some(project),
                start_ms: now_ms() - ago_s * 1000,
                ..Default::default()
            }
        };
        if demo != "settings" {
            self.dotify.demo = true;
        }
        match demo {
            "idle" | "popover" | "selected" => {
                self.dotify.tracker = Tracker::Idle;
                if demo != "idle" {
                    self.dotify_open_popover(cx);
                }
            }
            "running" | "running-open" | "confirm" => {
                self.dotify.tracker = Tracker::Running(fake(self, 2 * 3600 + 14 * 60 + 9));
                if demo != "running" {
                    self.dotify_open_popover(cx);
                }
                if demo == "confirm" {
                    self.dotify.confirm_out.click(now_ms());
                }
            }
            "overtime" => {
                self.dotify.tracker = Tracker::Running(fake(self, 8 * 3600 + 24 * 60));
                self.dotify.overtime = true;
                self.dotify.overtime_at = Some(Instant::now());
                self.dotify.overtime_wall_ms = Some(now_ms() - 24 * 60_000);
            }
            "result" => {
                let entry = Entry { end_ms: Some(now_ms()), duration_h: 4.25, ..fake(self, 4 * 3600 + 15 * 60) };
                self.dotify.tracker = Tracker::Idle;
                self.dotify_open_popover(cx);
                self.dotify.flow.clocked_out(entry);
            }
            "offline" => {
                self.dotify.tracker = Tracker::Idle;
                self.dotify_set_error(Some(DotifyError::Offline("demo".into())));
                self.dotify_open_popover(cx);
            }
            "switch" | "switch-accept" => {
                self.dotify.tracker = Tracker::Running(Entry { project: Some(Project::new(222, "DLTX Solutions")), ..fake(self, 2 * 3600 + 14 * 60) });
                let project = Project::new(178, "Resona - Web Revamp");
                self.dotify.switch = Some(SwitchOffer {
                    repo: "gitlab.dot.co.id/resona/web-revamp".into(),
                    folder: String::new(),
                    project,
                    description: "feat/warranty-claims".into(),
                    busy: None,
                });
            }
            "settings" => self.open_settings(cx),
            "settings-disconnected" => {
                self.dotify.demo_settings_disconnected = true;
                self.open_settings(cx);
            }
            _ => {}
        }
        self.dotify_ensure_tick(cx);
        tracing::info!(target: "keel_ui::dotify", demo = %demo, "dotify demo");
        cx.notify();
    }
}

impl KeelWindow {
    /// Demo sections (local only, never saved): favorites and recent sampled
    /// from the loaded list; `selected` also picks a project.
    fn dotify_demo_rows(&mut self, demo: &str, cx: &mut Context<Self>) {
        if self.dotify.all_projects.is_empty() {
            // Dotify unreachable: sample names.
            let names = [
                "ANALYST", "Analyst (Intern)", "Bikin Konten", "Boilerplate - BE", "Boilerplate - FE", "Boilerplate - Mobile", "Design System", "Dotify",
                "Internal Tools", "Keel", "Mobile App", "Research", "Website Revamp",
            ];
            self.dotify.all_projects = names.iter().enumerate().map(|(i, n)| Project::new(-(i as i64) - 1, *n)).collect();
            self.dotify.projects_loaded = true;
        }
        self.dotify.memory.recent = self.dotify.all_projects.iter().skip(10).take(3).cloned().collect();
        self.dotify.fav_snapshot = self.dotify.all_projects.iter().skip(4).take(2).cloned().collect();
        self.dotify.fav_ids = self.dotify.fav_snapshot.iter().map(|p| p.id).collect();
        // Replay the entry with the sections in place.
        self.dotify.open_fresh = true;
        self.dotify.bench = Bench { on: std::env::var_os("KEEL_DOTIFY_FRAMELOG").is_some(), ..Default::default() };
        if demo == "selected" {
            self.dotify.picked = self.dotify.all_projects.get(10).cloned();
            self.dotify.picked_at = Some(Instant::now());
            self.dotify.highlight = None;
        }
        cx.notify();
    }
}

fn err_kind(e: &DotifyError) -> &'static str {
    match e {
        DotifyError::Auth(_) => "auth",
        DotifyError::Offline(_) => "offline",
        DotifyError::Rejected(_) => "rejected",
        DotifyError::Server(_) => "server",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_snap_never_moves_a_keyed_width() {
        // Settled content: the ±0.5 px snap while the header glides is ignored.
        assert!(!size_follows(0.5, true, 5_000.));
        assert!(!size_follows(-0.5, true, 5_000.));
        assert!(!size_follows(1.0, true, 5_000.));
        // A real change (a longer label) still eases.
        assert!(size_follows(24., true, 5_000.));
        // Just after the content changed, small measured steps are followed.
        assert!(size_follows(0.5, true, 100.));
        // Unkeyed sizes keep the half-pixel rule.
        assert!(size_follows(0.5, false, 5_000.));
        assert!(!size_follows(0.4, false, 5_000.));
    }
}

//! The session card: always open at the top right of a chat, over the
//! transcript (which keeps a gutter for it, so no text sits under it; the
//! composer keeps the same gutter, so both share one column).
//!
//! Three labelled sections, the way dev-tool status panels (Linear, Zed,
//! Raycast) group them:
//! - Context: used / total and %, a bar in the accent (amber from 70%, red
//!   from 90%), and the token split (in, out, cached). The model rides on
//!   the label line.
//! - Project: the folder, or, when the chat belongs to one, the project name
//!   and every folder in the same shape: its name, the branch it is on, and
//!   the files that changed there. A project has no branch picker. A chat
//!   that is not in a project keeps the branch button, which switches that
//!   chat's checkout with the same uncommitted-changes guard as starting a
//!   chat on a branch. Refreshed on turn end, when the engine sees the
//!   checkout change, after a switch, and every few seconds while the
//!   window is active.
//! - Background: what runs for this chat besides the turn itself
//!   (subagents, the browser agent), each with a status dot, elapsed time
//!   and an always-visible stop button (dimmed, with the reason, where no
//!   per-item stop exists). "Nothing running" otherwise.
//!
//! When the transcript is too narrow for the gutter, the card folds into a
//! one-row pill.
//!
//! Look: solid floating surface, the app's own type ramp (13 px body, 12 px
//! secondary, 11 px labels), 12 px padding on an 8 px rhythm, hairline
//! dividers between sections. Motion uses the strong ease-out
//! (`cubic-bezier(0.23, 1, 0.32, 1)`), stays at or under 300 ms, and is
//! timestamp-driven: frames are asked for only while something moves.
//! Reduced motion keeps fades and drops movement.

use crate::hover_fade::{HoverFadeExt, reveal_on_hover};
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, ClickEvent, Context, ElementId, FontFeatures, Hsla, IntoElement, SharedString,
    Window, div, prelude::*, px,
};
use keel_proto::ChatStatus;
use keel_theme::Tokens;
use serde_json::Value;

use super::KeelWindow;
use super::composer_view::Picker;
use super::dotify_ui::Tip;
use crate::icons::Icon;
use crate::motion::{ease_out_strong, row_enter, row_exit};
use crate::style::{KeelStyled, MONO, RADIUS_LG, RADIUS_MD, RADIUS_SM, c, zpx};
use crate::usage::{ChatUsage, ModelTokens, compact};

/// The card's width. Its least inset from the transcript's top and right
/// edges, and its padding.
pub(super) const CARD_W: f32 = 340.;
const EDGE: f32 = 12.;
const PAD: f32 = 12.;
/// From the transcript's text (and the composer's edge) to the card.
pub(super) const COLUMN_GAP: f32 = 24.;
/// The rows' own side inset (`px_4`): the column box ends this far past its text.
const ROW_INSET: f32 = 16.;
/// The card opens beside the column while this much reading width is left
/// (or the column's own max, if that is less); below it, the pill.
pub(super) const MIN_COLUMN: f32 = 520.;
/// Between a section's lines, and the room either side of a divider.
const LINE_GAP: f32 = 6.;
const SECTION_GAP: f32 = 10.;
/// The context bar.
const BAR_H: f32 = 4.;
/// The branch button.
const BRANCH_H: f32 = 28.;
/// The pill: its height, and how far a narrow transcript drops below it.
const PILL_H: f32 = 26.;
/// The timer pill resting in the same corner: the card sits this far below it.
const TIMER_GAP: f32 = 8.;
/// Below this much reading width (the column's max) the card folds; it
/// opens again only past it by this much, so a width at the edge doesn't
/// flip it back and forth.
const HYSTERESIS: f32 = 16.;
/// Full ↔ pill.
const MODE_GLIDE: Duration = Duration::from_millis(240);
/// The card arriving with its chat: fades in and settles 4 px down.
const ENTER: Duration = Duration::from_millis(200);
const ENTER_DROP: f32 = 4.;
/// A number changing: a straight crossfade.
const FADE: Duration = Duration::from_millis(120);
/// The bar (and the pill's ring) easing to a new reading.
const SWEEP: Duration = Duration::from_millis(300);
const RING: f32 = 12.;
const RING_STROKE: f32 = 1.75;
/// Git: a change waits this long for more before it asks; the poll asks
/// this often while the window is active.
const DEBOUNCE: Duration = Duration::from_millis(400);
const POLL: Duration = Duration::from_secs(5);
/// Between two browser requests the agent is still at it.
const BROWSER_GRACE: Duration = Duration::from_millis(2500);
/// One background line and how many show before "+N more".
const ROW_H: f32 = 24.;
const MAX_ROWS: usize = 3;
/// A refused branch switch stays readable this long.
const NOTICE_HOLD: Duration = Duration::from_secs(10);
/// Changed paths named under one folder on the card. The rest is "N more".
const CARD_FILE_ROWS: usize = 2;

pub(super) const SUBAGENT_NO_STOP: &str =
    "Subagents can't be stopped one at a time. Stop in the composer ends the whole turn.";
const BROWSER_STOP: &str = "Stop the browser agent's work for this chat";
const BRANCH_TIP: &str = "Switch the branch this chat runs in";
const BRANCH_BUSY: &str =
    "Can't switch branches while the agent is working. Wait for the turn to end.";
const BRANCH_BUSY_OTHER: &str =
    "Another chat's agent is working in this folder. Switch when its turn ends.";
const BRANCH_SWITCHING: &str = "Switching branches…";

/// What the engine said about the chat's working tree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) enum Git {
    #[default]
    Unknown,
    /// Not a git checkout (or the folder is gone).
    NoRepo,
    Repo(GitInfo),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct GitFileChange {
    pub path: String,
    pub status: char,
    pub added: Option<u32>,
    pub removed: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct GitInfo {
    pub branch: Option<String>,
    pub detached: Option<String>,
    pub files: u64,
    pub untracked: u64,
    pub added: u64,
    pub removed: u64,
    pub file_details: Vec<GitFileChange>,
}

/// A `chat_git_summary` reply.
pub(super) fn parse_git(v: &Value) -> Git {
    if v["repo"].as_bool() != Some(true) {
        return Git::NoRepo;
    }
    let n = |k: &str| v[k].as_u64().unwrap_or(0);
    let s = |k: &str| v[k].as_str().filter(|s| !s.is_empty()).map(str::to_string);
    let file_details = v
        .get("file_details")
        .and_then(|d| serde_json::from_value::<Vec<GitFileChange>>(d.clone()).ok())
        .unwrap_or_default();
    Git::Repo(GitInfo {
        branch: s("branch"),
        detached: s("detached"),
        files: n("files"),
        untracked: n("untracked"),
        added: n("added"),
        removed: n("removed"),
        file_details,
    })
}

impl GitInfo {
    pub(super) fn branch_label(&self) -> String {
        match (&self.branch, &self.detached) {
            (Some(b), _) => b.clone(),
            (None, Some(sha)) => format!("HEAD at {sha}"),
            _ => "no branch".into(),
        }
    }
}

/// Minimum spacing (px) between the expanded sidebar and the chat session section.
/// With the transcript rows' and composer's own `px_4` (16px), this guarantees
/// at least 64px of comfortable breathing room from the sidebar border.
pub(super) const SIDEBAR_CHAT_GAP: f32 = 48.;

/// Full mode layout for a resting pane width `w`: the card is pinned `edge`
/// from the right; the column is centered in the reading region between the
/// left edge and the card, respecting `min_left` clearance.
/// Returns the reading region's left and right padding within the pane, and the card's right inset.
pub(super) fn full_layout(
    w: f32,
    column: f32,
    card: f32,
    gap: f32,
    edge: f32,
    min_left: f32,
) -> (f32, f32, f32) {
    let base_right = edge + card + gap;
    let region = (w - base_right).max(0.);
    let col = column.min((region - min_left).max(0.));
    let slack = (region - col).max(0.);
    let left = (slack / 2.).max(min_left).floor();
    let right = (w - left - col).max(base_right);
    (left, right, edge)
}

/// Pill mode layout for a resting pane width `w`: the column is centered
/// across the pane width, respecting `min_left` clearance. Returns `(left, right, col)`.
pub(super) fn pill_layout(w: f32, column: f32, min_left: f32) -> (f32, f32, f32) {
    let col = column.min((w - min_left).max(0.));
    let slack = (w - col).max(0.);
    let left = (slack / 2.).max(min_left).floor();
    let right = (w - left - col).max(0.).floor();
    (left, right, col)
}

/// Full or pill, given the reading width the full card would leave.
pub(super) fn next_compact(compact: bool, avail: f32, need: f32) -> bool {
    if compact {
        avail < need + HYSTERESIS
    } else {
        avail < need
    }
}

/// Where the reading column's text box sat in a frame: its left edge in the
/// window, its width, and the strip over it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct ColumnGeo {
    pub x: f32,
    pub w: f32,
    pub strip: f32,
    /// The docked composer's width. Unlike the text (`w`, held at its
    /// landing width so it wraps once), the composer is a visible box: its
    /// width glides from the last frame's to the landing one on the slide's
    /// clock, so its right edge moves one way, in step with the left.
    pub dock_w: f32,
}

/// A sidebar or right panel slide. The column wraps once at the width it
/// will land at (`to.w`) and keeps it; only its left edge and the strip
/// glide, on the slide's own curve, from where the last frame left them.
#[derive(Clone, Copy, Debug)]
pub(super) struct ColumnSlide {
    /// Which toggles this slide is for (sidebar, right panel generations).
    key: (u64, u64),
    from: ColumnGeo,
    pub to: ColumnGeo,
}

impl ColumnSlide {
    /// The width text wraps at for the whole slide: the narrower end, so it
    /// fits the gliding box on every frame and re-wraps once (at the start
    /// when narrowing, at landing when widening), never per frame.
    pub fn wrap_w(&self) -> f32 {
        let from = if self.from.dock_w > 0. {
            self.from.dock_w
        } else {
            self.from.w
        };
        from.min(self.to.w)
    }
}

/// The card's compactness mid-glide (0 full, 1 pill).
fn mode_now(card: &SessionCard, now: Instant) -> f32 {
    let target = if card.compact { 1. } else { 0. };
    match card.mode_at {
        Some(at) => {
            let p = (now.saturating_duration_since(at).as_secs_f32() / MODE_GLIDE.as_secs_f32())
                .min(1.);
            card.mode_from + (target - card.mode_from) * ease_out_strong(p)
        }
        None => target,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProcKind {
    Subagent,
    Browser,
}

/// What a row's stop button does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Stop {
    /// Cancel the chat's running and queued browser work, as turning its
    /// robot off does (the switch itself stays on).
    Browser { chat: String },
    /// No way to stop it alone; the reason is the tooltip.
    Unavailable(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProcRow {
    pub key: String,
    pub kind: ProcKind,
    pub label: String,
    pub started_ms: i64,
    pub stop: Stop,
}

/// The chat's background work: its browser agent (label, since), then its
/// live subagents (the engine's `subagents.{id}` views), oldest first.
pub(super) fn process_rows<'a>(
    chat: &str,
    subagents: impl IntoIterator<Item = &'a Value>,
    browser: Option<(String, i64)>,
) -> Vec<ProcRow> {
    let mut rows: Vec<ProcRow> = browser
        .map(|(label, since)| ProcRow {
            key: "browser".into(),
            kind: ProcKind::Browser,
            label,
            started_ms: since,
            stop: Stop::Browser {
                chat: chat.to_string(),
            },
        })
        .into_iter()
        .collect();
    let mut subs: Vec<ProcRow> = subagents
        .into_iter()
        .filter_map(|sub| {
            let status = sub["status"].as_str().unwrap_or("running");
            if !matches!(status, "running" | "blocked") {
                return None;
            }
            let text = |k: &str| {
                sub[k]
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty() && *s != "subagent")
                    .map(str::to_string)
            };
            let mut label = text("name")
                .or_else(|| text("description"))
                .unwrap_or_else(|| "subagent".into());
            if status == "blocked" {
                label.push_str(" · waiting");
            }
            Some(ProcRow {
                key: format!("sub:{}", sub["id"].as_str().unwrap_or_default()),
                kind: ProcKind::Subagent,
                label,
                started_ms: sub["created_at"].as_i64().unwrap_or(0),
                stop: Stop::Unavailable(SUBAGENT_NO_STOP),
            })
        })
        .collect();
    subs.sort_by_key(|r| r.started_ms);
    rows.extend(subs);
    rows
}

/// A value crossfading to a new one.
#[derive(Debug, Clone, Default)]
struct Fade {
    now: String,
    prev: String,
    at: Option<Instant>,
}

impl Fade {
    fn set(&mut self, value: &str, now: Instant, animate: bool) {
        if value == self.now {
            return;
        }
        if self.now.is_empty() || !animate {
            self.now = value.to_string();
            self.at = None;
            return;
        }
        self.prev = std::mem::replace(&mut self.now, value.to_string());
        self.at = Some(now);
    }

    /// 0..1 into the change, `None` at rest.
    fn progress(&self, now: Instant) -> Option<f32> {
        let p = now.saturating_duration_since(self.at?).as_secs_f32() / FADE.as_secs_f32();
        (p < 1.).then(|| ease_out_strong(p))
    }
}

#[derive(Debug, Clone, Copy)]
struct Ring {
    from: f32,
    to: f32,
    at: Instant,
}

impl Ring {
    fn at(&self, now: Instant) -> f32 {
        let p =
            (now.saturating_duration_since(self.at).as_secs_f32() / SWEEP.as_secs_f32()).min(1.);
        self.from + (self.to - self.from) * ease_out_strong(p)
    }
}

/// The chat's own branch listing, for the picker while the card opened it.
#[derive(Debug, Clone, Default)]
pub(super) struct CardBranches {
    pub branches: Vec<String>,
    pub current: Option<String>,
    pub detached: Option<String>,
    pub loading: bool,
}

impl CardBranches {
    /// A listing with the checked-out branch pinned first.
    fn from_reply(v: &Value) -> Self {
        let s = |k: &str| v[k].as_str().filter(|s| !s.is_empty()).map(str::to_string);
        let mut branches: Vec<String> = v["branches"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|b| b.as_str().map(str::to_string))
            .collect();
        let current = s("current");
        if let Some(name) = &current {
            branches.retain(|b| b != name);
            branches.insert(0, name.clone());
        }
        let detached = if current.is_some() {
            None
        } else {
            s("detached")
        };
        Self {
            branches,
            current,
            detached,
            loading: false,
        }
    }

    /// After a switch: `name` is checked out and leads the list.
    fn checked_out(&mut self, name: &str) {
        self.branches.retain(|b| b != name);
        self.branches.insert(0, name.to_string());
        self.current = Some(name.to_string());
        self.detached = None;
    }
}

/// The changes line: "+12 −3 · 4 files", untracked counted apart, or
/// `None` for a clean tree.
#[allow(dead_code)]
pub(super) fn changes_parts(info: &GitInfo) -> Option<(Option<(String, String)>, String)> {
    if info.files == 0 && info.untracked == 0 {
        return None;
    }
    let lines = (info.added + info.removed > 0)
        .then(|| (format!("+{}", info.added), format!("−{}", info.removed)));
    let tracked = info.files.saturating_sub(info.untracked);
    let mut text = match tracked {
        0 => String::new(),
        1 => "1 file changed".into(),
        n => format!("{n} files changed"),
    };
    if info.untracked > 0 {
        if !text.is_empty() {
            text.push_str(" · ");
        }
        text.push_str(&format!("{} untracked", info.untracked));
    }
    Some((lines, text))
}

pub(super) struct SessionCard {
    chat: Option<String>,
    opened_at: Instant,
    git: Git,
    fetching: bool,
    again: bool,
    fetched_at: Option<Instant>,
    debounce: u64,
    ticker: Option<gpui::Task<()>>,
    pub(super) compact: bool,
    /// The glide between modes: compactness it left from, and when.
    mode_from: f32,
    mode_at: Option<Instant>,
    /// The current glide was started by a sidebar or panel slide: the
    /// column already took its end room, so it must not follow the glide.
    slide_mode: bool,
    /// The mode change rode a right panel slide: the pane's right edge, the
    /// card's anchor, moved at once, so the outgoing form is cut (it would
    /// otherwise flash where it never was, or under the drawer) and only
    /// the incoming one glides in.
    cut_out: bool,
    /// The transcript's size last frame, and its left edge in the window.
    pub(super) size: Rc<Cell<(f32, f32)>>,
    origin_x: Rc<Cell<f32>>,
    /// The reading region's left and right padding this frame (the
    /// composer keeps the same).
    room_l: f32,
    room_x: f32,
    ring: Option<Ring>,
    fades: HashMap<&'static str, Fade>,
    entered: HashMap<String, Instant>,
    shown: Vec<ProcRow>,
    leaving: Vec<(ProcRow, Instant)>,
    /// The browser agent's current streak: label, since (ms), last seen.
    browser: Option<(String, i64, Instant)>,
    /// The branch menu was opened from the card (it lists this chat's
    /// checkout and a pick switches it).
    pub(super) picking: bool,
    branches: Option<CardBranches>,
    branch_gen: u64,
    /// A switch in flight, to this branch.
    switching: Option<String>,
    /// Why the last switch was refused, and since when.
    notice: Option<(String, Instant)>,
    /// Last known git summary per chat. A switch paints the cached value on
    /// the first frame when we have one, so the changes line never collapses
    /// to Unknown then pops back.
    git_by_chat: HashMap<String, Git>,
    /// Task steps currently painted under the card (held across a switch
    /// until the new chat's messages settle, or swapped from cache).
    pub(super) held_steps: Option<crate::steps::Steps>,
    pub(super) held_steps_chat: Option<String>,
    /// True while the current chat's messages are still loading and we have
    /// not yet committed what steps (if any) belong under the card.
    steps_pending: bool,
    /// Last known steps per chat (`None` = known empty). Lets a return visit
    /// paint the right todo card on the first frame.
    steps_by_chat: HashMap<String, Option<crate::steps::Steps>>,
    /// Repositories collapsed in the connected tree view.
    closed_repos: std::collections::HashSet<String>,
    /// Repositories where "+more" sample was expanded.
    expanded_more: std::collections::HashSet<String>,
    /// When branch was copied: (repo_key, timestamp).
    branch_copied: Option<(String, Instant)>,
}

impl Default for SessionCard {
    fn default() -> Self {
        Self {
            chat: None,
            opened_at: Instant::now(),
            git: Git::Unknown,
            fetching: false,
            again: false,
            fetched_at: None,
            debounce: 0,
            ticker: None,
            compact: false,
            mode_from: 0.,
            mode_at: None,
            slide_mode: false,
            cut_out: false,
            size: Rc::new(Cell::new((0., 0.))),
            origin_x: Rc::new(Cell::new(0.)),
            room_l: 0.,
            room_x: 0.,
            ring: None,
            fades: HashMap::new(),
            entered: HashMap::new(),
            shown: Vec::new(),
            leaving: Vec::new(),
            browser: None,
            picking: false,
            branches: None,
            branch_gen: 0,
            switching: None,
            notice: None,
            git_by_chat: HashMap::new(),
            held_steps: None,
            held_steps_chat: None,
            steps_pending: false,
            steps_by_chat: HashMap::new(),
            closed_repos: std::collections::HashSet::new(),
            expanded_more: std::collections::HashSet::new(),
            branch_copied: None,
        }
    }
}

/// Tabular figures, so counts don't jiggle as they change.
fn tnum<E: Styled>(el: E) -> E {
    el.font_features(FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)])))
}

/// The accent while there's room, amber at 70%, red at 90%.
fn fill_color(fill: f32, t: Tokens) -> Hsla {
    c(if fill >= 0.9 {
        t.danger
    } else if fill >= 0.7 {
        t.warning
    } else {
        t.accent
    })
}

/// `color` at `alpha` of its own opacity.
fn alpha(color: Hsla, a: f32) -> Hsla {
    Hsla {
        a: color.a * a,
        ..color
    }
}

fn ring(size: f32, fill: f32, track: Hsla, color: Hsla) -> AnyElement {
    gpui::canvas(
        |_, _, _| {},
        move |bounds, _, window, _| paint_ring(bounds, fill, track, color, window),
    )
    .size(zpx(size))
    .flex_none()
    .into_any_element()
}

/// A full track and a clockwise arc from twelve o'clock, as polylines.
fn paint_ring(
    bounds: gpui::Bounds<gpui::Pixels>,
    fill: f32,
    track: Hsla,
    color: Hsla,
    window: &mut Window,
) {
    let stroke = f32::from(zpx(RING_STROKE));
    let center = bounds.center();
    let radius = f32::from(bounds.size.width.min(bounds.size.height)) / 2. - stroke / 2.;
    let point = |turn: f32| {
        let a = turn * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
        gpui::point(
            center.x + px(radius * a.cos()),
            center.y + px(radius * a.sin()),
        )
    };
    let arc = |to: f32, color: Hsla, window: &mut Window| {
        if to <= 0. {
            return;
        }
        let steps = ((64. * to).ceil() as usize).max(2);
        let mut path = gpui::PathBuilder::stroke(px(stroke));
        path.move_to(point(0.));
        for i in 1..=steps {
            path.line_to(point(to * i as f32 / steps as f32));
        }
        if let Ok(path) = path.build() {
            window.paint_path(path, color);
        }
    };
    arc(1., track, window);
    arc(crate::motion::unit(fill), color, window);
}

/// The context bar: a full-width track and the used share, never thinner
/// than a dot once anything is used.
fn bar(fill: f32, track: Hsla, color: Hsla) -> AnyElement {
    let fill = crate::motion::unit(fill);
    div()
        .w_full()
        .h(zpx(BAR_H))
        .rounded_full()
        .bg(track)
        .overflow_hidden()
        .child(
            div()
                .h_full()
                .rounded_full()
                .bg(color)
                .w(gpui::relative(fill))
                .when(fill > 0., |d| d.min_w(zpx(BAR_H))),
        )
        .into_any_element()
}

/// A section's small label, with an optional trailing note.
fn section_label(text: &'static str, note: Option<AnyElement>, t: Tokens) -> AnyElement {
    div()
        .h(zpx(16.))
        .flex()
        .flex_row()
        .items_center()
        .gap(zpx(8.))
        .t_caption()
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(c(t.text_muted))
        .whitespace_nowrap()
        .child(div().flex_none().child(text))
        .child(div().flex_1())
        .children(note)
        .into_any_element()
}

/// A hairline across the card, between sections.
fn divider(t: Tokens) -> AnyElement {
    div()
        .flex_none()
        .h(px(1.))
        .mx(zpx(-PAD))
        .my(zpx(SECTION_GAP))
        .bg(c(t.border))
        .into_any_element()
}

fn elapsed(now_ms: i64, since_ms: i64) -> String {
    crate::motion::elapsed_label(Duration::from_millis((now_ms - since_ms).max(0) as u64))
}

/// The token split for the context section: "In 20k · Out 4.2k · Read 15k · Write 2k".
fn token_split(usage: &ChatUsage) -> Option<String> {
    let sum = usage.sum();
    if sum.total() == 0 {
        return None;
    }
    let mut parts = vec![
        format!("In {}", compact(sum.input)),
        format!("Out {}", compact(sum.output)),
    ];
    if sum.cache_read > 0 {
        parts.push(format!("Read {}", compact(sum.cache_read)));
    }
    if sum.cache_write > 0 {
        parts.push(format!("Write {}", compact(sum.cache_write)));
    }
    if sum.cache_warm > 0 {
        parts.push(format!("Warm {}", compact(sum.cache_warm)));
    }
    if let Some(rate) = sum.hit_rate() {
        parts.push(format!("Hit rate {:.0}%", (rate * 100.0).round()));
    }
    Some(parts.join(" · "))
}

struct RepoTreeItem {
    id: String,
    name: String,
    branch: String,
    branch_display: String,
    files_count: u32,
    total_additions: u64,
    total_deletions: u64,
    file_details: Vec<GitFileChange>,
    target_prefix: String,
}

fn truncate_branch(branch: &str) -> String {
    if branch.len() <= 16 {
        branch.to_string()
    } else {
        format!("{}…", &branch[..15])
    }
}

fn mini_diff_bar(adds: u64, dels: u64, t: Tokens) -> Option<AnyElement> {
    let total = adds + dels;
    if total == 0 {
        return None;
    }
    let add_pct = (adds as f32 / total as f32).clamp(0.05, 0.95);
    let del_pct = 1. - add_pct;
    Some(
        div()
            .flex_none()
            .w(zpx(22.))
            .h(zpx(4.))
            .rounded(px(2.))
            .overflow_hidden()
            .bg(c(t.border).opacity(0.6))
            .flex()
            .flex_row()
            .items_center()
            .child(div().h_full().w(gpui::relative(add_pct)).bg(c(t.success)))
            .child(div().h_full().w(gpui::relative(del_pct)).bg(c(t.danger)))
            .into_any_element(),
    )
}

fn leader_line(color: Hsla) -> AnyElement {
    gpui::canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let y = f32::from(bounds.center().y);
            let start_x = f32::from(bounds.origin.x);
            let end_x = start_x + f32::from(bounds.size.width);
            let dash_w = 2.5;
            let gap = 2.5;
            let mut x = start_x;
            while x + dash_w <= end_x {
                window.paint_quad(gpui::fill(
                    gpui::Bounds {
                        origin: gpui::point(px(x), px(y)),
                        size: gpui::size(px(dash_w), px(1.)),
                    },
                    color,
                ));
                x += dash_w + gap;
            }
        },
    )
    .flex_1()
    .min_w(zpx(6.))
    .h(zpx(1.))
    .into_any_element()
}

fn tree_connector(is_last: bool, border_color: Hsla) -> AnyElement {
    div()
        .relative()
        .w(zpx(16.))
        .h_full()
        .flex_none()
        .child(
            div()
                .absolute()
                .left(zpx(4.))
                .top(zpx(-4.))
                .when(is_last, |d| d.h(zpx(16.)))
                .when(!is_last, |d| d.bottom(zpx(-4.)))
                .w(px(1.5))
                .bg(border_color),
        )
        .child(
            div()
                .absolute()
                .left(zpx(4.))
                .top(zpx(4.))
                .w(zpx(12.))
                .h(zpx(8.))
                .border_l_2()
                .border_b_2()
                .border_color(border_color)
                .rounded_bl(px(5.)),
        )
        .into_any_element()
}

impl KeelWindow {
    /// Follow the open chat. Hero → chat plays the enter; chat → chat keeps
    /// the shell put (no re-enter, no room/mode reset) and only refreshes
    /// content, so the floating card does not flicker between sessions.
    pub(super) fn card_sync_chat(&mut self, cx: &mut Context<Self>) {
        if self.session_card.chat == self.current_chat {
            return;
        }
        let from_chat = self.session_card.chat.is_some();
        let to_chat = self.current_chat.is_some();
        if from_chat && to_chat {
            // Same floating shell: swap the chat id and clear per-chat rows.
            // Keep opened_at / room / mode / ring so the card stays put.
            // Keep the previous git (or the new chat's cache) so the changes
            // line never collapses to Unknown for a frame; refresh replaces
            // it atomically when the summary lands. Hold the todo card until
            // the new chat's messages settle (or paint from cache).
            let card = &mut self.session_card;
            card.chat = self.current_chat.clone();
            if let Some(id) = card.chat.as_ref() {
                if let Some(g) = card.git_by_chat.get(id).cloned() {
                    card.git = g;
                }
                // else keep previous git until fetch — never Unknown mid-switch
            }
            card.fetching = false;
            card.again = false;
            card.fetched_at = None;
            card.debounce = 0;
            card.shown.clear();
            card.leaving.clear();
            card.browser = None;
            card.entered.clear();
            card.fades.clear();
            card.closed_repos.clear();
            card.expanded_more.clear();
            card.branch_copied = None;
            card.picking = false;
            card.branches = None;
            card.branch_gen = card.branch_gen.wrapping_add(1);
            card.switching = None;
            card.notice = None;
            card.steps_pending = true;
            if let Some(id) = card.chat.as_ref() {
                if let Some(cached) = card.steps_by_chat.get(id).cloned() {
                    card.held_steps = cached;
                    card.held_steps_chat = Some(id.clone());
                }
                // else keep previous held_steps until settle
            }
            // Ring stays: it sweeps from the previous fill to the new one.
        } else {
            let (size, origin_x) = (
                self.session_card.size.clone(),
                self.session_card.origin_x.clone(),
            );
            let (compact, ticker) = (self.session_card.compact, self.session_card.ticker.take());
            // Keep per-chat caches across leave/return so a revisit paints
            // the right git line and todo card on the first frame.
            let git_by_chat = std::mem::take(&mut self.session_card.git_by_chat);
            let steps_by_chat = std::mem::take(&mut self.session_card.steps_by_chat);
            self.session_card = SessionCard {
                chat: self.current_chat.clone(),
                size,
                origin_x,
                compact,
                ticker,
                git_by_chat,
                steps_by_chat,
                ..SessionCard::default()
            };
            if let Some(id) = self.session_card.chat.clone() {
                if let Some(g) = self.session_card.git_by_chat.get(&id).cloned() {
                    self.session_card.git = g;
                }
                if let Some(cached) = self.session_card.steps_by_chat.get(&id).cloned() {
                    self.session_card.held_steps = cached;
                    self.session_card.held_steps_chat = Some(id.clone());
                }
                self.session_card.steps_pending = true;
            }
        }
        self.card_refresh_git(cx);
        self.card_ensure_ticker(cx);
    }

    /// Steps under the card this frame. Chat switches are frequent: never
    /// collapse the todo card then expand it. Until the new chat's messages
    /// settle we keep the previous paint, or swap from a per-chat cache on
    /// the first frame when we have one. Height then changes at most once.
    fn card_task_steps(&mut self) -> Option<crate::steps::Steps> {
        let Some(chat) = self.current_chat.clone() else {
            self.session_card.held_steps = None;
            self.session_card.held_steps_chat = None;
            self.session_card.steps_pending = false;
            return None;
        };
        let live = self.current_task_steps();
        let settled = self.chat_reveal.is_none();
        // Reveal can finish a frame before history rows are applied. Only
        // treat "no steps" as final once messages exist, or the chat has
        // never had any (nothing to load).
        let history_ready = !self.msg_order.is_empty()
            || self
                .state
                .chats
                .get(&chat)
                .is_some_and(|c| c.last_message_at == 0);
        let empty_confirmed = settled && history_ready;

        if self.session_card.steps_pending {
            if let Some(steps) = live {
                self.session_card
                    .steps_by_chat
                    .insert(chat.clone(), Some(steps.clone()));
                self.session_card.held_steps = Some(steps.clone());
                self.session_card.held_steps_chat = Some(chat);
                self.session_card.steps_pending = false;
                return Some(steps);
            }
            if empty_confirmed {
                self.session_card.steps_by_chat.insert(chat.clone(), None);
                self.session_card.held_steps = None;
                self.session_card.held_steps_chat = Some(chat);
                self.session_card.steps_pending = false;
                return None;
            }
            if let Some(cached) = self.session_card.steps_by_chat.get(&chat).cloned() {
                self.session_card.held_steps = cached.clone();
                self.session_card.held_steps_chat = Some(chat);
                return cached;
            }
            return self.session_card.held_steps.clone();
        }

        if let Some(steps) = live {
            self.session_card
                .steps_by_chat
                .insert(chat.clone(), Some(steps.clone()));
            self.session_card.held_steps = Some(steps.clone());
            self.session_card.held_steps_chat = Some(chat);
            return Some(steps);
        }
        if empty_confirmed && self.session_card.held_steps_chat.as_deref() == Some(chat.as_str()) {
            self.session_card.steps_by_chat.insert(chat, None);
            self.session_card.held_steps = None;
        }
        self.session_card.held_steps.clone()
    }

    /// Something may have changed the working tree: ask again shortly
    /// (bursts of changes make one request).
    pub(super) fn card_poke(&mut self, cx: &mut Context<Self>) {
        if self.session_card.chat.is_none() {
            return;
        }
        self.session_card.debounce += 1;
        let n = self.session_card.debounce;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| {
                if this.session_card.debounce == n {
                    this.card_refresh_git(cx);
                }
            });
        })
        .detach();
    }

    pub(super) fn card_on_turn_end(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        if self.session_card.chat.as_deref() == Some(chat_id) {
            self.card_poke(cx);
        }
    }

    /// The engine saw files change in a checkout (an agent's edit, among others).
    pub(super) fn card_on_checkout(&mut self, checkout_id: &str, cx: &mut Context<Self>) {
        let ours = self
            .session_card
            .chat
            .as_ref()
            .and_then(|id| self.state.chats.get(id))
            .is_some_and(|c| c.checkout_id == checkout_id);
        if ours {
            self.card_poke(cx);
        }
    }

    /// The chat's folder, unless it's a scratch / no-project chat.
    fn card_folder(&self) -> Option<(String, String)> {
        let chat = self
            .session_card
            .chat
            .as_ref()
            .and_then(|id| self.state.chats.get(id))?;
        if self.state.is_scratch_chat(chat) {
            return None;
        }
        let space = self.state.spaces.get(&chat.space_id)?;
        (!self.state.is_scratch_space(space)).then(|| (space.id.clone(), space.name.clone()))
    }

    /// One request at a time; one asked for meanwhile runs when it lands.
    fn card_refresh_git(&mut self, cx: &mut Context<Self>) {
        let (Some(chat), Some(client)) = (self.session_card.chat.clone(), self.client.clone())
        else {
            return;
        };
        if self.card_folder().is_none() {
            self.session_card.git = Git::NoRepo;
            return;
        }
        if self.session_card.fetching {
            self.session_card.again = true;
            return;
        }
        self.session_card.fetching = true;
        self.session_card.fetched_at = Some(Instant::now());
        cx.spawn(async move |this, cx| {
            let res = client
                .call("chat_git_summary", serde_json::json!({ "chat_id": chat }))
                .await;
            let _ = this.update(cx, |this, cx| {
                let card = &mut this.session_card;
                card.fetching = false;
                if card.chat.as_deref() == Some(chat.as_str()) {
                    let git = match res {
                        Ok(v) => parse_git(&v),
                        Err(err) => {
                            tracing::debug!(target: "keel_ui::card", "chat_git_summary: {err}");
                            Git::NoRepo
                        }
                    };
                    if !matches!(git, Git::Unknown) {
                        card.git_by_chat.insert(chat.clone(), git.clone());
                    }
                    if git != card.git {
                        card.git = git;
                        cx.notify();
                    }
                }
                if std::mem::take(&mut this.session_card.again) {
                    this.card_refresh_git(cx);
                }
            });
        })
        .detach();
    }

    /// A one-second tick: elapsed times move while rows show, a refusal
    /// notice expires, and the git poll runs while the window is active.
    fn card_ensure_ticker(&mut self, cx: &mut Context<Self>) {
        if self.session_card.ticker.is_some() {
            return;
        }
        self.session_card.ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let alive = this.update(cx, |this, cx| {
                    let card = &mut this.session_card;
                    if card.chat.is_none() || card.chat != this.current_chat {
                        return;
                    }
                    if card
                        .notice
                        .as_ref()
                        .is_some_and(|(_, at)| at.elapsed() >= NOTICE_HOLD)
                    {
                        card.notice = None;
                        cx.notify();
                    }
                    if !card.shown.is_empty() || card.browser.is_some() {
                        cx.notify();
                    }
                    if this.window_active
                        && !card.fetching
                        && card.fetched_at.is_none_or(|at| at.elapsed() >= POLL)
                    {
                        this.card_refresh_git(cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        }));
    }

    /// How far down from the transcript's top the card starts: under the
    /// timer pill when that rests in the same corner.
    fn card_top(&self) -> f32 {
        match self.timer_top_right_reach() {
            Some(reach) => reach + f32::from(zpx(TIMER_GAP)),
            None => f32::from(zpx(EDGE)),
        }
    }

    /// How folded the card is now (0 full, 1 pill), following the
    /// transcript's width with hysteresis and gliding between the two.
    fn card_compactness(&mut self, now: Instant, window: &mut Window) -> f32 {
        let reduced = self.reduced();
        let z = |v: f32| f32::from(zpx(v));
        let need = f32::from(self.column_max()).min(z(MIN_COLUMN));

        // Mid-slide the mode was settled when the slide began (`card_room`);
        // the card only finishes its glide, the column doesn't follow it.
        if self.right_panel_toggling() || self.sidebar_toggling() || self.browser_expanding() {
            let card = &mut self.session_card;
            if card
                .mode_at
                .is_some_and(|at| now.saturating_duration_since(at) >= MODE_GLIDE)
            {
                card.mode_at = None;
            }
            if card.mode_at.is_some() {
                window.request_animation_frame();
            }
            return mode_now(card, now);
        }

        let pane_w = self.pane_width(window);
        let card = &mut self.session_card;
        let current = |card: &SessionCard| {
            let target = if card.compact { 1. } else { 0. };
            match card.mode_at {
                Some(at) => {
                    let p = (now.saturating_duration_since(at).as_secs_f32()
                        / MODE_GLIDE.as_secs_f32())
                    .min(1.);
                    card.mode_from + (target - card.mode_from) * ease_out_strong(p)
                }
                None => target,
            }
        };
        let w = pane_w.unwrap_or_else(|| {
            let (w, _) = card.size.get();
            if w > 0. {
                w
            } else {
                f32::from(window.viewport_size().width)
            }
        });
        if w > 0. {
            // The column box beside the card at its tightest (card at EDGE).
            let avail = w - z(EDGE + CARD_W + COLUMN_GAP - ROW_INSET);
            let next = next_compact(card.compact, avail, need);
            if next != card.compact {
                card.mode_from = current(card);
                card.compact = next;
                card.mode_at = (!reduced).then_some(now);
                card.slide_mode = false;
                card.cut_out = false;
            }
        }
        if card
            .mode_at
            .is_some_and(|at| now.saturating_duration_since(at) >= MODE_GLIDE)
        {
            card.mode_at = None;
            card.slide_mode = false;
            card.cut_out = false;
        }
        if card.mode_at.is_some() {
            window.request_animation_frame();
        }
        current(card)
    }

    #[allow(clippy::too_many_arguments)]
    fn calc_card_room_endpoints(
        viewport_w: f32,
        full_sidebar_w: f32,
        browser_w: f32,
        open: f32,
        col: f32,
        min_left: f32,
        k: f32,
        card_top: f32,
    ) -> (f32, f32, f32) {
        let w0 = (viewport_w - browser_w).max(1.);
        let w1 = (viewport_w - full_sidebar_w - browser_w).max(1.);

        let z = |v: f32| f32::from(zpx(v));

        let (l0_full, r0_full, _) = full_layout(
            w0,
            col,
            z(CARD_W),
            z(COLUMN_GAP - ROW_INSET),
            z(EDGE),
            min_left,
        );
        let (l1_full, r1_full, _) = full_layout(
            w1,
            col,
            z(CARD_W),
            z(COLUMN_GAP - ROW_INSET),
            z(EDGE),
            min_left,
        );

        let (l0_pill, r0_pill, _) = pill_layout(w0, col, min_left);
        let (l1_pill, r1_pill, _) = pill_layout(w1, col, min_left);

        let full_left = l0_full + (l1_full - l0_full) * open;
        let full_right = r0_full + (r1_full - r0_full) * open;
        let pill_left = l0_pill + (l1_pill - l0_pill) * open;
        let pill_right = r0_pill + (r1_pill - r0_pill) * open;

        let strip = (card_top + z(PILL_H + 8.) - 24.).max(0.);
        let room_l = full_left * (1. - k) + pill_left * k;
        let room_r = full_right * (1. - k) + pill_right * k;
        (room_l, room_r, strip * k)
    }

    /// Room the transcript keeps for the card: in full mode the reading
    /// region's left and right padding ([`full_layout`]: the card pinned top
    /// right, the column centered in the available reading region), under the pill a top
    /// strip. Mid-glide, in between (eased, so the column, the composer and
    /// the card slide together). Returns (left, right, strip).
    pub(super) fn card_room(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (f32, f32, f32) {
        self.card_sync_chat(cx);
        if self.session_card.chat.is_none() {
            (self.session_card.room_l, self.session_card.room_x) = (0., 0.);
            self.column_slide = None;
            return (0., 0., 0.);
        }
        let z = |v: f32| f32::from(zpx(v));
        let viewport_w = f32::from(window.viewport_size().width);
        let full_sidebar_w = self.state.sidebar_width();
        let col = f32::from(self.column_max());
        let min_left = z(SIDEBAR_CHAT_GAP);
        let card_top = self.card_top();
        // The right panel's in-flow room at rest (its spacer takes its end
        // width when a toggle starts; the pane slides over it).
        let right_w = self.chat_right_room(window);
        let now = Instant::now();

        if self.sidebar_toggling() || self.right_panel_toggling() {
            let key = (
                self.sidebar_anim.map_or(0, |(_, n)| n),
                self.right_panel_anim.map_or(0, |(_, n)| n),
            );
            if self.column_slide.map(|s| s.key) != Some(key) {
                let right_moves = self.right_panel_toggling()
                    && self.column_slide.map(|s| s.key.1) != Some(key.1);
                // Where it lands: the settled layout once the slide is over,
                // card mode included (decided now, with the same hysteresis).
                let side_end = full_sidebar_w * self.sidebar_target();
                let end_w = (viewport_w - side_end - right_w).max(1.);
                let overhead = z(EDGE + CARD_W + COLUMN_GAP - ROW_INSET);
                let compact = next_compact(
                    self.session_card.compact,
                    end_w - overhead,
                    col.min(z(MIN_COLUMN)),
                );
                let reduced = self.reduced();
                let card = &mut self.session_card;
                if compact != card.compact {
                    card.mode_from = mode_now(card, now);
                    card.compact = compact;
                    card.mode_at = (!reduced).then_some(now);
                    card.cut_out = right_moves;
                }
                card.slide_mode = card.mode_at.is_some();
                let k = if compact { 1. } else { 0. };
                let (l, r, strip) = Self::calc_card_room_endpoints(
                    viewport_w,
                    full_sidebar_w,
                    right_w,
                    self.sidebar_target(),
                    col,
                    min_left,
                    k,
                    card_top,
                );
                let to_w = (end_w - l - r).max(1.);
                let to = ColumnGeo {
                    x: side_end + l,
                    w: to_w,
                    strip,
                    dock_w: to_w,
                };
                let mut from = if self.column_last.w > 0. {
                    self.column_last
                } else {
                    to
                };
                if from.dock_w <= 0. {
                    from.dock_w = from.w;
                }
                self.column_slide = Some(ColumnSlide { key, from, to });
            }
            let Some(slide) = self.column_slide else {
                return (0., 0., 0.);
            };
            let p = self.slide_progress();
            let lerp = |a: f32, b: f32| a + (b - a) * p;
            let pane_left = full_sidebar_w * self.sidebar_open_now();
            let pane_w = viewport_w - pane_left - right_w;
            let room_l = (lerp(slide.from.x, slide.to.x) - pane_left).max(0.);
            let room_r = (pane_w - room_l - slide.to.w).max(0.);
            let strip = lerp(slide.from.strip, slide.to.strip).max(0.);
            let dock_w = lerp(slide.from.dock_w, slide.to.dock_w).max(1.);
            self.column_last = ColumnGeo {
                x: pane_left + room_l,
                w: slide.to.w,
                strip,
                dock_w,
            };
            let card = &mut self.session_card;
            card.room_l = room_l;
            card.room_x = room_r;
            return (room_l, room_r, strip);
        }
        self.column_slide = None;

        let k = self.card_compactness(now, window);
        // After a slide the column is already at its end room; the card's
        // glide finishing must not drag it.
        let card = &self.session_card;
        let k_room = if card.slide_mode && card.mode_at.is_some() {
            if card.compact { 1. } else { 0. }
        } else {
            k
        };
        let open = self.sidebar_open_now();
        let (room_l, room_r, strip) = Self::calc_card_room_endpoints(
            viewport_w,
            full_sidebar_w,
            right_w,
            open,
            col,
            min_left,
            k_room,
            card_top,
        );
        let pane_left = full_sidebar_w * open;
        let w = (viewport_w - pane_left - right_w - room_l - room_r).max(0.);
        self.column_last = ColumnGeo {
            x: pane_left + room_l,
            w,
            strip,
            dock_w: w,
        };
        let card = &mut self.session_card;
        card.room_l = room_l;
        card.room_x = room_r;
        (room_l, room_r, strip)
    }

    /// The right panel's room beside the chat at rest: none when shut.
    fn chat_right_room(&self, window: &Window) -> f32 {
        if !self.browser_allowed() || !self.right_panel_open() {
            0.
        } else {
            self.browser_width(window)
        }
    }

    /// The reading region's (left, right) padding this frame. The composer
    /// (and the "Latest" pill) keep the same, so their edges line up with
    /// the transcript's text.
    pub(super) fn card_column_room(&self) -> (f32, f32) {
        if self.current_chat.is_some() && self.session_card.chat == self.current_chat {
            (self.session_card.room_l, self.session_card.room_x)
        } else {
            (0., 0.)
        }
    }

    /// The target reading region (left, right) padding for the chat session,
    /// available even during chat creation or hero exit before the session card has mounted.
    pub(super) fn target_card_column_room(&self, window: &Window) -> (f32, f32) {
        let (room_l, room_r) = self.card_column_room();
        if room_l > 0. || room_r > 0. {
            return (room_l, room_r);
        }
        let z = |v: f32| f32::from(zpx(v));
        let viewport_w = f32::from(window.viewport_size().width);
        let full_sidebar_w = self.state.sidebar_width();
        let col = f32::from(self.column_max());
        let min_left = z(SIDEBAR_CHAT_GAP);
        let card_top = self.card_top();
        let right_w = self.chat_right_room(window);
        let side_end = full_sidebar_w * self.sidebar_target();
        let avail_w = (viewport_w - side_end - right_w).max(1.);
        let overhead = z(EDGE + CARD_W + COLUMN_GAP - ROW_INSET);
        let compact = next_compact(
            self.session_card.compact,
            avail_w - overhead,
            col.min(z(MIN_COLUMN)),
        );
        let k = if compact { 1. } else { 0. };
        let (room_l, room_r, _) = Self::calc_card_room_endpoints(
            viewport_w,
            full_sidebar_w,
            right_w,
            self.sidebar_target(),
            col,
            min_left,
            k,
            card_top,
        );
        (room_l, room_r)
    }

    /// A wheel at window `x` lands in the margin beside the reading region
    /// (where the list doesn't reach): the transcript scrolls it by hand,
    /// so the wheel still works anywhere outside the card.
    pub(super) fn card_wheel_outside_column(&self, x: f32) -> bool {
        let (left, right) = self.card_column_room();
        let (w, _) = self.session_card.size.get();
        let x = x - self.session_card.origin_x.get();
        (left > 0. && x < left) || (right > 0. && x > w - right)
    }

    /// Records the transcript's size for the next frame's layout.
    pub(super) fn card_measure(&self) -> AnyElement {
        let size = self.session_card.size.clone();
        let origin_x = self.session_card.origin_x.clone();
        gpui::canvas(
            move |bounds, window, _| {
                origin_x.set(f32::from(bounds.origin.x));
                let next = (f32::from(bounds.size.width), f32::from(bounds.size.height));
                let prev = size.get();
                if (prev.0 - next.0).abs() > 0.5 || (prev.1 - next.1).abs() > 0.5 {
                    size.set(next);
                    window.refresh();
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0()
        .into_any_element()
    }

    fn card_fade(&mut self, key: &'static str, value: String, now: Instant) -> Fade {
        let animate = !self.reduced();
        let fade = self.session_card.fades.entry(key).or_default();
        fade.set(&value, now, animate);
        fade.clone()
    }

    /// A value that crossfades when it changes.
    fn fade_el(fade: &Fade, now: Instant, window: &mut Window) -> AnyElement {
        match fade.progress(now) {
            None => div().child(fade.now.clone()).into_any_element(),
            Some(p) => {
                window.request_animation_frame();
                div()
                    .relative()
                    .child(div().opacity(p).child(fade.now.clone()))
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .right_0()
                            .whitespace_nowrap()
                            .opacity(1. - p)
                            .child(fade.prev.clone()),
                    )
                    .into_any_element()
            }
        }
    }

    /// The process rows this frame, with entering and leaving ones tracked.
    fn card_proc_rows(&mut self, now: Instant, now_ms: i64) -> Vec<ProcRow> {
        let chat = self.session_card.chat.clone().unwrap_or_default();
        let activity = self.browser_activity(&chat);
        let card = &mut self.session_card;
        card.browser = match (activity, card.browser.take()) {
            (Some(label), Some((_, since, _))) => Some((label, since, now)),
            (Some(label), None) => Some((label, now_ms, now)),
            (None, Some(prev)) if now.saturating_duration_since(prev.2) < BROWSER_GRACE => {
                Some(prev)
            }
            (None, _) => None,
        };
        let browser = card
            .browser
            .as_ref()
            .map(|(label, since, _)| (label.clone(), *since));
        let rows = process_rows(&chat, self.subagents.values(), browser);
        let card = &mut self.session_card;
        // Rows there when the chat opened arrive with the card, not one by one.
        let settled = now.checked_sub(crate::motion::ROW_ENTER).unwrap_or(now);
        let opening = card.opened_at.elapsed() < ENTER;
        for row in &rows {
            card.entered
                .entry(row.key.clone())
                .or_insert(if opening { settled } else { now });
        }
        for old in std::mem::take(&mut card.shown) {
            if !rows.iter().any(|r| r.key == old.key) {
                card.entered.remove(&old.key);
                card.leaving.push((old, now));
            }
        }
        card.leaving.retain(|(row, at)| {
            now.saturating_duration_since(*at) < crate::motion::ROW_EXIT
                && !rows.iter().any(|r| r.key == row.key)
        });
        card.shown = rows.clone();
        rows
    }

    fn card_stop(&mut self, stop: &Stop, cx: &mut Context<Self>) {
        if let Stop::Browser { chat } = stop {
            tracing::info!(target: "keel_ui::card", "browser agent stopped from the session card");
            self.on_browser_cancel(String::new(), chat.clone(), cx);
            self.session_card.browser = None;
            cx.notify();
        }
    }

    /// Why the branch can't be switched right now, if it can't: a switch
    /// in flight, or an agent mid-turn in this checkout (this chat, or
    /// another sharing the folder).
    fn card_switch_blocked(&self) -> Option<&'static str> {
        if self.session_card.switching.is_some() {
            return Some(BRANCH_SWITCHING);
        }
        let chat = self
            .session_card
            .chat
            .as_ref()
            .and_then(|id| self.state.chats.get(id))?;
        let busy =
            |status: ChatStatus| matches!(status, ChatStatus::Running | ChatStatus::AwaitingInput);
        if busy(chat.status) {
            return Some(BRANCH_BUSY);
        }
        let other = !chat.checkout_id.is_empty()
            && self.state.chats.values().any(|o| {
                o.id != chat.id
                    && !o.archived
                    && !o.tombstoned
                    && o.checkout_id == chat.checkout_id
                    && busy(o.status)
            });
        other.then_some(BRANCH_BUSY_OTHER)
    }

    /// Opens the branch picker on this chat's own checkout.
    pub(super) fn card_open_branches(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.card_switch_blocked().is_some() {
            return;
        }
        self.open_picker_with_search(Picker::Branch, window, cx);
        if self.open_picker() == Some(Picker::Branch) {
            self.session_card.picking = true;
            self.session_card.notice = None;
            self.card_load_branches(cx);
        }
    }

    /// Another menu opened (or the hero's branch chip): the branch menu
    /// lists the new-chat folder again.
    pub(super) fn card_end_picking(&mut self) {
        self.session_card.picking = false;
    }

    /// The branch menu's listing while the card opened it.
    pub(super) fn card_branch_menu(&self) -> Option<CardBranches> {
        self.session_card.picking.then(|| {
            self.session_card.branches.clone().unwrap_or(CardBranches {
                loading: true,
                ..Default::default()
            })
        })
    }

    /// Fetch this chat's branches. The checked-out one shows at once (from
    /// the card's git line) while the listing loads.
    fn card_load_branches(&mut self, cx: &mut Context<Self>) {
        let (Some(chat), Some(client)) = (self.session_card.chat.clone(), self.client.clone())
        else {
            return;
        };
        let card = &mut self.session_card;
        card.branch_gen += 1;
        let generation = card.branch_gen;
        let known = match &card.git {
            Git::Repo(info) => info.branch.clone(),
            _ => None,
        };
        let list = card.branches.get_or_insert_with(Default::default);
        if list.branches.is_empty()
            && let Some(name) = known
        {
            list.checked_out(&name);
        }
        list.loading = list.branches.is_empty();
        cx.spawn(async move |this, cx| {
            let res = client
                .call("chat_branches", serde_json::json!({ "chat_id": chat }))
                .await;
            let _ = this.update(cx, |this, cx| {
                let card = &mut this.session_card;
                if card.branch_gen != generation || card.chat.as_deref() != Some(chat.as_str()) {
                    return;
                }
                card.branches = Some(match res {
                    Ok(v) => CardBranches::from_reply(&v),
                    Err(err) => {
                        tracing::debug!(target: "keel_ui::card", "chat_branches: {err}");
                        CardBranches::default()
                    }
                });
                cx.notify();
            });
        })
        .detach();
    }

    /// A branch picked in the card's menu: check it out in this chat's
    /// folder (or worktree). The engine refuses on uncommitted changes or a
    /// running turn; the reason shows under the branch.
    pub(super) fn card_pick_branch(&mut self, branch: String, cx: &mut Context<Self>) {
        self.set_picker(None, cx);
        let current = self
            .session_card
            .branches
            .as_ref()
            .and_then(|b| b.current.clone());
        if current.as_deref() == Some(branch.as_str()) {
            return;
        }
        if let Some(why) = self.card_switch_blocked() {
            self.session_card.notice = Some((why.to_string(), Instant::now()));
            cx.notify();
            return;
        }
        let (Some(chat), Some(client)) = (self.session_card.chat.clone(), self.client.clone())
        else {
            return;
        };
        self.session_card.switching = Some(branch.clone());
        self.session_card.notice = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = client.call("chat_checkout_branch", serde_json::json!({ "chat_id": chat, "branch": branch })).await;
            let _ = this.update(cx, |this, cx| {
                // The folder's listing in the new-chat cache is stale now.
                this.branch_cache.clear();
                let card = &mut this.session_card;
                if card.chat.as_deref() != Some(chat.as_str()) {
                    return;
                }
                card.switching = None;
                match res {
                    Ok(_) => {
                        tracing::info!(target: "keel_ui::card", "chat branch switched from the session card");
                        if let Some(list) = card.branches.as_mut() {
                            list.checked_out(&branch);
                        }
                    }
                    Err(err) => card.notice = Some((err, Instant::now())),
                }
                this.card_refresh_git(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// The card (or the pill), placed over the transcript's top right.
    pub(super) fn render_session_card(
        &mut self,
        t: Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let chat_id = self.session_card.chat.clone()?;
        let now = Instant::now();
        let now_ms = crate::trace_view::now_ms();
        let reduced = self.reduced();
        let k = self.card_compactness(now, window);
        let enter = (now
            .saturating_duration_since(self.session_card.opened_at)
            .as_secs_f32()
            / ENTER.as_secs_f32())
        .min(1.);
        if enter < 1. {
            window.request_animation_frame();
        }
        let enter = ease_out_strong(enter);
        // Reduced motion keeps the fade and drops the drop.
        let drop = if reduced {
            0.
        } else {
            -ENTER_DROP * (1. - enter)
        };
        let usage = self
            .state
            .chats
            .get(&chat_id)
            .map(|c| c.usage.clone())
            .unwrap_or_default();
        let fill = usage.fill();
        let ring_to = fill.unwrap_or(0.);
        let ring_now = match self.session_card.ring {
            Some(r) if (r.to - ring_to).abs() < 1e-4 => r.at(now),
            prev => {
                let from = if reduced {
                    ring_to
                } else {
                    prev.map_or(0., |r| r.at(now))
                };
                let r = Ring {
                    from,
                    to: ring_to,
                    at: now,
                };
                self.session_card.ring = Some(r);
                r.at(now)
            }
        };
        if !reduced
            && self
                .session_card
                .ring
                .is_some_and(|r| now.saturating_duration_since(r.at) < SWEEP)
        {
            window.request_animation_frame();
        }
        let rows = self.card_proc_rows(now, now_ms);
        let open = self.open_picker() == Some(Picker::Branch) && self.session_card.picking;
        let menu = if self.session_card.picking {
            self.menu_phase(Picker::Branch.menu_id(), open, cx)
                .map(|leaving| self.render_picker_menu(Picker::Branch, leaving, false, t, cx))
        } else {
            None
        };
        let git = self.session_card.git.clone();
        let folder = self.card_folder();

        let top = self.card_top();
        let (_, h) = self.session_card.size.get();
        let max_h = (h - top - f32::from(zpx(16.))).max(f32::from(zpx(80.)));
        // The picker hangs under whichever form is (mostly) showing.
        let (mut full_menu, mut pill_menu) = (None, None);
        if k < 0.5 {
            full_menu = menu
        } else {
            pill_menu = menu
        }
        // The wheel over the card scrolls the card (when it overflows),
        // never the transcript under it.
        let swallow = |d: gpui::Div| {
            d.on_scroll_wheel(|_: &gpui::ScrollWheelEvent, _, cx: &mut gpui::App| {
                cx.stop_propagation()
            })
        };
        // Cut: the outgoing form is gone at once, the incoming one glides.
        let cut = self.session_card.cut_out && self.session_card.mode_at.is_some();
        let (full_k, pill_k) = match (cut, self.session_card.compact) {
            (true, true) => (1., k),
            (true, false) => (k, 0.),
            _ => (k, k),
        };
        self.geo_trace
            .card_look
            .set((k, enter * (1. - full_k), enter * pill_k));
        let task_steps = self.card_task_steps();
        let git_k = match &self.session_card.git {
            Git::Unknown => 0.,
            Git::NoRepo => 1.,
            Git::Repo(_) => 2.,
        };
        self.geo_trace.card_meta.set((
            if self.session_card.steps_pending {
                1.
            } else {
                0.
            },
            if self.session_card.held_steps.is_some() {
                1.
            } else {
                0.
            },
            git_k,
        ));
        let mut layers: Vec<AnyElement> = Vec::new();
        if full_k < 1. {
            let card = self.render_card_full(
                &chat_id,
                &usage,
                ring_now,
                &git,
                folder.clone(),
                &rows,
                full_menu,
                now,
                now_ms,
                t,
                window,
                cx,
            );
            let mut col = swallow(div())
                .id("session-card-column")
                .absolute()
                .top(px(top) + zpx(drop - 4. * k))
                .right(zpx(EDGE))
                .w(zpx(CARD_W))
                .max_h(px(max_h))
                .pb(zpx(2.))
                .flex()
                .flex_col()
                .gap(zpx(10.))
                .overflow_y_scroll()
                .opacity(enter * (1. - full_k))
                .children(super::dev::GeoTrace::probe(&self.geo_trace, |g| &g.card))
                .child(card);

            if let Some(steps) = &task_steps {
                col = col.child(
                    div()
                        .relative()
                        .children(super::dev::GeoTrace::probe(&self.geo_trace, |g| &g.todo))
                        .child(self.render_task_steps_card(steps, t)),
                );
            }

            layers.push(col.into_any_element());
        }
        if pill_k > 0. {
            let pill = self.render_card_pill(
                ring_now,
                &usage,
                &git,
                folder.is_some(),
                rows.len(),
                pill_menu,
                t,
                cx,
            );
            layers.push(
                swallow(div())
                    .absolute()
                    .top(px(top) + zpx(drop + 4. * (1. - k)))
                    .right(zpx(EDGE))
                    .opacity(enter * pill_k)
                    .children(super::dev::GeoTrace::probe(&self.geo_trace, |g| &g.pill))
                    .child(pill)
                    .into_any_element(),
            );
        }
        Some(
            div()
                .absolute()
                .inset_0()
                .children(layers)
                .into_any_element(),
        )
    }

    fn render_connected_tree_repo(
        &self,
        repo: &RepoTreeItem,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let text = c(t.text);
        let muted = c(t.text_muted);
        let medium = gpui::FontWeight::MEDIUM;
        let spine_color = alpha(c(t.border), 0.85);

        let is_open = !self.session_card.closed_repos.contains(&repo.id);
        let is_expanded = self.session_card.expanded_more.contains(&repo.id);
        let sample_cap = if is_expanded { 12 } else { CARD_FILE_ROWS };
        let visible_files: Vec<&GitFileChange> =
            repo.file_details.iter().take(sample_cap).collect();
        let more_count = (repo.files_count as usize).saturating_sub(visible_files.len());
        let show_more_pill = is_expanded || more_count > 0;

        let is_copied = self
            .session_card
            .branch_copied
            .as_ref()
            .is_some_and(|(id, at)| id == &repo.id && at.elapsed() < Duration::from_millis(1600));
        let tip_text: String = if is_copied {
            "Copied!".to_string()
        } else {
            repo.branch.clone()
        };

        // Header: Repo Icon, Name, Middle Dot, Branch Trigger, Leader Line, Diff Badge
        let mut header = div()
            .id(ElementId::Name(
                format!("tree-repo-header-{}", repo.id).into(),
            ))
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(4.))
            .w_full()
            .min_w_0()
            .py(zpx(1.5))
            .px(zpx(3.))
            .rounded(RADIUS_MD)
            .when(repo.files_count > 0, |d| {
                d.cursor_pointer()
                    .hover_fade(alpha(c(t.hover), 0.5), None)
                    .on_click(cx.listener({
                        let repo_id = repo.id.clone();
                        move |this, _: &ClickEvent, _, cx| {
                            if this.session_card.closed_repos.contains(&repo_id) {
                                this.session_card.closed_repos.remove(&repo_id);
                            } else {
                                this.session_card.closed_repos.insert(repo_id.clone());
                            }
                            cx.notify();
                        }
                    }))
            });

        // Node icon (amber rounded square)
        header = header.child(
            div()
                .flex_none()
                .text_color(c(t.warning))
                .child(Icon::RepoNode.el(zpx(12.5))),
        );

        // Repo name
        header = header.child(
            div()
                .flex_none()
                .t_small()
                .font_weight(medium)
                .text_color(text)
                .child(repo.name.clone()),
        );

        // Middle dot
        if !repo.branch.is_empty() {
            header = header.child(
                div()
                    .flex_none()
                    .t_caption()
                    .text_color(c(t.text_faint))
                    .child("·"),
            );

            // Branch anchor
            let branch_btn = div()
                .id(ElementId::Name(format!("card-branch-{}", repo.id).into()))
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(3.))
                .px(zpx(5.))
                .py(zpx(1.5))
                .max_w(zpx(130.))
                .rounded(RADIUS_SM)
                .border_1()
                .border_color(c(t.border).opacity(0.5))
                .bg(alpha(c(t.hover), 0.3))
                .cursor_pointer()
                .hover(|s| s.border_color(c(t.border)))
                .hover_fade(c(t.hover), None)
                .child(
                    div()
                        .flex_none()
                        .text_color(muted)
                        .child(Icon::Branch.el(zpx(10.))),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_family(MONO)
                        .text_size(px(10.5))
                        .text_color(if is_copied { c(t.accent) } else { text })
                        .truncate()
                        .child(if is_copied {
                            "Copied!".to_string()
                        } else {
                            repo.branch_display.clone()
                        }),
                )
                .tooltip(move |_, cx| cx.new(|_| Tip::new(tip_text.clone(), t)).into())
                .tooltip_show_delay(crate::view::tooltip::delay())
                .on_click(cx.listener({
                    let branch = repo.branch.clone();
                    let repo_id = repo.id.clone();
                    move |this, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        this.session_card.branch_copied = Some((repo_id.clone(), Instant::now()));
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(branch.clone()));
                        let repo_id_spawn = repo_id.clone();
                        cx.spawn(async move |this, cx| {
                            cx.background_executor()
                                .timer(Duration::from_millis(1600))
                                .await;
                            let _ = this.update(cx, |this, cx| {
                                if this
                                    .session_card
                                    .branch_copied
                                    .as_ref()
                                    .is_some_and(|(id, _)| id == &repo_id_spawn)
                                {
                                    this.session_card.branch_copied = None;
                                    cx.notify();
                                }
                            });
                        })
                        .detach();
                        cx.notify();
                    }
                }));

            header = header.child(branch_btn);
        }

        // Dotted leader line and diff stat badge (only shown when there are changed files)
        if repo.files_count > 0 {
            header = header.child(leader_line(c(t.border).opacity(0.6)));

            let badge_tip = if repo.total_additions > 0 || repo.total_deletions > 0 {
                format!(
                    "{} files changed · +{} / -{}",
                    repo.files_count, repo.total_additions, repo.total_deletions
                )
            } else {
                format!("{} files changed", repo.files_count)
            };

            let mut b = div()
                .id(ElementId::Name(
                    format!("tree-repo-badge-{}", repo.id).into(),
                ))
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(3.))
                .px(zpx(5.))
                .py(zpx(1.))
                .rounded(RADIUS_SM)
                .bg(c(t.code).opacity(0.45))
                .font_family(MONO)
                .text_size(px(10.5));

            if let Some(bar) = mini_diff_bar(repo.total_additions, repo.total_deletions, t) {
                b = b.child(bar);
            }
            if repo.total_additions > 0 || repo.total_deletions > 0 {
                if repo.total_additions > 0 {
                    b = b.child(
                        div()
                            .text_color(c(t.success))
                            .font_weight(medium)
                            .child(format!("+{}", repo.total_additions)),
                    );
                }
                if repo.total_deletions > 0 {
                    b = b.child(
                        div()
                            .text_color(c(t.danger))
                            .font_weight(medium)
                            .child(format!("-{}", repo.total_deletions)),
                    );
                }
            } else {
                b = b.child(
                    div()
                        .text_color(muted)
                        .child(format!("{} changed", repo.files_count)),
                );
            }
            let badge = b
                .tooltip(move |_, cx| cx.new(|_| Tip::new(badge_tip.clone(), t)).into())
                .tooltip_show_delay(crate::view::tooltip::delay())
                .into_any_element();
            header = header.child(badge);
        }

        let mut group_el = div()
            .flex()
            .flex_col()
            .gap(zpx(2.))
            .w_full()
            .min_w_0()
            .child(header);

        // Connected Tree Spine Children: only shown when there are changed files
        if is_open && repo.files_count > 0 && (!visible_files.is_empty() || show_more_pill) {
            let mut children_col = div()
                .flex()
                .flex_col()
                .gap(zpx(1.))
                .w_full()
                .min_w_0()
                .pl(zpx(10.));

            for (ix, file) in visible_files.iter().enumerate() {
                let is_last = !show_more_pill && (ix + 1 == visible_files.len());
                let group_id = SharedString::from(format!("tree-file-{}-{ix}", repo.id));
                let file_target = if repo.target_prefix.is_empty() {
                    file.path.clone()
                } else {
                    format!("{}/{}", repo.target_prefix, file.path)
                };

                let (bg_color, text_color) = match file.status {
                    'A' => (alpha(c(t.success), 0.14), c(t.success)),
                    'D' => (alpha(c(t.danger), 0.14), c(t.danger)),
                    _ => (alpha(c(t.warning), 0.14), c(t.warning)),
                };

                let (dir, base) = match file.path.rfind('/') {
                    Some(idx) => (&file.path[..=idx], &file.path[idx + 1..]),
                    None => ("", file.path.as_str()),
                };

                let status_char = match file.status {
                    '?' => 'U',
                    other => other,
                };

                let item = div()
                    .id(ElementId::Name(
                        format!("tree-item-{}-{ix}", repo.id).into(),
                    ))
                    .hover_probe(group_id.clone())
                    .group(group_id.clone())
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(zpx(23.))
                    .child(tree_connector(is_last, spine_color))
                    .child(
                        div()
                            .id(ElementId::Name(
                                format!("tree-file-link-{}-{ix}", repo.id).into(),
                            ))
                            .relative()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .px(zpx(4.))
                            .rounded(RADIUS_SM)
                            .cursor_pointer()
                            .hover_fade(alpha(c(t.hover), 0.5), None)
                            .on_click(cx.listener({
                                let target = file_target.clone();
                                move |this, _: &ClickEvent, window, cx| {
                                    this.show_file_diff(target.clone(), window, cx);
                                }
                            }))
                            .child(
                                div()
                                    .flex_1()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(zpx(4.))
                                    .min_w_0()
                                    .mr(zpx(6.))
                                    .child(
                                        div()
                                            .flex_none()
                                            .size(zpx(15.))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .pb(px(1.))
                                            .rounded(px(3.))
                                            .bg(bg_color)
                                            .text_color(text_color)
                                            .font_family(MONO)
                                            .text_size(px(10.))
                                            .line_height(px(10.))
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child(status_char.to_string()),
                                    )
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_color(muted)
                                            .child(Icon::File.el(zpx(11.))),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .flex()
                                            .flex_row()
                                            .items_baseline()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .when(!dir.is_empty(), |d| {
                                                d.child(
                                                    div()
                                                        .flex_shrink(1.)
                                                        .min_w_0()
                                                        .truncate()
                                                        .font_family(MONO)
                                                        .text_size(px(11.))
                                                        .text_color(c(t.text_faint))
                                                        .child(dir.to_string()),
                                                )
                                            })
                                            .child(
                                                div()
                                                    .flex_none()
                                                    .min_w_0()
                                                    .font_family(MONO)
                                                    .text_size(px(11.))
                                                    .font_weight(medium)
                                                    .text_color(text)
                                                    .truncate()
                                                    .child(base.to_string()),
                                            ),
                                    ),
                            )
                            .child(div().flex_none().when(
                                file.added.is_some() || file.removed.is_some(),
                                |d| {
                                    d.child(
                                        tnum(
                                            div()
                                                .flex()
                                                .flex_row()
                                                .items_center()
                                                .gap(zpx(3.))
                                                .font_family(MONO)
                                                .text_size(px(10.5)),
                                        )
                                        .when_some(file.added.filter(|&a| a > 0), |d, a| {
                                            d.child(
                                                div()
                                                    .text_color(c(t.success))
                                                    .font_weight(medium)
                                                    .child(format!("+{a}")),
                                            )
                                        })
                                        .when_some(
                                            file.removed.filter(|&r| r > 0),
                                            |d, r| {
                                                d.child(
                                                    div()
                                                        .text_color(c(t.danger))
                                                        .font_weight(medium)
                                                        .child(format!("-{r}")),
                                                )
                                            },
                                        ),
                                    )
                                },
                            ))
                            .child(reveal_on_hover(
                                "actions-reveal",
                                group_id.clone(),
                                0.,
                                1.,
                                div()
                                    .absolute()
                                    .right(zpx(2.))
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(zpx(2.))
                                    .bg(c(t.floating))
                                    .pl(zpx(4.))
                                    .child(
                                        div()
                                            .id(ElementId::Name(
                                                format!("tree-diff-btn-{}-{ix}", repo.id).into(),
                                            ))
                                            .size(zpx(18.))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .rounded(RADIUS_SM)
                                            .border_1()
                                            .border_color(c(t.border))
                                            .bg(c(t.floating))
                                            .cursor_pointer()
                                            .hover(|s| s.border_color(c(t.text_muted)))
                                            .hover_fade(c(t.hover), None)
                                            .child(Icon::FileDiff.el(zpx(10.)))
                                            .tooltip(move |_, cx| {
                                                cx.new(|_| Tip::new("View Diff", t)).into()
                                            })
                                            .tooltip_show_delay(crate::view::tooltip::delay())
                                            .on_click(cx.listener({
                                                let target = file_target.clone();
                                                move |this, _: &ClickEvent, window, cx| {
                                                    cx.stop_propagation();
                                                    this.show_file_diff(target.clone(), window, cx);
                                                }
                                            })),
                                    )
                                    .child(
                                        div()
                                            .id(ElementId::Name(
                                                format!("tree-open-btn-{}-{ix}", repo.id).into(),
                                            ))
                                            .size(zpx(18.))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .rounded(RADIUS_SM)
                                            .border_1()
                                            .border_color(c(t.border))
                                            .bg(c(t.floating))
                                            .cursor_pointer()
                                            .hover(|s| s.border_color(c(t.text_muted)))
                                            .hover_fade(c(t.hover), None)
                                            .child(Icon::Forward.el(zpx(10.)))
                                            .tooltip(move |_, cx| {
                                                cx.new(|_| Tip::new("Open in diff panel", t)).into()
                                            })
                                            .tooltip_show_delay(crate::view::tooltip::delay())
                                            .on_click(cx.listener({
                                                let target = file_target.clone();
                                                move |this, _: &ClickEvent, window, cx| {
                                                    cx.stop_propagation();
                                                    this.show_file_diff(target.clone(), window, cx);
                                                }
                                            })),
                                    ),
                            )),
                    );
                children_col = children_col.child(item);
            }

            if show_more_pill {
                children_col = children_col.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .h(zpx(22.))
                        .child(tree_connector(true, spine_color))
                        .child(
                            div()
                                .id(ElementId::Name(format!("tree-more-{}", repo.id).into()))
                                .cursor_pointer()
                                .px(zpx(8.))
                                .py(zpx(1.5))
                                .rounded_full()
                                .bg(c(t.code).opacity(0.4))
                                .hover(|s| s.text_color(text))
                                .hover_fade(c(t.hover), None)
                                .t_caption()
                                .font_weight(medium)
                                .text_color(muted)
                                .child(if is_expanded {
                                    format!("Collapse sample · {more_count} more")
                                } else {
                                    format!("+{more_count} more files")
                                })
                                .on_click(cx.listener({
                                    let repo_id = repo.id.clone();
                                    move |this, _: &ClickEvent, _, cx| {
                                        if this.session_card.expanded_more.contains(&repo_id) {
                                            this.session_card.expanded_more.remove(&repo_id);
                                        } else {
                                            this.session_card.expanded_more.insert(repo_id.clone());
                                        }
                                        cx.notify();
                                    }
                                })),
                        ),
                );
            }

            group_el = group_el.child(children_col);
        }

        group_el.into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_card_full(
        &mut self,
        chat_id: &str,
        usage: &ChatUsage,
        bar_now: f32,
        git: &Git,
        folder: Option<(String, String)>,
        rows: &[ProcRow],
        menu: Option<AnyElement>,
        now: Instant,
        now_ms: i64,
        t: Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let text = c(t.text);
        let muted = c(t.text_muted);
        let medium = gpui::FontWeight::MEDIUM;
        let fill = usage.fill();
        let reduced = self.reduced();

        // Context: the model on the label line; used / total and %; the bar; the split.
        let model = self.state.chats.get(chat_id).and_then(|chat| {
            match self.model_name(&chat.harness, &chat.model) {
                Some(name) => Some(name),
                None => {
                    (!chat.model.is_empty()).then(|| crate::model_menu::format_name(&chat.model))
                }
            }
        });
        let used = self.card_fade(
            "used",
            if usage.context_used > 0 {
                compact(usage.context_used)
            } else {
                "0".into()
            },
            now,
        );
        let pct =
            fill.map(|f| self.card_fade("pct", format!("{}%", (f * 100.).round() as u32), now));
        let tip = context_tip(usage);
        let split = token_split(usage);
        let tone = fill.map_or(muted, |f| fill_color(f, t));
        let pct_color = match fill {
            Some(f) if f >= 0.7 => tone,
            _ => text,
        };
        let context = div()
            .id("card-context")
            .flex()
            .flex_col()
            .gap(zpx(LINE_GAP))
            .child(section_label(
                "Context",
                model.map(|m| {
                    div()
                        .min_w_0()
                        .truncate()
                        .font_weight(gpui::FontWeight::NORMAL)
                        .child(m)
                        .into_any_element()
                }),
                t,
            ))
            .child(
                tnum(
                    div()
                        .flex()
                        .flex_row()
                        .items_baseline()
                        .whitespace_nowrap()
                        .t_body(),
                )
                .child(
                    div()
                        .font_weight(medium)
                        .text_color(text)
                        .child(Self::fade_el(&used, now, window)),
                )
                .child(div().text_color(muted).child(match fill {
                    Some(_) => format!(" / {}", compact(usage.context_window)),
                    None => " used".into(),
                }))
                .child(div().flex_1().min_w(zpx(8.)))
                .when_some(pct, |d, p| {
                    d.child(
                        div()
                            .font_weight(medium)
                            .text_color(pct_color)
                            .child(Self::fade_el(&p, now, window)),
                    )
                }),
            )
            .when(fill.is_some(), |d| d.child(bar(bar_now, c(t.border), tone)))
            .child(
                tnum(div().t_small().text_color(muted).truncate()).child(match (&split, fill) {
                    (Some(s), _) => s.clone(),
                    (None, Some(_)) => "No tokens used yet".into(),
                    (None, None) => "This agent doesn't report its context window".into(),
                }),
            )
            .tooltip(move |_, cx| cx.new(|_| Tip::new(tip.clone(), t)).into())
            .tooltip_show_delay(crate::view::tooltip::delay());

        // Project: connected tree hierarchy for projects and non-project folders.
        if let Some(id) = self.projects.chat_project_id(chat_id) {
            self.refresh_project_folders(&id, cx);
        }
        let project_branches = self.projects.card_branches(chat_id);
        let mut project = div()
            .flex()
            .flex_col()
            .gap(zpx(LINE_GAP))
            .child(section_label("Project", None, t));
        match folder {
            None => {
                let chat_row = self.state.chats.get(chat_id);
                let hosted = chat_row
                    .and_then(|c| self.state.spaces.get(&c.space_id))
                    .is_some_and(|s| s.device_id == self.state.local_device_id);
                let idle = chat_row.is_some_and(|c| {
                    !matches!(
                        c.status,
                        keel_proto::ChatStatus::Running | keel_proto::ChatStatus::AwaitingInput
                    )
                });
                let isolated = chat_row.is_some_and(|c| {
                    !c.cwd.is_empty()
                        && std::path::Path::new(&c.cwd)
                            .starts_with(self.state.data_dir.join("scratch"))
                });
                project = project.child(
                    div().t_small().text_color(muted).child(if isolated {
                        "Scratch folder · Isolated per chat"
                    } else {
                        "No project folder"
                    }),
                );
                if hosted {
                    let move_id = chat_id.to_string();
                    let btn = div()
                        .id("card-move-to-folder")
                        .mt(zpx(2.))
                        .px(zpx(8.))
                        .h(zpx(24.))
                        .rounded(RADIUS_MD)
                        .border_1()
                        .border_color(c(t.border).opacity(0.5))
                        .bg(alpha(c(t.hover), 0.3))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(zpx(6.))
                        .t_small()
                        .font_weight(medium)
                        .child(
                            div()
                                .flex_none()
                                .text_color(muted)
                                .child(Icon::Folder.el(zpx(12.))),
                        )
                        .child("Move to folder…")
                        .child(
                            div()
                                .flex_none()
                                .text_color(muted)
                                .child(Icon::MoveRight.el(zpx(11.))),
                        );
                    let btn = if idle {
                        btn.text_color(text)
                            .cursor_pointer()
                            .hover(|s| s.border_color(c(t.border)))
                            .hover_fade(c(t.hover), None)
                            .active(|s| s.opacity(0.82))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.move_chat_to_folder(move_id.clone(), cx);
                            }))
                            .tooltip(move |_, cx| {
                                cx.new(|_| {
                                    Tip::new("Promote scratch files into a project folder", t)
                                })
                                .into()
                            })
                            .tooltip_show_delay(crate::view::tooltip::delay())
                    } else {
                        btn.text_color(muted)
                            .opacity(0.6)
                            .tooltip(move |_, cx| {
                                cx.new(|_| Tip::new("Wait for the active turn to finish", t))
                                    .into()
                            })
                            .tooltip_show_delay(crate::view::tooltip::delay())
                    };
                    project = project.child(btn);
                }
            }
            Some((_, name)) => {
                if let Some((project_name, lines)) = &project_branches {
                    project = project.child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(zpx(6.))
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(muted)
                                    .child(Icon::Folder.el(zpx(13.))),
                            )
                            .child(
                                div()
                                    .t_body()
                                    .font_weight(medium)
                                    .text_color(text)
                                    .min_w_0()
                                    .truncate()
                                    .child(project_name.clone()),
                            ),
                    );

                    let mut tree_stack = div().flex().flex_col().gap(zpx(6.));
                    for line in lines {
                        let details: Vec<GitFileChange> = if !line.file_details.is_empty() {
                            line.file_details.clone()
                        } else {
                            line.files
                                .iter()
                                .map(|p| GitFileChange {
                                    path: p.clone(),
                                    status: 'M',
                                    added: None,
                                    removed: None,
                                })
                                .collect()
                        };
                        let branch_str = if line.branch.is_empty() {
                            if !line.note.is_empty() {
                                line.note.clone()
                            } else {
                                "detached".into()
                            }
                        } else {
                            line.branch.clone()
                        };
                        let repo_item = RepoTreeItem {
                            id: line.alias.clone(),
                            name: line.alias.clone(),
                            branch: branch_str.clone(),
                            branch_display: truncate_branch(&branch_str),
                            files_count: line.file_count.unwrap_or(line.files.len() as u32),
                            total_additions: line.total_additions,
                            total_deletions: line.total_deletions,
                            file_details: details,
                            target_prefix: format!("@{}", line.alias),
                        };
                        tree_stack =
                            tree_stack.child(self.render_connected_tree_repo(&repo_item, t, cx));
                    }
                    project = project.child(tree_stack);
                } else {
                    // Non-project session: single repo tree
                    match git {
                        Git::Unknown => {
                            project = project.child(
                                div().h(zpx(BRANCH_H)).child(
                                    div()
                                        .t_small()
                                        .text_color(muted)
                                        .child("Loading git status…"),
                                ),
                            );
                        }
                        Git::NoRepo => {
                            project = project.child(
                                div()
                                    .t_small()
                                    .text_color(muted)
                                    .child("Not a git repository"),
                            );
                        }
                        Git::Repo(info) => {
                            let branch_str = info.branch_label();
                            let repo_item = RepoTreeItem {
                                id: "current-repo".into(),
                                name,
                                branch: branch_str.clone(),
                                branch_display: truncate_branch(&branch_str),
                                files_count: info.files as u32,
                                total_additions: info.added,
                                total_deletions: info.removed,
                                file_details: info.file_details.clone(),
                                target_prefix: String::new(),
                            };
                            project =
                                project.child(self.render_connected_tree_repo(&repo_item, t, cx));
                        }
                    }
                }

                if let Some((why, at)) = self.session_card.notice.clone() {
                    let p = if reduced {
                        1.
                    } else {
                        (now.saturating_duration_since(at).as_secs_f32() / FADE.as_secs_f32())
                            .min(1.)
                    };
                    if p < 1. {
                        window.request_animation_frame();
                    }
                    project = project.child(
                        div()
                            .flex()
                            .flex_row()
                            .items_start()
                            .gap(zpx(6.))
                            .opacity(ease_out_strong(p))
                            .t_small()
                            .text_color(c(t.warning))
                            .child(
                                div()
                                    .flex_none()
                                    .pt(zpx(2.))
                                    .child(Icon::Alert.el(zpx(12.))),
                            )
                            .child(div().flex_1().min_w_0().child(why)),
                    );
                }
            }
        }
        if menu.is_some() {
            project = project.children(menu);
        }

        // Background: always there; rows glide in and out.
        let leaving: Vec<(ProcRow, Instant)> = self.session_card.leaving.clone();
        let mut items: Vec<(ProcRow, Option<crate::motion::RowLook>)> = rows
            .iter()
            .take(MAX_ROWS)
            .map(|r| {
                let at = self
                    .session_card
                    .entered
                    .get(&r.key)
                    .copied()
                    .unwrap_or(now);
                (r.clone(), row_enter(at, now, reduced))
            })
            .collect();
        items.extend(
            leaving
                .into_iter()
                .map(|(r, at)| (r, row_exit(now.saturating_duration_since(at), reduced))),
        );
        let clock = (now_ms.rem_euclid(1_600_000)) as f32;
        let mut lines: Vec<AnyElement> = Vec::new();
        for (row, look) in items {
            if look.is_some() {
                window.request_animation_frame();
            }
            let look = look.unwrap_or(crate::motion::RowLook {
                opacity: 1.,
                dy: 0.,
                height: 1.,
            });
            let key = format!("row-{}", row.key);
            let waiting = row.label.ends_with(" · waiting");
            // The dot breathes like a running trace step.
            if !reduced {
                window.request_animation_frame();
            }
            let breathe = if reduced {
                1.
            } else {
                0.55 + 0.45 * (crate::trace_view::dot_opacity(clock, 0.) - 0.35) / 0.65
            };
            let (enabled, tip): (bool, &'static str) = match &row.stop {
                Stop::Browser { .. } => (true, BROWSER_STOP),
                Stop::Unavailable(why) => (false, why),
            };
            let stop = row.stop.clone();
            let button =
                div()
                    .id(ElementId::Name(format!("card-stop-{}", row.key).into()))
                    .flex_none()
                    .size(zpx(22.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(RADIUS_SM)
                    .border_1()
                    .border_color(c(t.border))
                    .text_color(if enabled { text } else { muted })
                    .when(!enabled, |b| b.opacity(0.55))
                    .child(Icon::Stop.el(zpx(10.)))
                    .tooltip(move |_, cx| cx.new(|_| Tip::new(tip, t)).into())
                    .tooltip_show_delay(crate::view::tooltip::delay())
                    .when(enabled, |b| {
                        b.cursor_pointer()
                            .hover(|s| s.text_color(c(t.danger)))
                            .hover_fade(c(t.hover), None)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.card_stop(&stop, cx)
                            }))
                    });
            let icon = match row.kind {
                ProcKind::Subagent => Icon::Agent,
                ProcKind::Browser => Icon::Globe,
            };
            lines.push(
                div()
                    .w_full()
                    .flex_none()
                    .overflow_hidden()
                    .h(zpx(ROW_H * look.height))
                    .child(
                        tnum(
                            div()
                                .id(ElementId::Name(key.into()))
                                .relative()
                                .top(zpx(look.dy))
                                .opacity(look.opacity)
                                .h(zpx(ROW_H))
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(zpx(8.))
                                .t_small()
                                .whitespace_nowrap(),
                        )
                        .child(
                            div()
                                .flex_none()
                                .size(zpx(6.))
                                .rounded_full()
                                .bg(c(if waiting {
                                    t.warning
                                } else if row.kind == ProcKind::Browser {
                                    t.info
                                } else {
                                    t.accent
                                }))
                                .opacity(breathe),
                        )
                        .child(div().flex_none().text_color(muted).child(icon.el(zpx(12.))))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(text)
                                .child(row.label.clone()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_color(muted)
                                .child(elapsed(now_ms, row.started_ms)),
                        )
                        .child(button),
                    )
                    .into_any_element(),
            );
        }
        if rows.len() > MAX_ROWS {
            lines.push(
                tnum(
                    div()
                        .h(zpx(20.))
                        .flex()
                        .items_center()
                        .t_small()
                        .text_color(muted),
                )
                .child(more_label(rows.len()))
                .into_any_element(),
            );
        }
        let running = rows.len();
        let note = (running > 0).then(|| {
            tnum(div())
                .font_weight(gpui::FontWeight::NORMAL)
                .child(format!("{running} running"))
                .into_any_element()
        });
        let background = div()
            .flex()
            .flex_col()
            .gap(zpx(LINE_GAP))
            .child(section_label("Background", note, t))
            .map(|d| {
                if lines.is_empty() {
                    d.child(div().t_small().text_color(muted).child("Nothing running"))
                } else {
                    d.child(div().flex().flex_col().gap(zpx(2.)).children(lines))
                }
            });

        div()
            .id("session-card")
            .w_full()
            .flex_none()
            .px(zpx(PAD))
            .py(zpx(10.))
            .flex()
            .flex_col()
            .bg(c(t.floating))
            .border_1()
            .border_color(c(t.border))
            .rounded(RADIUS_LG)
            .occlude()
            .child(context)
            .child(divider(t))
            .child(project)
            .child(divider(t))
            .child(background)
            .into_any_element()
    }

    fn render_task_steps_card(&self, steps: &crate::steps::Steps, t: Tokens) -> AnyElement {
        let is_pending = self.is_plan_pending();
        let is_running = self.active_status() == Some(ChatStatus::Running) || self.dev_live;
        let done = if is_pending { 0 } else { steps.done() };
        let header = div()
            .h(zpx(30.))
            .px_3()
            .flex()
            .items_center()
            .gap_1p5()
            .border_b_1()
            .border_color(c(t.border))
            .text_size(zpx(11.5))
            .text_color(c(t.text_muted))
            .child(Icon::ListChecks.el(zpx(12.)))
            .child(format!("{done} of {}", steps.items.len()));

        let rows = steps.items.iter().enumerate().map(|(ix, label)| {
            let state = if is_pending {
                "later"
            } else if is_running && steps.current == Some(ix) {
                "now"
            } else if ix < done {
                "done"
            } else {
                "later"
            };
            let badge = div()
                .flex_none()
                .size(zpx(20.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .font_family(MONO)
                .text_size(zpx(11.))
                .when(state == "now", |d| {
                    d.bg(c(t.accent))
                        .text_color(c(t.text_on_accent))
                        .child((ix + 1).to_string())
                })
                .when(state == "done", |d| {
                    d.bg(c(t.success).opacity(0.15))
                        .text_color(c(t.success))
                        .child(Icon::Check.el(zpx(11.)))
                })
                .when(state == "later", |d| {
                    d.border_1()
                        .border_color(c(t.border))
                        .text_color(c(t.text_faint))
                        .child((ix + 1).to_string())
                });

            div()
                .min_h(zpx(36.))
                .py_1p5()
                .px_3()
                .flex()
                .items_center()
                .gap_3()
                .when(ix > 0, |d| d.border_t_1().border_color(c(t.border)))
                .when(state == "later", |d| d.opacity(0.55))
                .child(badge)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(zpx(12.5))
                        .text_color(c(if state == "later" {
                            t.text_muted
                        } else {
                            t.text
                        }))
                        .child(label.clone()),
                )
                .when(state == "now", |d| {
                    d.child(
                        div()
                            .flex_none()
                            .text_size(zpx(11.))
                            .text_color(c(t.text_faint))
                            .child("now"),
                    )
                })
        });

        div()
            .id("task-steps-card")
            .w_full()
            .flex_none()
            .bg(c(t.floating))
            .border_1()
            .border_color(c(t.border))
            .rounded(RADIUS_LG)
            .shadow(vec![gpui::BoxShadow {
                color: alpha(c(t.shadow), 0.5),
                offset: gpui::point(px(0.), zpx(4.)),
                blur_radius: zpx(16.),
                spread_radius: px(0.),
                inset: false,
            }])
            .occlude()
            .child(header)
            .children(rows)
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_card_pill(
        &mut self,
        ring_now: f32,
        usage: &ChatUsage,
        git: &Git,
        has_folder: bool,
        running: usize,
        menu: Option<AnyElement>,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = c(t.text_muted);
        let fill = usage.fill();
        let context = match (fill, usage.context_used) {
            (Some(f), _) => format!("{}%", (f * 100.).round() as u32),
            (None, 0) => "0".into(),
            (None, used) => compact(used),
        };
        let tip = context_tip(usage);
        let blocked = self.card_switch_blocked();
        let mut pill = tnum(
            div()
                .id("session-pill")
                .h(zpx(PILL_H))
                .px(zpx(10.))
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(10.))
                .t_small()
                .text_color(muted)
                .whitespace_nowrap()
                .bg(c(t.floating))
                .border_1()
                .border_color(c(t.border))
                .rounded_full()
                .shadow(vec![gpui::BoxShadow {
                    color: alpha(c(t.shadow), 0.5),
                    offset: gpui::point(px(0.), zpx(2.)),
                    blur_radius: zpx(10.),
                    spread_radius: px(0.),
                    inset: false,
                }])
                .occlude(),
        )
        .child(
            div()
                .id("pill-context")
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(6.))
                .child(ring(
                    RING,
                    ring_now,
                    c(t.border),
                    fill.map_or(muted, |f| fill_color(f, t)),
                ))
                .child(div().text_color(c(t.text)).child(context))
                .tooltip(move |_, cx| cx.new(|_| Tip::new(tip.clone(), t)).into())
                .tooltip_show_delay(crate::view::tooltip::delay()),
        );
        if has_folder && let Git::Repo(info) = git {
            let tip: &'static str = blocked.unwrap_or(BRANCH_TIP);
            let branch_bounds = self.card_branch_bounds.clone();
            pill = pill.child(
                div()
                    .id("pill-branch")
                    .relative()
                    .child(
                        gpui::canvas(
                            move |bounds, _, _| {
                                branch_bounds.set(Some(bounds));
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .inset_0(),
                    )
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(4.))
                    .max_w(zpx(140.))
                    .mx(zpx(-4.))
                    .px(zpx(4.))
                    .rounded(RADIUS_SM)
                    .text_color(c(t.text))
                    .when(blocked.is_none(), |d| d.hover_fade(c(t.hover), None))
                    .child(div().text_color(muted).child(Icon::Branch.el(zpx(12.))))
                    .child(
                        div().min_w_0().truncate().child(
                            self.session_card
                                .switching
                                .clone()
                                .unwrap_or_else(|| info.branch_label()),
                        ),
                    )
                    .tooltip(move |_, cx| cx.new(|_| Tip::new(tip, t)).into())
                    .tooltip_show_delay(crate::view::tooltip::delay())
                    .when(blocked.is_none(), |d| {
                        d.cursor_pointer().on_click(cx.listener(
                            |this, _: &ClickEvent, window, cx| this.card_open_branches(window, cx),
                        ))
                    }),
            );
            if info.added + info.removed > 0 {
                pill = pill.child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(zpx(4.))
                        .child(
                            div()
                                .text_color(c(t.success))
                                .child(format!("+{}", info.added)),
                        )
                        .child(
                            div()
                                .text_color(c(t.danger))
                                .child(format!("−{}", info.removed)),
                        ),
                );
            }
        }
        if running > 0 {
            pill = pill.child(
                div()
                    .id("pill-running")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(4.))
                    .child(div().size(zpx(6.)).rounded_full().bg(c(t.accent)))
                    .child(running.to_string())
                    .tooltip(move |_, cx| {
                        cx.new(|_| {
                            Tip::new(format!("{running} running · widen the window to manage"), t)
                        })
                        .into()
                    })
                    .tooltip_show_delay(crate::view::tooltip::delay()),
            );
        }
        div()
            .relative()
            .flex()
            .flex_col()
            .items_end()
            .child(pill)
            .children(menu)
            .into_any_element()
    }
}

/// The context row's tooltip: this chat's token totals and their split.
fn context_tip(usage: &ChatUsage) -> String {
    let sum = usage.sum();
    let split = |m: &ModelTokens| {
        let mut parts = vec![
            format!("in {}", compact(m.input)),
            format!("out {}", compact(m.output)),
        ];
        if m.cache_read > 0 {
            parts.push(format!("read {}", compact(m.cache_read)));
        }
        if m.cache_write > 0 {
            parts.push(format!("write {}", compact(m.cache_write)));
        }
        if m.cache_warm > 0 {
            parts.push(format!("warm {}", compact(m.cache_warm)));
        }
        if let Some(rate) = m.hit_rate() {
            parts.push(format!("hit rate {:.0}%", (rate * 100.0).round()));
        }
        parts.join(" · ")
    };
    let mut lines = vec![format!("This chat: {} tokens", compact(usage.total()))];
    let models: Vec<&ModelTokens> = usage.models.iter().filter(|m| m.total() > 0).collect();
    match models.as_slice() {
        [] => {}
        [only] => lines.push(split(only)),
        many => {
            if let Some(rate) = sum.hit_rate() {
                lines.push(format!("Hit rate: {:.0}%", (rate * 100.0).round()));
            }
            lines.extend(many.iter().map(|m| format!("{}: {}", m.model, split(m))));
        }
    }
    if usage.fill().is_none() && usage.context_used > 0 {
        lines.push("This agent doesn't report its context window".into());
    }
    lines.join("\n")
}

/// The line under the first [`MAX_ROWS`] background rows.
fn more_label(total: usize) -> String {
    format!("+{} more", total.saturating_sub(MAX_ROWS))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn git_replies_parse() {
        assert_eq!(parse_git(&json!({ "repo": false })), Git::NoRepo);
        assert_eq!(parse_git(&json!({})), Git::NoRepo);
        let Git::Repo(info) = parse_git(
            &json!({ "repo": true, "branch": "main", "detached": null, "files": 3, "untracked": 1, "added": 12, "removed": 4 }),
        ) else {
            panic!("repo");
        };
        assert_eq!(
            (
                info.branch_label(),
                info.files,
                info.added,
                info.removed,
                info.untracked
            ),
            ("main".into(), 3, 12, 4, 1)
        );
        let Git::Repo(detached) =
            parse_git(&json!({ "repo": true, "branch": "", "detached": "abc1234", "files": 0 }))
        else {
            panic!("repo");
        };
        assert_eq!(
            (detached.branch_label(), changes_parts(&detached)),
            ("HEAD at abc1234".into(), None)
        );
        let Git::Repo(one) = parse_git(&json!({ "repo": true, "files": 1 })) else {
            panic!("repo")
        };
        assert_eq!(one.files, 1);
    }

    #[test]
    fn full_layout_pins_the_card_and_centers_the_column() {
        // (w, column, card, gap, edge, min_left):
        // Wide (1600 px), collapsed (min_left = 0):
        // region = 1600 - 300 = 1300 px; slack = 1300 - 760 = 540 px split into 270 px each side.
        let (l, r, card) = full_layout(1600., 760., 280., 8., 12., 0.);
        assert_eq!((l, r, card), (270., 570., 12.));
        assert_eq!(1600. - l - r, 760.);
        assert_eq!(r - (12. + 280. + 8.), l);

        // Wide (1352 px, after 248 px sidebar), expanded (min_left = 48):
        // region = 1352 - 300 = 1052 px; slack = 1052 - 760 = 292 px split into 146 px each side.
        let (l, r, card) = full_layout(1352., 760., 280., 8., 12., 48.);
        assert_eq!((l, r, card), (146., 446., 12.));
        assert_eq!(1352. - l - r, 760.);
        assert_eq!(r - (12. + 280. + 8.), l);

        // Narrower (1024 px), expanded (min_left = 48):
        // region = 1024 - 300 = 724 px; column takes 724 - 48 = 676 px.
        let (l, r, card) = full_layout(1024., 760., 280., 8., 12., 48.);
        assert_eq!((l, 1024. - l - r, card), (48., 676., 12.));
        assert_eq!(r, 300.);
    }

    #[test]
    fn compact_mode_has_hysteresis() {
        let need = 760.;
        // Full stays full down to the line, then folds.
        assert!(!next_compact(false, 760., need));
        assert!(next_compact(false, 759., need));
        // Folded stays folded until there's 16 px to spare.
        assert!(next_compact(true, 760., need));
        assert!(next_compact(true, 775., need));
        assert!(!next_compact(true, 776., need));
        // A width wobbling around the line doesn't flip it.
        let mut compact = false;
        let mut flips = 0;
        for w in [770., 755., 765., 758., 770., 774., 762.] {
            let next = next_compact(compact, w, need);
            flips += usize::from(next != compact);
            compact = next;
        }
        assert_eq!((compact, flips), (true, 1));
    }

    #[test]
    fn process_rows_map_subagents_and_the_browser() {
        let subs = [
            json!({ "id": "s2", "name": "research", "status": "running", "created_at": 2_000 }),
            json!({ "id": "s1", "name": "", "description": "Find the bug", "status": "blocked", "created_at": 1_000 }),
            json!({ "id": "s3", "name": "done one", "status": "completed", "created_at": 500 }),
            json!({ "id": "s4", "name": "failed", "status": "error", "created_at": 600 }),
            json!({ "id": "s5", "status": "running", "created_at": 3_000 }),
        ];
        let rows = process_rows(
            "chat-1",
            subs.iter(),
            Some(("click · github.com".into(), 4_000)),
        );
        let view: Vec<_> = rows
            .iter()
            .map(|r| (r.key.as_str(), r.kind, r.label.as_str(), r.started_ms))
            .collect();
        assert_eq!(
            view,
            vec![
                ("browser", ProcKind::Browser, "click · github.com", 4_000),
                (
                    "sub:s1",
                    ProcKind::Subagent,
                    "Find the bug · waiting",
                    1_000
                ),
                ("sub:s2", ProcKind::Subagent, "research", 2_000),
                ("sub:s5", ProcKind::Subagent, "subagent", 3_000),
            ]
        );
        // Stop wiring: the browser row cancels this chat's browser work;
        // subagents have no per-item stop and say why.
        assert_eq!(
            rows[0].stop,
            Stop::Browser {
                chat: "chat-1".into()
            }
        );
        assert!(
            rows[1..]
                .iter()
                .all(|r| r.stop == Stop::Unavailable(SUBAGENT_NO_STOP))
        );
        assert!(process_rows("c", std::iter::empty(), None).is_empty());
    }

    #[test]
    fn fades_crossfade_only_on_change() {
        let t0 = Instant::now();
        let mut fade = Fade::default();
        fade.set("3 files", t0, true);
        assert_eq!((fade.now.as_str(), fade.progress(t0)), ("3 files", None));
        fade.set("3 files", t0, true);
        assert_eq!(fade.at, None);
        fade.set("4 files", t0, true);
        assert_eq!(
            (fade.prev.as_str(), fade.now.as_str()),
            ("3 files", "4 files")
        );
        assert!(
            fade.progress(t0 + Duration::from_millis(80))
                .is_some_and(|p| p > 0. && p < 1.)
        );
        assert_eq!(fade.progress(t0 + FADE), None);
        fade.set("5 files", t0, false);
        assert_eq!((fade.now.as_str(), fade.at), ("5 files", None));
    }

    #[test]
    fn changes_line_counts_untracked_apart_and_clean_is_none() {
        let info = |files, untracked, added, removed| GitInfo {
            files,
            untracked,
            added,
            removed,
            ..GitInfo::default()
        };
        assert_eq!(changes_parts(&info(0, 0, 0, 0)), None);
        assert_eq!(
            changes_parts(&info(4, 1, 12, 3)),
            Some((
                Some(("+12".into(), "−3".into())),
                "3 files changed · 1 untracked".into()
            ))
        );
        assert_eq!(
            changes_parts(&info(1, 0, 0, 0)),
            Some((None, "1 file changed".into()))
        );
        assert_eq!(
            changes_parts(&info(2, 2, 0, 0)),
            Some((None, "2 untracked".into()))
        );
    }

    #[test]
    fn branch_listing_pins_the_checked_out_branch() {
        let list = CardBranches::from_reply(
            &json!({ "branches": ["a", "main", "b"], "current": "main", "detached": "abc" }),
        );
        assert_eq!(
            (
                list.branches.clone(),
                list.current.as_deref(),
                list.detached.as_deref()
            ),
            (
                vec!["main".to_string(), "a".into(), "b".into()],
                Some("main"),
                None
            )
        );
        let mut list = CardBranches::from_reply(
            &json!({ "branches": ["a", "b"], "current": null, "detached": "abc1234" }),
        );
        assert_eq!(list.detached.as_deref(), Some("abc1234"));
        list.checked_out("b");
        assert_eq!(
            (list.branches, list.current.as_deref(), list.detached),
            (vec!["b".to_string(), "a".into()], Some("b"), None)
        );
    }

    #[test]
    fn token_split_reads_in_out_cached() {
        let mut usage = ChatUsage::default();
        assert_eq!(token_split(&usage), None);
        usage.models.push(ModelTokens {
            model: "m".into(),
            input: 1200,
            output: 300,
            cache_read: 0,
            cache_write: 0,
            cache_warm: 0,
        });
        assert!(token_split(&usage).is_some_and(|s| s.starts_with("In ")
            && s.contains(" · Out ")
            && !s.contains("Cached")));
    }

    #[test]
    fn more_than_three_rows_fold_into_a_count() {
        assert_eq!(MAX_ROWS, 3);
        assert_eq!(more_label(5), "+2 more");
    }

    #[test]
    fn context_tooltip_carries_the_totals_and_split() {
        let usage = ChatUsage::default();
        assert_eq!(context_tip(&usage), "This chat: 0 tokens");
    }

    #[test]
    fn context_tooltip_shows_cache_split_and_hit_rate() {
        let usage = ChatUsage {
            models: vec![ModelTokens {
                model: "claude".into(),
                input: 200,
                output: 50,
                cache_read: 800,
                cache_write: 0,
                cache_warm: 100,
            }],
            context_used: 1050,
            context_window: 200_000,
        };
        let tip = context_tip(&usage);
        assert!(tip.contains("This chat: 1.1k tokens"), "{tip}");
        assert!(tip.contains("in 200"), "{tip}");
        assert!(tip.contains("out 50"), "{tip}");
        assert!(tip.contains("read 800"), "{tip}");
        assert!(tip.contains("warm 100"), "{tip}");
        assert!(tip.contains("hit rate 80%"), "{tip}");
    }

    #[test]
    fn fill_is_the_accent_until_seventy_percent() {
        let t = Tokens::dark();
        assert_eq!(fill_color(0.69, t), c(t.accent));
        assert_eq!(fill_color(0.7, t), c(t.warning));
        assert_eq!(fill_color(0.9, t), c(t.danger));
    }

    #[test]
    fn elapsed_reads_from_the_start() {
        assert_eq!(elapsed(65_000, 0), "1m 5s");
        assert_eq!(elapsed(1_000, 2_000), "0s");
    }
}

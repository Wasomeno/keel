//! Virtualized transcript list.
//!
//! Bottom-aligned virtualized list for chat messages and streaming live turns.
//! Block rows with stable `{message_id}#{block_id}` identifiers; a message's rows
//! are contiguous and upserted by row id, so live rows and the persisted message
//! with the same id merge instead of duplicating.
//! Height memoization by `(row_id, content_len, width)`: a row is remeasured only
//! when its memo key (content) changes; width changes are handled by the list.
//! Stick-to-bottom with user-scroll interrupt:
//! - Wheel-up breaks stickiness.
//! - Re-engages when within 70 px of the bottom, or immediately on user's own send.
//! - Reduced motion: no spring, jump directly to offset (we only ever jump).
//! - Code block height is `lines * line_height`, independent of syntax highlight.
//! - Paint-only syntax colors.

use crate::hover_fade::HoverFadeExt;
use crate::motion::reveal_on_hover_curved;
use crate::style::zpx;
use std::collections::HashMap;
use std::ops::Range;
use std::time::{Duration, Instant};

use gpui::{
    AnimationExt as _, AnyElement, Context, ElementId, FollowMode, Hsla, IntoElement, ListAlignment, ListState,
    Pixels, Render, SharedString, Styled, TextAlign, TextRun, Window, div, prelude::*, px,
    relative,
};
use keel_theme::{SyntaxPaintMap, Tokens};

use crate::files::{FileKind, FileRef};
use crate::icons::Icon;
use crate::markdown::TranscriptBlock;
use crate::markdown::{ColumnAlign, ImageRef, Inline, InlineSpan, ToolEntry};
use crate::style::{KeelStyled, MONO, RADIUS_LG, RADIUS_MD, RADIUS_SM, READING, c};

/// Height memo key: (row_id, content_len, width_in_pixels_rounded).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HeightMemoKey {
    pub row_id: String,
    pub content_len: usize,
    pub width: u32,
}

/// Memoized row heights to avoid re-measuring unchanged rows.
#[derive(Default, Debug)]
pub struct HeightMemo {
    cache: HashMap<HeightMemoKey, Pixels>,
}

impl HeightMemo {
    pub fn new() -> Self {
        Self::default()
    }

    fn key(row_id: &str, content_len: usize, width: Pixels) -> HeightMemoKey {
        HeightMemoKey {
            row_id: row_id.to_string(),
            content_len,
            width: f32::from(width).round() as u32,
        }
    }

    pub fn get(&self, row_id: &str, content_len: usize, width: Pixels) -> Option<Pixels> {
        self.cache
            .get(&Self::key(row_id, content_len, width))
            .copied()
    }

    pub fn insert(&mut self, row_id: &str, content_len: usize, width: Pixels, height: Pixels) {
        self.cache
            .insert(Self::key(row_id, content_len, width), height);
    }

    pub fn clear(&mut self) {
        self.cache.clear();
    }
}

/// One row item in the virtualized transcript list.
#[derive(Debug, Clone)]
pub struct TranscriptRowItem {
    pub message_id: String,
    pub role: String,
    pub block: TranscriptBlock,
    /// When the row first appeared.
    pub born: std::time::Instant,
    /// When its entrance plays (stagger included). Only rows that arrive
    /// live get one: history, a reopened chat and rows scrolled back into
    /// view are `None` and draw at rest. An update keeps it, so nothing
    /// replays.
    pub enter: Option<std::time::Instant>,
    /// Removed or replaced while live: drawn fading and collapsing for
    /// [`crate::motion::ROW_EXIT`], then reaped. Upserts skip it.
    pub leaving: Option<std::time::Instant>,
}

impl TranscriptRowItem {
    fn new(message_id: &str, role: &str, block: TranscriptBlock) -> Self {
        Self {
            message_id: message_id.to_string(),
            role: role.to_string(),
            block,
            born: std::time::Instant::now(),
            enter: None,
            leaving: None,
        }
    }

    /// Its entrance or exit at `now`: `None` at rest. Tool groups with live
    /// running tools enter line by line (`trace_view`); finished tool groups
    /// (such as when expanding "Show works") glide open here.
    pub fn look(&self, now: std::time::Instant, reduced: bool) -> Option<crate::motion::RowLook> {
        if let Some(at) = self.leaving {
            return crate::motion::row_exit(now.saturating_duration_since(at), reduced);
        }
        if let TranscriptBlock::ToolGroup { tools, .. } = &self.block {
            if tools.iter().any(|t| t.state == "running") {
                return None;
            }
        }
        let at = self.enter?;
        crate::motion::row_enter(at, now, reduced)
    }
}

/// Prose that folds into a turn's trace (see `fold_turn_rows`).
fn is_folded_prose(block: &TranscriptBlock) -> bool {
    matches!(
        block,
        TranscriptBlock::Paragraph { .. }
            | TranscriptBlock::Heading { .. }
            | TranscriptBlock::List { .. }
            | TranscriptBlock::Quote { .. }
            | TranscriptBlock::Code { .. }
            | TranscriptBlock::Table { .. }
            | TranscriptBlock::Rule { .. }
            | TranscriptBlock::Interim(_)
    )
}

impl TranscriptRowItem {
    pub fn row_id(&self) -> &str {
        self.block.id()
    }

    pub fn content_len(&self) -> usize {
        self.block.content_len()
    }
}

/// Transcript component state and virtualized view logic.
pub struct TranscriptState {
    /// Shared with the list's render closure: render clones the `Rc`, not
    /// the rows (it runs every frame while anything animates). Mutations go
    /// through `Rc::make_mut`, which only copies if a frame still holds it.
    pub items: std::rc::Rc<Vec<TranscriptRowItem>>,
    pub list_state: ListState,
    pub height_memo: HeightMemo,
    pub stick_to_bottom: bool,
    pub reduced_motion: bool,
    /// The "Latest" click's smooth scroll: where it began (px from the top)
    /// and when. Following resumes once it lands; any wheel cancels it.
    pub glide: Option<(f32, std::time::Instant)>,
    pub last_measured_row: Option<String>,
    pub measure_count: usize,
    /// Rows arriving now animate in, and rows removed now fade out: the view
    /// sets it while the open chat's run is live. Off, everything cuts.
    pub animate_new: bool,
    /// When this transcript was (re)filled: rows publishing in the first
    /// moments are the chat loading, not live output.
    pub opened_at: std::time::Instant,
    /// The current arrival burst: when it began, and rows in it so far.
    burst: Option<(std::time::Instant, usize)>,
    /// Natural heights of rows mid entrance or exit, measured as they draw,
    /// so their height can glide.
    pub row_heights: std::rc::Rc<std::cell::RefCell<HashMap<String, f32>>>,
}

pub fn tool_group_height(
    group_id: &str,
    tools: &[crate::markdown::ToolEntry],
    overrides: &[(String, bool)],
) -> f32 {
    let base = crate::trace_view::LINE_H * tools.len().max(1) as f32 + 4.0;
    let mut extra = 0.0;
    for tool in tools {
        let is_error = tool.state == "error";
        let open = overrides
            .iter()
            .find(|(id, _)| {
                id == &crate::turn::line_id(group_id, &tool.tool_id) || id.ends_with(&tool.tool_id)
            })
            .map(|(_, o)| *o)
            .unwrap_or(is_error);
        if open {
            let body_lines = if let Some(diff) = &tool.diff {
                diff.new_text.lines().count()
                    + diff.old_text.as_deref().map_or(0, |o| o.lines().count())
            } else if !tool.output_tail.is_empty() {
                tool.output_tail.lines().count()
            } else {
                0
            };
            if body_lines > 0 {
                let pad = 8.0 + 2.0;
                let body_h = (body_lines.max(1) as f32 * 16.0 + 34.0)
                    .min(crate::code_block::COMPACT_CAP + 2.0)
                    + pad;
                extra += body_h;
            }
        }
    }
    base + extra
}

impl TranscriptState {
    /// Scrolling down to within this distance of the bottom resumes following.
    /// Only at the bottom itself: resuming switches the list to Tail, which
    /// snaps to the end at once, so a wider band pulled a user who was merely
    /// near the bottom the rest of the way down.
    pub const RE_ENGAGE_THRESHOLD: Pixels = px(2.0);
    /// Rows within this distance above/below the viewport get measured. The list
    /// counts unmeasured rows as 0 px tall, so a small overdraw let a fast flick
    /// clamp the scroll to 0 (a jump to the very top) and made rows appended
    /// below look like "near the bottom" (a yank back down).
    pub const OVERDRAW: Pixels = px(2048.0);
    /// Code (and folded trace text) line height, at the current zoom.
    pub fn line_height() -> Pixels {
        crate::style::zpx(20.0)
    }

    /// Reading line height for prose: about 1.6 of the 14 px body.
    pub fn prose_line_height() -> Pixels {
        crate::style::zpx(PROSE_LINE)
    }

    pub fn new(reduced_motion: bool) -> Self {
        Self {
            items: std::rc::Rc::default(),
            // Top: a short transcript anchors to the top. Following the tail is
            // done by the list's own Tail mode inside layout (see `set_stick`).
            list_state: {
                let state = ListState::new(0, ListAlignment::Top, Self::OVERDRAW);
                state.set_follow_mode(FollowMode::Tail);
                state
            },
            height_memo: HeightMemo::new(),
            stick_to_bottom: true,
            reduced_motion,
            glide: None,
            last_measured_row: None,
            measure_count: 0,
            animate_new: false,
            opened_at: std::time::Instant::now(),
            burst: None,
            row_heights: std::rc::Rc::default(),
        }
    }

    /// Drop every row (switching chats). Stickiness resets.
    pub fn clear(&mut self) {
        self.items = std::rc::Rc::default();
        self.height_memo.clear();
        self.list_state.reset(0);
        self.glide = None;
        self.opened_at = std::time::Instant::now();
        self.burst = None;
        self.row_heights.borrow_mut().clear();
        self.set_stick(true);
    }

    /// User's own send (or the "Latest" pill) re-engages following and jumps
    /// to the end.
    pub fn on_own_send(&mut self) {
        self.glide = None;
        self.set_stick(true);
    }

    /// The "Latest" pill: glide to the end, then follow. Reduced motion (or
    /// nothing to scroll) jumps at once, like a send.
    pub fn glide_to_latest(&mut self, now: std::time::Instant) {
        let max = self.list_state.max_offset_for_scrollbar().y;
        if self.reduced_motion || max <= px(0.) {
            return self.on_own_send();
        }
        let from = -self.list_state.scroll_px_offset_for_scrollbar().y;
        self.glide = Some((f32::from(from), now));
    }

    /// One frame of the glide, before layout. True while still moving (ask
    /// for another frame). The goal is re-read each frame, so streamed rows
    /// push it; on landing, Tail takes over and pins the true bottom.
    pub fn advance_glide(&mut self, now: std::time::Instant) -> bool {
        let Some((from, at)) = self.glide else {
            return false;
        };
        let to = f32::from(self.list_state.max_offset_for_scrollbar().y);
        let (pos, done) = crate::motion::scroll_glide(from, to, now.saturating_duration_since(at));
        if done {
            self.glide = None;
            self.set_stick(true);
            return false;
        }
        let current = f32::from(-self.list_state.scroll_px_offset_for_scrollbar().y);
        self.list_state.scroll_by(px(pos - current));
        true
    }

    /// Single source of truth for following. `stick_to_bottom` drives the
    /// "Latest" pill; the list's Tail mode does the actual snapping, once per
    /// layout, so nothing scrolls from `render` and nothing re-engages behind
    /// the user's back (Normal mode never self-re-engages).
    pub(crate) fn set_stick(&mut self, on: bool) {
        self.stick_to_bottom = on;
        if on {
            // Tail also anchors the scroll at the end right away.
            self.list_state.set_follow_mode(FollowMode::Tail);
        } else {
            self.list_state.set_follow_mode(FollowMode::Normal);
        }
    }

    /// User scroll decision, given the position after the scroll applied.
    /// Wheel-up (positive `delta_y`) interrupts following. Only a deliberate
    /// wheel-down that reaches the bottom (within `RE_ENGAGE_THRESHOLD`)
    /// resumes it, so being near the bottom never pulls you there; zero-delta events (trackpad phase/momentum ends) change nothing, so a
    /// small scroll-up is never yanked back down.
    pub fn handle_scroll(&mut self, distance_from_bottom: Pixels, delta_y: Pixels) {
        if delta_y > zpx(0.0) {
            self.stick_to_bottom = false;
        } else if delta_y < zpx(0.0) && distance_from_bottom <= Self::RE_ENGAGE_THRESHOLD {
            self.stick_to_bottom = true;
        }
    }

    /// Wheel event on the transcript. The list's own wheel handler runs first
    /// (bubble order is innermost first), so this reads the post-scroll position.
    pub fn on_wheel(&mut self, delta_y: Pixels) {
        // The user's own scroll always wins over the pill's glide.
        if delta_y != px(0.) {
            self.glide = None;
        }
        let was_sticky = self.stick_to_bottom;
        let max = self.list_state.max_offset_for_scrollbar().y;
        if max <= zpx(0.) {
            // Everything fits: there is no "away from the bottom" to be.
            self.stick_to_bottom = true;
        } else {
            let current = -self.list_state.scroll_px_offset_for_scrollbar().y;
            self.handle_scroll((max - current).max(zpx(0.)), delta_y);
        }
        if self.stick_to_bottom != was_sticky {
            self.set_stick(self.stick_to_bottom);
        }
    }

    /// Jump to the bottom of the list without changing follow state.
    pub fn scroll_to_bottom(&self) {
        self.list_state.scroll_to_end();
    }

    /// Whether the transcript is currently scrolled to the bottom (or fits within view).
    pub fn is_at_bottom(&self) -> bool {
        if self.items.is_empty() {
            return true;
        }
        let max = self.list_state.max_offset_for_scrollbar().y;
        if max <= zpx(0.) {
            return true;
        }
        let current = -self.list_state.scroll_px_offset_for_scrollbar().y;
        self.stick_to_bottom || (max - current) <= Self::RE_ENGAGE_THRESHOLD
    }

    /// Put the row at `index` at the top and stop following, so the jump
    /// is not yanked back down on the next frame.
    pub fn scroll_to_row(&mut self, index: usize) {
        if index >= self.items.len() {
            return;
        }
        self.glide = None;
        self.set_stick(false);
        self.list_state.scroll_to(gpui::ListOffset {
            item_ix: index,
            offset_in_item: px(0.),
        });
    }

    /// First row of each user message, with a one-line preview. These are
    /// the points a session is made of.
    pub fn user_points(&self) -> Vec<(usize, String)> {
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for (index, row) in self.items.iter().enumerate() {
            if row.role != "user" || !seen.insert(row.message_id.clone()) {
                continue;
            }
            let text = match &row.block {
                TranscriptBlock::Paragraph { content, .. }
                | TranscriptBlock::Heading { content, .. }
                | TranscriptBlock::Quote { content, .. } => content.clone(),
                TranscriptBlock::Images { .. } => "Image".to_string(),
                _ => continue,
            };
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            if text.is_empty() {
                continue;
            }
            let text = match text.char_indices().nth(72) {
                Some((cut, _)) => format!("{}…", &text[..cut]),
                None => text,
            };
            out.push((index, text));
        }
        out
    }

    /// Set or replace rows in the transcript (e.g. from persisted session).
    pub fn set_rows(&mut self, rows: Vec<TranscriptRowItem>) {
        self.items = std::rc::Rc::new(rows);
        self.list_state.reset(self.items.len());
    }

    /// Update a single row at index (e.g. when streaming delta modifies the tail block).
    pub fn remeasure_row(&mut self, index: usize) {
        if index < self.items.len() {
            self.last_measured_row = Some(self.items[index].row_id().to_string());
            self.measure_count += 1;
            self.list_state.remeasure_items(index..index + 1);
        }
    }

    /// Append a new row to the end of the transcript.
    pub fn append_row(&mut self, row: TranscriptRowItem) {
        let index = self.items.len();
        std::rc::Rc::make_mut(&mut self.items).push(row);
        self.list_state.splice(index..index, 1);
        self.last_measured_row = Some(self.items[index].row_id().to_string());
        self.measure_count += 1;
    }

    /// Upsert all rows of one message. Rows are matched by row id; an unchanged
    /// row is not remeasured, a changed row is remeasured in place, new rows are
    /// spliced in after the message's existing rows, and surplus rows removed.
    pub fn upsert_message(&mut self, message_id: &str, role: &str, blocks: Vec<TranscriptBlock>) {
        self.upsert_message_before(message_id, role, blocks, &[]);
    }

    /// [`Self::upsert_message`], and a new message goes ahead of the rows of
    /// `later` (the messages after it in chat order): rows publish when they
    /// are ready, so a reopened chat's tool-only turn can beat the user
    /// message (waiting on its parse) that came before it.
    pub fn upsert_message_before(
        &mut self,
        message_id: &str,
        role: &str,
        blocks: Vec<TranscriptBlock>,
        later: &[String],
    ) {
        let now = std::time::Instant::now();
        let live = self.live_motion(now);
        let start = self.items.iter().position(|r| r.message_id == message_id);
        let Some(start) = start else {
            if blocks.is_empty() {
                return;
            }
            let index = self
                .items
                .iter()
                .position(|r| later.iter().any(|m| *m == r.message_id))
                .unwrap_or(self.items.len());
            let count = blocks.len();
            let rows: Vec<_> = blocks
                .into_iter()
                .map(|block| {
                    let mut row = TranscriptRowItem::new(message_id, role, block);
                    row.enter = self.stamp_enter(live, now);
                    row
                })
                .collect();
            std::rc::Rc::make_mut(&mut self.items).splice(index..index, rows);
            self.list_state.splice(index..index, count);
            self.last_measured_row = Some(self.items[index + count - 1].row_id().to_string());
            self.measure_count += count;
            return;
        };
        // The message's rows, ghosts included. Rows match by row id, so a row
        // keeps its identity (and its entrance state) wherever it lands; the
        // list sees only the inserts and removals, so measured heights stay.
        let mut end = start
            + self.items[start..]
                .iter()
                .take_while(|r| r.message_id == message_id)
                .count();
        let wanted: std::collections::HashSet<String> =
            blocks.iter().map(|b| b.id().to_string()).collect();
        // Narration that just folded behind the turn's step line leaves at
        // once: no fade or collapse outside the fold.
        let folding = blocks
            .iter()
            .any(|b| matches!(b, TranscriptBlock::TurnSummary { .. }));
        let snaps = |row: &TranscriptRowItem| folding && is_folded_prose(&row.block);
        let mut ix = start;
        for block in blocks {
            loop {
                if ix < end && self.items[ix].leaving.is_some() {
                    if snaps(&self.items[ix]) && !wanted.contains(self.items[ix].row_id()) {
                        self.retire(ix, false, now);
                        end -= 1;
                    } else {
                        ix += 1;
                    }
                    continue;
                }
                if ix < end && self.items[ix].row_id() == block.id() {
                    let old = &self.items[ix];
                    if old.block != block || old.role != role {
                        let mut row = old.clone();
                        row.block = block;
                        row.role = role.to_string();
                        std::rc::Rc::make_mut(&mut self.items)[ix] = row;
                        self.last_measured_row = Some(self.items[ix].row_id().to_string());
                        self.measure_count += 1;
                        self.list_state.remeasure_items(ix..ix + 1);
                    }
                    ix += 1;
                    break;
                }
                if ix < end && !wanted.contains(self.items[ix].row_id()) {
                    let animate = live && !snaps(&self.items[ix]);
                    if self.retire(ix, animate, now) {
                        ix += 1;
                    } else {
                        end -= 1;
                    }
                    continue;
                }
                let mut row = TranscriptRowItem::new(message_id, role, block);
                row.enter = self.stamp_enter(live, now);
                std::rc::Rc::make_mut(&mut self.items).insert(ix, row);
                self.list_state.splice(ix..ix, 1);
                self.last_measured_row = Some(self.items[ix].row_id().to_string());
                self.measure_count += 1;
                ix += 1;
                end += 1;
                break;
            }
        }
        // What's left was dropped (or moved, and already drawn anew above).
        while ix < end {
            if self.items[ix].leaving.is_some() {
                if snaps(&self.items[ix]) && !wanted.contains(self.items[ix].row_id()) {
                    self.retire(ix, false, now);
                    end -= 1;
                } else {
                    ix += 1;
                }
                continue;
            }
            let moved = wanted.contains(self.items[ix].row_id());
            let animate = live && !moved && !snaps(&self.items[ix]);
            if self.retire(ix, animate, now) {
                ix += 1;
            } else {
                end -= 1;
            }
        }
    }

    /// Row `ix` is gone from its message: a ghost that fades out (true), or
    /// removed at once (false).
    fn retire(&mut self, ix: usize, animate: bool, now: std::time::Instant) -> bool {
        if animate {
            std::rc::Rc::make_mut(&mut self.items)[ix].leaving = Some(now);
            return true;
        }
        std::rc::Rc::make_mut(&mut self.items).remove(ix);
        self.list_state.splice(ix..ix + 1, 0);
        false
    }

    /// Rows publishing now are live output, not the chat loading.
    fn live_motion(&self, now: std::time::Instant) -> bool {
        self.animate_new
            && !self.reduced_motion
            && now.saturating_duration_since(self.opened_at)
                >= std::time::Duration::from_millis(800)
    }

    /// The entrance start for a row arriving now: rows within ~100 ms of the
    /// burst's first step 30 ms apart, three at most.
    fn stamp_enter(&mut self, live: bool, now: std::time::Instant) -> Option<std::time::Instant> {
        if !live {
            return None;
        }
        let k = match self.burst {
            Some((at, k)) if now.saturating_duration_since(at) < crate::motion::ROW_BURST => k + 1,
            _ => {
                self.burst = Some((now, 0));
                0
            }
        };
        if let Some((_, n)) = self.burst.as_mut() {
            *n = k;
        }
        Some(now + crate::motion::ROW_STAGGER * k.min(3) as u32)
    }

    /// Drop ghosts whose exit has played. True while any row still enters
    /// or leaves (the view keeps asking for frames).
    pub fn reap(&mut self, now: std::time::Instant) -> bool {
        let done = |r: &TranscriptRowItem| {
            r.leaving
                .is_some_and(|at| now.saturating_duration_since(at) >= crate::motion::ROW_EXIT)
        };
        while let Some(ix) = self.items.iter().position(done) {
            let id = self.items[ix].row_id().to_string();
            std::rc::Rc::make_mut(&mut self.items).remove(ix);
            self.list_state.splice(ix..ix + 1, 0);
            if !id.contains('#') {
                self.row_heights.borrow_mut().remove(&id);
            }
        }
        let moving = self
            .items
            .iter()
            .any(|r| r.look(now, self.reduced_motion).is_some());
        // Fold bodies (`fold:`) and step rows keep theirs: a close starts from it.
        // ponytail: fold heights live until the chat switches; prune if chats get huge.
        if !moving && !self.row_heights.borrow().is_empty() {
            self.row_heights
                .borrow_mut()
                .retain(|k, _| k.starts_with("fold:") || k.contains('#'));
        }
        moving
    }

    /// Height for a code block is `lines * line_height` (+ padding), independent
    /// of highlighting. Other rows are measured by the list; this estimate is
    /// only memoized for callers that need a number up front.
    pub fn compute_block_height(&mut self, block: &TranscriptBlock, width: Pixels) -> Pixels {
        let row_id = block.id();
        let content_len = block.content_len();
        if let Some(cached) = self.height_memo.get(row_id, content_len, width) {
            return cached;
        }
        let height = match block {
            // Muted narration keeps the prose type, so it is as tall.
            TranscriptBlock::Interim(inner) => return self.compute_block_height(inner, width),
            TranscriptBlock::Code {
                line_count,
                language,
                content,
                closed,
                ..
            } => diagram_height(language, content, *closed, width).unwrap_or_else(|| {
                let _ = line_count;
                let mut props = crate::code_block::CodeBlockProps::new(
                    "",
                    content.as_str(),
                    crate::code_block::CodeSize::Full,
                );
                props.expanded = false;
                px(crate::code_block::outer_height(&props, 900., std::time::Instant::now()) + 8.0)
            }),
            TranscriptBlock::Paragraph { content, .. } => {
                let lines = content.lines().count().max(1);
                px((lines as f32) * f32::from(Self::prose_line_height()) + 12.0)
            }
            TranscriptBlock::List { items, .. } => px((items.len().max(1) as f32)
                * (f32::from(Self::prose_line_height()) + 4.0)
                + 12.0),
            TranscriptBlock::Heading { level, .. } => px(match level {
                1 => 52.0,
                2 => 44.0,
                _ => 38.0,
            }),
            TranscriptBlock::Quote { content, .. } => {
                px((content.lines().count().max(1) as f32) * f32::from(Self::line_height()) + 8.0)
            }
            TranscriptBlock::Rule { .. } => px(17.0),
            TranscriptBlock::Table { header, rows, .. } => {
                let shown = rows.len() + usize::from(header.iter().any(|c| !c.text.is_empty()));
                px(shown as f32 * 30.0 + 10.0)
            }
            TranscriptBlock::ToolGroup {
                tools, expanded, ..
            } if tools.len() == 1 && tools[0].is_receipt() => px(32.0
                + if *expanded {
                    receipt_body_height(&tools[0])
                } else {
                    0.0
                }),
            TranscriptBlock::ToolGroup {
                id,
                tools,
                overrides,
                ..
            } => px(tool_group_height(id, tools, overrides)),
            TranscriptBlock::Plan {
                entries,
                live,
                expanded,
                ..
            } => {
                let open = expanded.unwrap_or(*live);
                px(
                    crate::trace_view::LINE_H * (1 + if open { entries.len() } else { 0 }) as f32
                        + 4.0,
                )
            }
            TranscriptBlock::TurnSummary { .. } => px(crate::trace_view::LINE_H),
            TranscriptBlock::Images { .. } => px(THUMB_H + 8.0),
            TranscriptBlock::Shots { paths, .. } => {
                px(paths.len().max(1) as f32 * (SHOT_H + 8.0) + 8.0)
            }
            TranscriptBlock::Files { .. } => px(FILE_CHIP_H + 12.0),
            TranscriptBlock::Reasoning {
                content, expanded, ..
            } => px(crate::trace_view::LINE_H
                + if *expanded {
                    content.lines().count() as f32 * f32::from(Self::line_height())
                } else {
                    0.0
                }),
            TranscriptBlock::Subagent { .. } => px(crate::trace_view::LINE_H),
            TranscriptBlock::Chart { .. } => px(crate::chart::HEIGHT + 76.0),
            TranscriptBlock::Choices { choices, .. } => {
                px(CHOICE_HEADER_H + CHOICE_ROW_H * choices.options.len() as f32 + 12.0)
            }
            TranscriptBlock::Stats { .. } => px(STAT_H + 10.0),
            TranscriptBlock::FileChanges { card, expanded, .. } => {
                let body_h = if card.files.len() == 1 && card.more == 0 {
                    0.0
                } else if *expanded {
                    FILE_ROW_H * card.files.len() as f32
                        + if card.more > 0 { FILE_ROW_H } else { 0.0 }
                } else {
                    0.0
                };
                px(FILES_HEAD_H + body_h + 12.0)
            }
            TranscriptBlock::MrCard { card, .. } => {
                px(MR_HEAD_H + if card.offer_merge { MR_FOOT_H } else { 0.0 } + 10.0)
            }
            TranscriptBlock::Tests { run, .. } => {
                let failures = if run.failures.is_empty() {
                    0.0
                } else {
                    12.0 + 22.0 * run.failures.len() as f32
                };
                px(TESTS_SUMMARY_H + failures + 10.0)
            }
            TranscriptBlock::Steps { steps, .. } => {
                px(CHOICE_HEADER_H + CHOICE_ROW_H * steps.items.len() as f32 + 12.0)
            }
            TranscriptBlock::Callout { .. } => px(CALLOUT_H + 10.0),
            TranscriptBlock::Compare { columns, .. } => {
                let points = columns.iter().map(|c| c.points.len()).max().unwrap_or(1);
                px(COMPARE_HEAD_H + 22.0 * points as f32 + 22.0)
            }
            TranscriptBlock::PlanCard { plan, .. } => {
                px(36. + 20. + plan.steps.len() as f32 * 40. + 48.)
            }
        };
        self.height_memo.insert(row_id, content_len, width, height);
        height
    }
}

/// Render one transcript row. Shared by the real list's render callback.
///
/// Assistant text is unboxed prose; the user's turns are right-aligned bubbles;
/// errors carry a danger edge; code and tools sit on recessed surfaces.
/// What a row asks of the window: open a tool group, copy a code block.
#[derive(Clone, Debug)]
pub enum RowAction {
    /// Open or close a folded row (tool group, thinking), by row id.
    Toggle {
        row_id: String,
    },
    OpenImage {
        path: String,
    },
    /// A file chip: preview in the browser pane, or open with the system app.
    OpenFile {
        path: String,
    },
    /// A changed file in FileChanges: open its diff in the right-side diffs panel.
    OpenFileDiff {
        path: String,
    },
    /// A subagent indicator row: open that subagent's session.
    OpenSubagent {
        subagent_id: String,
    },
    Copy {
        row_id: String,
        content: String,
    },
    /// A diagram's expand button: show it full window, to pan and zoom.
    OpenDiagram {
        row_id: String,
        source: String,
    },
    /// A diagram as a PNG on the clipboard, drawn in these colors.
    CopyImage {
        row_id: String,
        source: String,
        tokens: Tokens,
    },
    /// A choice button: send `text` as the user's next message.
    Choose {
        text: String,
    },
    OpenUrl {
        url: String,
    },
    /// An MR card's Merge button (`row_id` keys its [`MergeUi`]); the first
    /// click arms it, a second one merges.
    Merge {
        row_id: String,
        message_id: String,
        part_id: String,
    },
    /// An MR card's Track: the chat's header follows this MR.
    TrackMr {
        mr: crate::mr::MrRef,
    },
    /// A code block's Run: type `command` into a new terminal tab. A risky
    /// one only runs on a second click (`row_id` keys the arming).
    Run {
        row_id: String,
        command: String,
        risky: bool,
    },
    /// A shell fence's "Insert in terminal": type `text` at this chat's
    /// prompt (a new shell if it has none) without pressing Return.
    InsertInTerminal {
        text: String,
    },
    /// A diagram node: start a question about it in the composer.
    AskAbout {
        label: String,
    },
    /// Open the full content in a modal viewer overlay.
    OpenFullOutput {
        title: Option<String>,
        content: String,
        error: bool,
    },
    /// Open or close a diagram's copy dropdown menu.
    ToggleDiagramMenu {
        row_id: String,
    },
    /// Close any open diagram copy dropdown menu.
    CloseDiagramMenu,
    /// Copy a chart as an image (2x, padded, card background) to clipboard.
    CopyChartImage {
        row_id: String,
        chart: Box<crate::chart::Chart>,
        tokens: Tokens,
    },
    /// Approve the plan and leave plan mode. `manual` reviews each edit.
    PlanApprove { row_id: String, manual: bool },
    PlanEdit { row_id: String },
    PlanAsk { row_id: String },
    PlanDone { row_id: String },
    PlanAdd { row_id: String },
    PlanDelete { row_id: String, index: usize },
    PlanDrag { row_id: String, index: usize },
    PlanDrop { row_id: String, index: usize },
    /// Expand or fold an approved or rejected plan.
    PlanToggle { row_id: String },
    /// Toggle target list expansion for a specific step.
    PlanToggleTargets { row_id: String, step_ix: usize },
    /// Toggle the edit mode split dropdown menu.
    PlanToggleModeMenu { row_id: String },
    /// Set the chosen edit mode for approval (manual vs auto-accept).
    PlanSetManual { row_id: String, manual: bool },
    /// Set the chosen approval mode (AutoEdit, Ask, Bypass).
    PlanSetMode { row_id: String, mode: crate::plan::ApprovalMode },
    /// Retry prompt send after resume failure.
    ResumeRetry,
    /// Clear native session and send prompt fresh.
    ResumeStartFresh,
}

/// Where an MR card's Merge button is, by row id. No entry: idle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MergeUi {
    /// Clicked once; a second click within a few seconds merges.
    Armed,
    Merging,
    Failed(String),
}

/// Delivers a [`RowAction`] to the window that owns the list.
pub type OnRowAction = std::rc::Rc<dyn Fn(RowAction, &mut gpui::App)>;

/// Per-frame inputs a row needs beyond its own content.
#[derive(Clone)]
pub struct RowContext {
    pub tokens: Tokens,
    pub syntax: SyntaxPaintMap,
    pub reduced: bool,
    /// Currently open diagram copy dropdown (row_id).
    pub diagram_menu: Option<String>,
    pub diagram_menu_ix: usize,
    pub diagram_menu_keyboard: bool,
    /// Folded rows the user just opened or closed: row id → (toggle number,
    /// when). Recent ones animate; the number names the animation.
    pub toggles: std::rc::Rc<HashMap<String, (u64, std::time::Instant)>>,
    /// The code row whose copy button shows "Copied".
    pub copied: Option<String>,
    /// When it was copied: code blocks time their "Copied" feedback from it.
    pub copied_at: Option<std::time::Instant>,
    pub on_action: OnRowAction,
    /// This row's place in the transcript, for text selection order.
    pub order: u64,
    /// The message whose choice buttons can be clicked: the latest one, while
    /// the chat is idle. Older ones are shown dimmed.
    pub live_choices: Option<String>,
    /// MR cards' Merge buttons that are not idle, by row id.
    pub merges: std::rc::Rc<HashMap<String, MergeUi>>,
    /// The MR this chat's header tracks, with its latest poll: a card for
    /// the same MR shows that instead of its turn-end snapshot.
    pub tracked_mr: Option<std::rc::Rc<crate::mr::Tracked>>,
    /// Risky Run buttons clicked once, waiting for the second click.
    pub armed_runs: std::rc::Rc<std::collections::HashSet<String>>,
    /// When the running turn started. A live trace line shows the elapsed time.
    pub run_at: Option<std::time::Instant>,
    /// The live step's wording changing: what it leaves, from where.
    pub step_from: Option<crate::trace_view::StepSwap>,
    pub tool_from: std::rc::Rc<std::collections::HashMap<String, crate::trace_view::StepSwap>>,
    /// Drawing narration inside an open fold: prose in the muted color.
    pub muted: bool,
    /// This frame's time. Trace motion is computed from it.
    pub now: std::time::Instant,
    pub now_ms: i64,
    /// Shared phase origin for shimmers and running dots.
    pub clock_epoch: std::time::Instant,
    /// The window's height, for code blocks' 70% expanded cap.
    pub viewport_h: f32,
    /// Trace lines (and code blocks) the user opened or closed, by line id.
    pub open_lines: std::rc::Rc<HashMap<String, bool>>,
    /// Natural heights of rows mid entrance or exit (`TranscriptState::row_heights`).
    pub row_heights: std::rc::Rc<std::cell::RefCell<HashMap<String, f32>>>,
    /// Maximum width for prose blocks (paragraphs, lists, headings).
    pub prose_max_w: f32,
    /// Mid-slide: the widest a user bubble may wrap at, in px. Rows follow
    /// the column's gliding width (their edges and right-aligned icons move
    /// with it), while text wraps once at the narrower end and keeps it.
    pub bubble_max_w: Option<f32>,
    /// Debug trace: records a code block's or tool row's painted box.
    pub probe: Option<std::rc::Rc<dyn Fn(RowProbe) -> Option<AnyElement>>>,
    /// The open chat is running, so a plan with no saved phase is streaming.
    pub plan_running: bool,
    /// Plan cards the user has edited, approved, or that a fixture set.
    pub plan_cards: std::rc::Rc<std::collections::HashMap<String, crate::plan::CardState>>,
}

/// Which row box a debug probe reads.
#[derive(Clone, Copy)]
pub enum RowProbe {
    Code,
    Tool,
}

fn diagram_height(
    language: &Option<String>,
    content: &str,
    closed: bool,
    width: Pixels,
) -> Option<Pixels> {
    if let Some(diagram) = crate::diagram::parse(language.as_deref(), content) {
        return Some(px(crate::diagram::size(&diagram, f32::from(width)).1 + 36.));
    }
    let col = (f32::from(width) - 2.).max(100.);
    let swapped = crate::mermaid::maybe_swap_direction(content, col);
    let size = crate::mermaid::size(&swapped).filter(|_| is_mermaid(language, closed))?;
    let (_, h) = crate::mermaid::inline_fit(size, col);
    let crop_bar = if crate::mermaid::is_cropped(size, col) { 34. } else { 0. };
    Some(px(36. + h + crate::mermaid::PAD * 2. + crop_bar))
}

/// A finished `mermaid` fence, or a `diagram` fence that is not a box drawing.
/// A fence still streaming stays code until it closes.
fn is_mermaid(language: &Option<String>, closed: bool) -> bool {
    closed
        && matches!(
            language.as_deref().map(str::trim),
            Some("mermaid" | "diagram")
        )
}

fn render_mermaid(
    item: &TranscriptRowItem,
    _language: &Option<String>,
    content: &str,
    rc: &RowContext,
) -> gpui::Stateful<gpui::Div> {
    let t = rc.tokens;
    let row_id = item.row_id().to_string();
    let col = rc.prose_max_w;
    let swapped = crate::mermaid::maybe_swap_direction(content, col);
    let source = swapped.into_owned();
    let size = crate::mermaid::size(&source).unwrap_or((1., 1.));
    let s = crate::mermaid::inline_scale(size, col);
    let cropped = crate::mermaid::is_cropped(size, col);
    let drawn_h = (size.1 * s).min(crate::mermaid::MAX_INLINE_H);
    let title = crate::mermaid::diagram_title(&source);
    let menu_open = rc.diagram_menu.as_deref() == Some(item.row_id());
    let group: gpui::SharedString = format!("diagram-{}", item.row_id()).into();

    let show_full = if cropped {
        let (open_id, open_src) = (row_id.clone(), source.clone());
        let open = rc.on_action.clone();
        Some(
            div()
                .id(ElementId::Name(format!("show-full-{}", item.row_id()).into()))
                .h(zpx(34.))
                .w_full()
                .bg(c(t.code))
                .flex()
                .flex_row()
                .items_center()
                .justify_center()
                .gap_2()
                .cursor_pointer()
                .text_color(c(t.text_muted))
                .hover(|s| s.bg(c(t.hover)).text_color(c(t.text)))
                .active(|s| s.bg(c(t.selected)).opacity(0.75))
                .on_click(move |_, _, cx| {
                    open(
                        RowAction::OpenDiagram {
                            row_id: open_id.clone(),
                            source: open_src.clone(),
                        },
                        cx,
                    );
                })
                .child(Icon::Expand.el(zpx(12.)))
                .child(
                    div()
                        .t_small()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child("Show full diagram"),
                ),
        )
    } else {
        None
    };

    div()
        .id(ElementId::Name(format!("diagram-{}", item.row_id()).into()))
        .relative()
        .hover_probe(group.clone())
        .group(group.clone())
        .w_full()
        .my_2()
        .rounded(RADIUS_MD)
        .bg(c(t.code))
        .overflow_hidden()
        .flex()
        .flex_col()
        .child(diagram_header(
            item,
            &title,
            rc.copied.as_deref() == Some(item.row_id()),
            menu_open,
            rc.diagram_menu_ix,
            rc.diagram_menu_keyboard,
            group,
            rc.on_action.clone(),
            row_id,
            source.clone(),
            t,
        ))
        .child(
            div()
                .w_full()
                .px_4()
                .pt_1()
                .pb_4()
                .flex()
                .justify_center()
                .overflow_hidden()
                .max_h(px(drawn_h + crate::mermaid::PAD * 2.))
                .child(
                    mermaid_canvas(source, t, "inline", rc.on_action.clone())
                        .w_full()
                        .max_w(px(size.0 * s))
                        .h(px(size.1 * s)),
                ),
        )
        .children(show_full)
}

fn render_diagram(
    item: &TranscriptRowItem,
    language: &Option<String>,
    content: &str,
    diagram: &crate::diagram::Diagram,
    rc: &RowContext,
) -> gpui::Stateful<gpui::Div> {
    let t = rc.tokens;
    let diagram = diagram.clone();
    let label = language.clone().unwrap_or_else(|| "diagram".into());
    let on_action = rc.on_action.clone();
    let (row_id, text) = (item.row_id().to_string(), content.to_string());
    div()
        .id(ElementId::Name(format!("diagram-{}", item.row_id()).into()))
        .w_full()
        .my_2()
        .rounded(RADIUS_LG)
        .bg(c(t.code))
        .border_1()
        .border_color(c(t.border))
        .overflow_hidden()
        .flex()
        .flex_col()
        .child(diagram_bar(
            item,
            &label,
            rc.copied.as_deref(),
            on_action,
            row_id,
            text,
            None,
            t,
        ))
        .child(
            gpui::canvas(
                {
                    let diagram = diagram.clone();
                    move |bounds, window, _| {
                        let w = f32::from(bounds.size.width).max(120.);
                        let placed = crate::diagram::place(&diagram, w);
                        // One hitbox per node, so a node can be clicked.
                        let nodes: Vec<(gpui::Hitbox, String)> = diagram
                            .nodes
                            .iter()
                            .filter_map(|node| {
                                let (x, y, w, h) = *placed.nodes.get(&node.id)?;
                                let at = gpui::Bounds::new(
                                    shift(bounds.origin, x, y),
                                    gpui::size(px(w), px(h)),
                                );
                                Some((
                                    window.insert_hitbox(at, gpui::HitboxBehavior::Normal),
                                    node.label.clone(),
                                ))
                            })
                            .collect();
                        (bounds.origin, placed, nodes)
                    }
                },
                {
                    let (diagram, on_action) = (diagram.clone(), rc.on_action.clone());
                    move |_, (origin, placed, nodes), window, cx| {
                        paint_diagram(&diagram, &placed, origin, t, window, cx);
                        clickable_nodes(nodes, on_action.clone(), window);
                    }
                },
            )
            .w_full()
            .h(px(crate::diagram::size(&diagram, 520.).1)),
        )
}

/// Header bar for a generative UI chart: badge on left, hover-only copy dropdown on right.
fn chart_bar(
    item: &TranscriptRowItem,
    chart: &crate::chart::Chart,
    group: gpui::SharedString,
    menu_open: bool,
    menu_ix: usize,
    keyboard_opened: bool,
    copied: Option<&str>,
    on_action: OnRowAction,
    t: Tokens,
) -> gpui::Div {
    let id = item.row_id();
    let row_id = id.to_string();
    let is_copied = copied.is_some_and(|c| {
        c == id || c == format!("{id}#csv") || c == format!("{id}#image")
    });

    let icon = match chart.kind {
        crate::chart::Kind::Bar => Icon::ChartColumn,
        crate::chart::Kind::Line => Icon::ChartLine,
    };
    let title = chart.display_title();

    let badge = div()
        .min_w_0()
        .flex_1()
        .flex()
        .flex_row()
        .items_center()
        .gap(zpx(8.))
        .t_caption()
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(c(t.text_muted))
        .child(icon.el(zpx(14.)))
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_size(zpx(12.))
                .child(title),
        );

    let copy_trigger = chart_copy_dropdown(
        id,
        row_id,
        chart.clone(),
        is_copied,
        menu_open,
        menu_ix,
        keyboard_opened,
        t,
        on_action,
    );

    let reveal = reveal_on_hover_curved(
        format!("chart-actions-{id}"),
        group,
        if menu_open { 1. } else { 0. },
        1.,
        std::time::Duration::from_millis(150),
        std::time::Duration::from_millis(100),
        div().child(copy_trigger),
    );

    div()
        .h(zpx(36.))
        .px_4()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .child(badge)
        .child(reveal)
}

fn chart_copy_dropdown(
    id: &str,
    row_id: String,
    chart: crate::chart::Chart,
    copied: bool,
    menu_open: bool,
    menu_ix: usize,
    keyboard_opened: bool,
    t: Tokens,
    on_action: OnRowAction,
) -> impl IntoElement {
    let action_copy_csv = on_action.clone();
    let action_copy_img = on_action.clone();
    let action_close = on_action.clone();
    let action_toggle = on_action.clone();
    let (csv_row, chart_csv) = (row_id.clone(), chart.clone());
    let (img_row, chart_img) = (row_id.clone(), chart.clone());
    let t_row = row_id;

    let trigger = div()
        .id(ElementId::Name(format!("copy-trigger-{id}").into()))
        .flex_none()
        .size(zpx(28.))
        .rounded(RADIUS_SM)
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .text_color(if copied { c(t.accent) } else { c(t.text_muted) })
        .hover_fade(c(t.hover), Some(c(t.selected)))
        .hover(move |s| s.text_color(c(t.text)))
        .active(|s| s.opacity(0.85))
        .on_click(move |_, _, cx| {
            action_toggle(RowAction::ToggleDiagramMenu { row_id: t_row.clone() }, cx);
        })
        .tooltip(move |_, cx| {
            let tip = if copied { "Copied" } else { "Copy" };
            cx.new(|_| crate::view::Tip::new(tip, t)).into()
        })
        .tooltip_show_delay(crate::view::tooltip::delay())
        .child(if copied {
            div()
                .with_animation(
                    ElementId::Name(format!("chart-copy-check-{id}").into()),
                    gpui::Animation::new(std::time::Duration::from_millis(150))
                        .with_easing(crate::motion::ease_out),
                    |el, delta| el.opacity(delta),
                )
                .child(Icon::Check.el(zpx(13.)))
        } else {
            div()
                .with_animation(
                    ElementId::Name(format!("chart-copy-icon-{id}").into()),
                    gpui::Animation::new(std::time::Duration::from_millis(150))
                        .with_easing(crate::motion::ease_out),
                    |el, delta| el.opacity(delta),
                )
                .child(Icon::Copy.el(zpx(13.)))
        });

    let selected_csv = keyboard_opened && menu_ix == 0;
    let selected_img = keyboard_opened && menu_ix == 1;

    let menu = if menu_open {
        let menu_el = div()
            .id(ElementId::Name(format!("copy-menu-{id}").into()))
            .absolute()
            .top(zpx(32.))
            .right_0()
            .w(zpx(136.))
            .occlude()
            .p(zpx(3.))
            .flex()
            .flex_col()
            .gap_0p5()
            .bg(c(t.floating))
            .rounded(RADIUS_MD)
            .border_1()
            .border_color(c(t.border).opacity(0.20))
            .shadow(vec![gpui::BoxShadow {
                color: c(t.shadow).opacity(0.35),
                offset: gpui::point(zpx(0.), zpx(4.)),
                blur_radius: zpx(10.),
                spread_radius: zpx(-2.),
                inset: false,
            }])
            .on_mouse_down_out(move |_, _, cx| {
                action_close(RowAction::CloseDiagramMenu, cx);
            })
            .child(
                dropdown_row(
                    format!("copy-csv-{id}"),
                    Icon::FileText,
                    "Copy as CSV",
                    selected_csv,
                    t,
                    move |_, _, cx| {
                        action_copy_csv(RowAction::Copy {
                            row_id: format!("{csv_row}#csv"),
                            content: chart_csv.to_csv(),
                        }, cx);
                    },
                ),
            )
            .child(
                dropdown_row(
                    format!("copy-image-{id}"),
                    Icon::Image,
                    "Copy as image",
                    selected_img,
                    t,
                    move |_, _, cx| {
                        action_copy_img(RowAction::CopyChartImage {
                            row_id: format!("{img_row}#image"),
                            chart: Box::new(chart_img.clone()),
                            tokens: t,
                        }, cx);
                    },
                ),
            );

        Some(if keyboard_opened {
            menu_el.into_any_element()
        } else {
            menu_el
                .with_animation(
                    ElementId::Name(format!("copy-menu-in-{id}").into()),
                    gpui::Animation::new(std::time::Duration::from_millis(150))
                        .with_easing(crate::motion::ease_out),
                    |el, delta| {
                        el.opacity(delta)
                            .top(zpx(32. - 3. * (1. - delta)))
                    },
                )
                .into_any_element()
        })
    } else {
        None
    };

    div()
        .relative()
        .flex_none()
        .child(trigger)
        .children(menu)
}

/// A generative UI chart: quiet, sleek, modern, consistent with refined cards.
fn render_chart(
    item: &TranscriptRowItem,
    chart: &crate::chart::Chart,
    _replaces: &Option<Box<TranscriptBlock>>,
    rc: &RowContext,
) -> gpui::Div {
    let t = rc.tokens;
    let row_id = item.row_id().to_string();
    let group = SharedString::from(format!("chart-group-{row_id}"));
    let menu_open = rc.diagram_menu.as_deref() == Some(&row_id);

    let header_bar = chart_bar(
        item,
        chart,
        group.clone(),
        menu_open,
        rc.diagram_menu_ix,
        rc.diagram_menu_keyboard,
        rc.copied.as_deref(),
        rc.on_action.clone(),
        t,
    );

    // Optional headline value row under the header
    let headline_row = chart.headline.as_ref().map(|h| {
        let delta_color = match h.better.as_deref() {
            Some("higher") => {
                if h.delta.as_deref().unwrap_or("").starts_with('-') {
                    c(t.danger)
                } else {
                    c(t.success)
                }
            }
            Some("lower") => {
                if h.delta.as_deref().unwrap_or("").starts_with('-') {
                    c(t.success)
                } else {
                    c(t.danger)
                }
            }
            _ => c(t.text_muted),
        };
        div()
            .px_4()
            .pt_1()
            .pb_1()
            .flex()
            .flex_row()
            .items_baseline()
            .gap_2()
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_size(zpx(18.))
                    .text_color(c(t.text))
                    .child(h.value.clone()),
            )
            .children(h.delta.as_ref().map(|d| {
                div()
                    .text_size(zpx(12.))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(delta_color)
                    .child(d.clone())
            }))
            .children(h.label.as_ref().map(|lbl| {
                div()
                    .text_size(zpx(12.))
                    .text_color(c(t.text_muted))
                    .child(lbl.clone())
            }))
    });

    let row_id_str = row_id.clone();
    let active_series: Vec<bool> = (0..chart.series.len())
        .map(|ix| !rc.open_lines.get(&format!("{row_id_str}~hide-series-{ix}")).copied().unwrap_or(false))
        .collect();

    let legend = chart.series.iter().enumerate().map(|(ix, series)| {
        let hide_key = format!("{row_id_str}~hide-series-{ix}");
        let hidden = rc.open_lines.get(&hide_key).copied().unwrap_or(false);
        let color = series_color(t, ix, series.highlight);
        let on_toggle = rc.on_action.clone();
        let toggle_key = if hidden {
            format!("{hide_key}=0")
        } else {
            format!("{hide_key}=1")
        };

        div()
            .id(ElementId::Name(format!("legend-{row_id_str}-{ix}").into()))
            .flex()
            .items_center()
            .gap(zpx(6.))
            .cursor_pointer()
            .text_color(c(t.text_muted))
            .hover(|s| s.text_color(c(t.text)))
            .active(|s| s.opacity(0.75))
            .when(hidden, |s| s.opacity(0.35))
            .on_click(move |_, _, cx| {
                on_toggle(RowAction::Toggle { row_id: toggle_key.clone() }, cx);
            })
            .child(
                div()
                    .size(px(8.))
                    .rounded(px(2.))
                    .bg(color),
            )
            .child(
                div()
                    .text_size(zpx(12.))
                    .font_weight(gpui::FontWeight::NORMAL)
                    .child(series.name.clone()),
            )
    });

    let chart_clone = chart.clone();
    let row_id_clone = row_id.clone();
    let reduced = rc.reduced;
    let content = div()
        .flex()
        .flex_col()
        .children(headline_row)
        .child(
            div()
                .px_4()
                .pt_1()
                .pb_2()
                .flex()
                .flex_wrap()
                .gap_3()
                .children(legend),
        )
        .child(
            gpui::canvas(
                |bounds, window, _| window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal),
                move |bounds, hitbox, window, cx| {
                    paint_chart(&chart_clone, &row_id_clone, menu_open, bounds, &hitbox, &active_series, reduced, t, window, cx)
                },
            )
            .w_full()
            .h(px(crate::chart::HEIGHT)),
        );

    let card = div()
        .id(ElementId::Name(format!("chart-card-{row_id}").into()))
        .relative()
        .hover_probe(group.clone())
        .group(group.clone())
        .w_full()
        .my_2()
        .rounded(RADIUS_MD)
        .bg(c(t.code))
        .border_1()
        .border_color(c(t.border).opacity(0.18))
        .group_hover(group, move |s| s.border_color(c(t.border).opacity(0.50)))
        .overflow_hidden()
        .flex()
        .flex_col()
        .child(header_bar)
        .child(content);

    div().w_full().py_1().child(crate::motion::appear(
        card,
        format!("chart-in-{row_id}"),
        crate::motion::BASE,
        4.,
        rc.reduced,
    ))
}

/// Choice buttons as a card: a header, then one row per option with its
/// number, label, a "Recommended" pill, and an arrow that lights on hover.
/// Only the latest turn's card clicks, and only while the chat is idle; an
/// older one is dimmed and says it was answered.
fn render_choices(
    item: &TranscriptRowItem,
    choices: &crate::choices::Choices,
    rc: &RowContext,
) -> gpui::Div {
    let t = rc.tokens;
    let live = rc.live_choices.as_deref() == Some(item.message_id.as_str());
    let header = div()
        .h(zpx(CHOICE_HEADER_H))
        .px_3()
        .flex()
        .items_center()
        .gap_1p5()
        .border_b_1()
        .border_color(c(t.border))
        .text_size(zpx(11.5))
        .text_color(c(t.text_muted))
        .child(
            if live {
                Icon::PointerClick
            } else {
                Icon::Check
            }
            .el(zpx(12.)),
        )
        .child(if live { "Pick one" } else { "Answered" })
        .when(live, |d| {
            d.child(div().text_color(c(t.text_faint)).child("· click to reply"))
        });
    let rows = choices.options.iter().enumerate().map(|(ix, label)| {
        let recommended = choices.recommended == Some(ix);
        let group = SharedString::from(format!("choice-{}-{ix}", item.row_id()));
        let (text, on_action) = (label.clone(), rc.on_action.clone());
        let badge = div()
            .flex_none()
            .size(zpx(20.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .font_family(MONO)
            .text_size(zpx(11.))
            .when(recommended, |d| {
                d.bg(c(t.accent)).text_color(c(t.text_on_accent))
            })
            .when(!recommended, |d| {
                d.border_1()
                    .border_color(c(t.border))
                    .text_color(c(t.text_muted))
            })
            .child((ix + 1).to_string());
        let pill = div()
            .flex_none()
            .h(zpx(18.))
            .px_1p5()
            .flex()
            .items_center()
            .gap_1()
            .rounded_full()
            .bg(c(t.selected))
            .text_size(zpx(10.5))
            .text_color(c(t.text_muted))
            .child(Icon::StarFilled.el(zpx(9.)))
            .child("Recommended");
        let pill = if recommended {
            crate::motion::settle(
                pill,
                format!("choice-rec-{}-{ix}", item.row_id()),
                rc.reduced,
            )
        } else {
            pill.into_any_element()
        };
        let row = div()
            .id(ElementId::Name(group.clone()))
            .group(group.clone())
            .h(zpx(CHOICE_ROW_H))
            .px_3()
            .flex()
            .items_center()
            .gap_3()
            .when(ix > 0, |d| d.border_t_1().border_color(c(t.border)))
            .when(live, |d| d.hover_fade(c(t.hover), Some(c(t.selected))))
            .child(badge)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(zpx(13.5))
                    .text_color(c(t.text))
                    .child(label.clone()),
            )
            .when(recommended, |d| d.child(pill))
            .when(live, |d| {
                d.cursor_pointer()
                    .active(|s| s.opacity(0.85))
                    .on_click(move |_, _, cx| {
                        on_action(RowAction::Choose { text: text.clone() }, cx)
                    })
                    .child(
                        div()
                            .flex_none()
                            .text_color(c(t.text_faint))
                            .group_hover(group, |s| s.text_color(c(t.text)))
                            .child(Icon::Forward.el(zpx(13.))),
                    )
            });
        crate::motion::stagger_in(
            row,
            format!("choice-{}-{ix}", item.row_id()),
            ix,
            rc.reduced,
        )
    });
    div().w_full().py_1().child(
        div()
            .w_full()
            .rounded(RADIUS_LG)
            .border_1()
            .border_color(c(t.border))
            .bg(c(t.surface))
            .overflow_hidden()
            .when(!live, |d| d.opacity(0.6))
            .child(header)
            .children(rows),
    )
}

/// A test run as a card: a status badge and headline with the command,
/// a bar split into passed, failed and skipped, the counts, then the
/// failing tests' names.
fn render_tests(
    item: &TranscriptRowItem,
    run: &crate::test_run::TestRun,
    rc: &RowContext,
) -> gpui::Div {
    let t = rc.tokens;
    let ok = run.failed == 0;
    let status = c(if ok { t.success } else { t.danger });
    let count = |n: u64, label: &'static str, color: Hsla| {
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(div().size(px(6.)).rounded_full().bg(color))
            .child(format!("{n} {label}"))
    };
    let summary = div()
        .px_3()
        .pt(px(10.))
        .pb(px(10.))
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(crate::motion::settle(
                    div()
                        .flex_none()
                        .size(zpx(20.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(status.opacity(0.15))
                        .text_color(status)
                        .child(if ok { Icon::Check } else { Icon::Alert }.el(zpx(11.))),
                    format!("tests-mark-{}", item.row_id()),
                    rc.reduced,
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(zpx(13.5))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(c(t.text))
                        .child(run.headline()),
                )
                .when(!run.command.is_empty(), |d| {
                    d.child(
                        div()
                            .flex_none()
                            .max_w(relative(0.45))
                            .truncate()
                            .font_family(MONO)
                            .text_size(zpx(11.5))
                            .text_color(c(t.text_faint))
                            .child(run.command.clone()),
                    )
                }),
        )
        .child(test_bar(run, t, rc.reduced))
        .child(
            div()
                .flex()
                .gap_3()
                .text_size(zpx(11.5))
                .text_color(c(t.text_muted))
                .child(count(run.passed, "passed", c(t.success)))
                .when(run.failed > 0, |d| {
                    d.child(count(run.failed, "failed", c(t.danger)))
                })
                .when(run.skipped > 0, |d| {
                    d.child(count(run.skipped, "skipped", c(t.text_faint)))
                }),
        );
    let failures = (!run.failures.is_empty()).then(|| {
        div()
            .border_t_1()
            .border_color(c(t.border))
            .px_3()
            .py(px(6.))
            .flex()
            .flex_col()
            .children(run.failures.iter().enumerate().map(|(ix, name)| {
                let row = div()
                    .h(px(22.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_none()
                            .size(px(5.))
                            .rounded_full()
                            .bg(c(t.danger)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(MONO)
                            .text_size(zpx(12.))
                            .text_color(c(t.text_muted))
                            .child(name.clone()),
                    );
                crate::motion::stagger_in(
                    row,
                    format!("test-fail-{}-{ix}", item.row_id()),
                    ix,
                    rc.reduced,
                )
            }))
    });
    let card = div()
        .w_full()
        .rounded(RADIUS_LG)
        .border_1()
        .border_color(c(t.border))
        .bg(c(t.surface))
        .overflow_hidden()
        .child(summary)
        .children(failures);
    div().w_full().py_1().child(card)
}

/// The pass/fail bar. Static: growing its width would reflow the card, and
/// this gpui has no clip-path to reveal it in place.
fn test_bar(run: &crate::test_run::TestRun, t: Tokens, _reduced: bool) -> gpui::Div {
    let total = run.total().max(1) as f32;
    let segment = |n: u64, color: Hsla| {
        (n > 0).then(|| div().h_full().w(relative(n as f32 / total)).bg(color))
    };
    div()
        .w_full()
        .h(px(6.))
        .flex()
        .rounded_full()
        .overflow_hidden()
        .bg(c(t.border))
        .children(segment(run.passed, c(t.success)))
        .children(segment(run.failed, c(t.danger)))
        .children(segment(run.skipped, c(t.text_faint)))
}

/// A merge request as a card: its status badge, number and title, the
/// branches, a status pill and "Open"; when the reply called it ready and
/// GitLab agreed, a Merge button that asks for a second click before it
/// merges (the engine checks the MR live first).
fn render_mr_card(
    item: &TranscriptRowItem,
    card: &crate::mr_card::MrCard,
    message_id: &str,
    part_id: &str,
    rc: &RowContext,
) -> gpui::Div {
    use crate::mr::MrStatus;
    let t = rc.tokens;
    // Tracked in this chat: the header's latest poll, not the turn's snapshot.
    let tracked = rc
        .tracked_mr
        .as_ref()
        .filter(|tracked| tracked.mr == card.mr);
    let snap = tracked
        .and_then(|tracked| tracked.snap.as_ref())
        .unwrap_or(&card.snap);
    let status = snap.status();
    let tint = c(crate::mr::status_color(Some(status), t));
    let row_id = item.row_id().to_string();
    let merge = rc.merges.get(&row_id).cloned();
    let pill = |text: String, color: Hsla| {
        div()
            .flex_none()
            .h(zpx(20.))
            .px_2()
            .flex()
            .items_center()
            .gap_1p5()
            .rounded_full()
            .bg(color.opacity(0.12))
            .text_size(zpx(11.5))
            .text_color(color)
            .child(div().size(px(6.)).rounded_full().bg(color))
            .child(text)
    };
    let open = {
        let (url, on_action) = (card.url(), rc.on_action.clone());
        div()
            .id(ElementId::Name(format!("mr-open-{row_id}").into()))
            .flex_none()
            .h(zpx(24.))
            .px_2()
            .flex()
            .items_center()
            .gap_1()
            .rounded(RADIUS_MD)
            .text_size(zpx(12.))
            .text_color(c(t.text_muted))
            .cursor_pointer()
            .hover(|s| s.text_color(c(t.text)))
            .hover_fade(c(t.hover), Some(c(t.selected)))
            .active(|s| s.opacity(0.85))
            .on_click(move |_, _, cx| on_action(RowAction::OpenUrl { url: url.clone() }, cx))
            .child("Open")
            .child(Icon::External.el(zpx(11.)))
    };
    // Tracking: a quiet label; otherwise a Track button (the header chip).
    let track = if tracked.is_some() {
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .text_size(zpx(11.5))
            .text_color(c(t.text_faint))
            .child(Icon::Eye.el(zpx(11.)))
            .child("Tracking")
            .into_any_element()
    } else {
        let (mr, on_action) = (card.mr.clone(), rc.on_action.clone());
        div()
            .id(ElementId::Name(format!("mr-track-{row_id}").into()))
            .flex_none()
            .h(zpx(24.))
            .px_2()
            .flex()
            .items_center()
            .gap_1()
            .rounded(RADIUS_MD)
            .text_size(zpx(12.))
            .text_color(c(t.text_muted))
            .cursor_pointer()
            .hover(|s| s.text_color(c(t.text)))
            .hover_fade(c(t.hover), Some(c(t.selected)))
            .active(|s| s.opacity(0.85))
            .on_click(move |_, _, cx| on_action(RowAction::TrackMr { mr: mr.clone() }, cx))
            .child(Icon::Eye.el(zpx(11.)))
            .child("Track")
            .into_any_element()
    };
    // CI on its own pill, unless the status already is the pipeline.
    let ci = (!snap.pipeline.is_empty() && !matches!(status, MrStatus::Running | MrStatus::Failed))
        .then(|| {
            let (word, color) = match snap.pipeline.as_str() {
                "success" => ("CI passed", t.success),
                "failed" => ("CI failed", t.danger),
                "running"
                | "pending"
                | "created"
                | "preparing"
                | "waiting_for_resource"
                | "scheduled" => ("CI running", t.warning),
                "canceled" => ("CI canceled", t.text_faint),
                "skipped" => ("CI skipped", t.text_faint),
                _ => ("CI", t.text_faint),
            };
            pill(word.to_string(), c(color))
        });
    let branches = (!snap.source_branch.is_empty()).then(|| {
        div()
            .truncate()
            .font_family(MONO)
            .text_size(zpx(11.5))
            .text_color(c(t.text_faint))
            .child(format!("{} → {}", snap.source_branch, snap.target_branch))
    });
    let head = div()
        .px_3()
        .py(px(10.))
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .flex_none()
                .size(zpx(30.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(tint.opacity(0.15))
                .text_color(tint)
                .child(Icon::GitLab.el(zpx(14.))),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap_1p5()
                        .min_w_0()
                        .child(
                            div()
                                .flex_none()
                                .font_family(MONO)
                                .text_size(zpx(12.))
                                .text_color(c(t.text_faint))
                                .child(card.mr.label()),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(zpx(13.5))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(c(t.text))
                                .child(snap.title.clone()),
                        ),
                )
                .children(branches),
        )
        .children(ci)
        .child(pill(snap.status_text(), tint))
        .child(track)
        .child(open);
    let can_merge = card.offer_merge && !status.is_done() && status != MrStatus::Failed;
    let foot = can_merge.then(|| {
        let on_action = rc.on_action.clone();
        let (label, icon, busy) = match &merge {
            Some(MergeUi::Armed) => ("Click again to merge".to_string(), Icon::Check, false),
            Some(MergeUi::Merging) => ("Merging…".to_string(), Icon::Spinner, true),
            _ => (format!("Merge {}", card.mr.label()), Icon::GitLab, false),
        };
        let armed = matches!(merge, Some(MergeUi::Armed));
        let (row, message_id, part_id) =
            (row_id.clone(), message_id.to_string(), part_id.to_string());
        let error = match &merge {
            Some(MergeUi::Failed(why)) => Some(why.clone()),
            _ => None,
        };
        div()
            .border_t_1()
            .border_color(c(t.border))
            .px_3()
            .h(zpx(MR_FOOT_H))
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(zpx(12.))
                    .text_color(c(if error.is_some() {
                        t.danger
                    } else {
                        t.text_faint
                    }))
                    .child(error.unwrap_or_else(|| {
                        format!(
                            "Keel checks {} on GitLab again before merging",
                            card.mr.label()
                        )
                    })),
            )
            .child({
                let merge_btn = div()
                    .id(ElementId::Name(format!("mr-merge-{row_id}").into()))
                    .flex_none()
                    .h(zpx(28.))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .rounded(RADIUS_MD)
                    .text_size(zpx(12.5))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(c(t.text_on_accent))
                    .bg(c(if armed { t.success } else { t.accent }))
                    .when(busy, |d| d.opacity(0.7))
                    .when(!busy, |d| {
                        d.cursor_pointer()
                            .hover(move |s| s.opacity(0.9))
                            .active(move |s| s.opacity(0.75))
                            .on_click(move |_, _, cx| {
                                on_action(
                                    RowAction::Merge {
                                        row_id: row.clone(),
                                        message_id: message_id.clone(),
                                        part_id: part_id.clone(),
                                    },
                                    cx,
                                )
                            })
                    })
                    .child(icon.el(zpx(12.)))
                    .child(label);
                if armed {
                    crate::motion::settle(merge_btn, format!("mr-armed-{row_id}"), rc.reduced)
                } else {
                    merge_btn.into_any_element()
                }
            })
    });
    div().w_full().py_1().child(
        div()
            .w_full()
            .rounded(RADIUS_LG)
            .border_1()
            .border_color(c(t.border))
            .bg(c(t.surface))
            .overflow_hidden()
            .child(head)
            .children(foot),
    )
}

/// Before → after figures as a row of tiles: the label, the new value large,
/// then the change (green when it went the better way, red when worse) and
/// the old value.
/// Numbered steps. Done steps stay readable, the current one is the filled
/// badge, and later ones fade. Rows cascade in; the current badge settles
/// with a small scale so the eye lands on it.
fn render_steps(
    item: &TranscriptRowItem,
    steps: &crate::steps::Steps,
    rc: &RowContext,
) -> gpui::Div {
    let t = rc.tokens;
    let done = steps.done();
    let header = div()
        .h(zpx(CHOICE_HEADER_H))
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
        let state = if steps.current == Some(ix) {
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
                d.bg(c(t.accent)).text_color(c(t.text_on_accent))
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
        let badge = if state == "now" {
            crate::motion::settle(
                badge.child((ix + 1).to_string()),
                format!("step-now-{}-{ix}", item.row_id()),
                rc.reduced,
            )
        } else {
            badge.into_any_element()
        };
        let row = div()
            .h(zpx(CHOICE_ROW_H))
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
                    .truncate()
                    .text_size(zpx(13.5))
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
            });
        crate::motion::stagger_in(row, format!("step-{}-{ix}", item.row_id()), ix, rc.reduced)
    });
    div().w_full().py_1().child(
        div()
            .w_full()
            .rounded(RADIUS_LG)
            .border_1()
            .border_color(c(t.border))
            .bg(c(t.surface))
            .overflow_hidden()
            .child(header)
            .children(rows),
    )
}

/// One sentence. It rises a few pixels into place; a warning or result
/// keeps its color, and reduced motion is a fade only.
fn render_callout(
    item: &TranscriptRowItem,
    callout: &crate::callout::Callout,
    rc: &RowContext,
) -> gpui::Div {
    let t = rc.tokens;
    let (icon, tone) = match callout.kind {
        crate::callout::Kind::Warning => (Icon::Alert, t.warning),
        crate::callout::Kind::Result => (Icon::Check, t.success),
        crate::callout::Kind::Note => (Icon::Dialog, t.text_muted),
    };
    let card = div()
        .w_full()
        .px_3()
        .py(px(10.))
        .flex()
        .flex_col()
        .gap_1()
        .rounded(RADIUS_LG)
        .border_1()
        .border_color(c(t.border))
        .bg(c(t.surface))
        .child(
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .text_size(zpx(11.5))
                .text_color(c(tone))
                .child(icon.el(zpx(12.)))
                .child(callout.label()),
        )
        .child(
            div()
                .text_size(zpx(13.5))
                .text_color(c(t.text))
                .child(callout.text.clone()),
        );
    div().w_full().py_1().child(crate::motion::appear(
        card,
        format!("callout-{}", item.row_id()),
        crate::motion::BASE,
        4.,
        rc.reduced,
    ))
}

/// Claims side by side. Columns cascade in; the recommended one gets the
/// same accent treatment as a recommended choice.
fn render_compare(
    item: &TranscriptRowItem,
    columns: &[crate::compare::Column],
    rc: &RowContext,
) -> gpui::Div {
    let t = rc.tokens;
    let cards = columns.iter().enumerate().map(|(ix, col)| {
        let points = col.points.iter().map(|point| {
            div()
                .h(px(22.))
                .flex()
                .items_center()
                .gap_1p5()
                .child(
                    div()
                        .flex_none()
                        .size(px(4.))
                        .rounded_full()
                        .bg(c(t.text_faint)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(zpx(12.5))
                        .text_color(c(t.text))
                        .child(point.clone()),
                )
        });
        let card = div()
            .id(("compare-card", ix))
            .flex_1()
            .min_w_0()
            .px_3()
            .py(px(10.))
            .flex()
            .flex_col()
            .gap_1()
            .rounded(RADIUS_LG)
            .border_1()
            .border_color(c(if col.recommended { t.accent } else { t.border }))
            .bg(c(t.surface))
            .hover_fade(c(t.hover), None)
            .child(
                div()
                    .h(px(18.))
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(zpx(13.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(c(t.text))
                            .child(col.title.clone()),
                    )
                    .when(col.recommended, |d| {
                        d.child(crate::motion::settle(
                            div()
                                .flex_none()
                                .h(zpx(18.))
                                .px_1p5()
                                .flex()
                                .items_center()
                                .rounded_full()
                                .bg(c(t.selected))
                                .text_size(zpx(10.5))
                                .text_color(c(t.text_muted))
                                .child("Recommended"),
                            format!("compare-rec-{}-{ix}", item.row_id()),
                            rc.reduced,
                        ))
                    }),
            )
            .children(points);
        crate::motion::stagger_in(
            card,
            format!("compare-{}-{ix}", item.row_id()),
            ix,
            rc.reduced,
        )
    });
    div().w_full().py_1().flex().gap_2().children(cards)
}

/// A box drawing, painted by the diagram fence's painter. The card fades in;
/// the boxes themselves stay put, since a moving graph is harder to read.
fn render_stats(
    item: &TranscriptRowItem,
    items: &[crate::stats::Stat],
    rc: &RowContext,
) -> gpui::Div {
    let t = rc.tokens;
    div()
        .w_full()
        .py_1()
        .flex()
        .gap_2()
        .children(items.iter().enumerate().map(|(ix, stat)| {
            let tone = match stat.verdict() {
                Some(true) => t.success,
                Some(false) => t.danger,
                None => t.text_muted,
            };
            let change = stat.change_text();
            let card = div()
                .flex_1()
                .min_w_0()
                .h(zpx(STAT_H))
                .px_3()
                .py(px(10.))
                .flex()
                .flex_col()
                .justify_between()
                .rounded(RADIUS_LG)
                .border_1()
                .border_color(c(t.border))
                .bg(c(t.surface))
                .hover(|s| s.border_color(c(t.focus)))
                .child(
                    div()
                        .truncate()
                        .text_size(zpx(11.5))
                        .text_color(c(t.text_muted))
                        .child(stat.label.clone()),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(zpx(22.))
                        .line_height(zpx(26.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(c(t.text))
                        .child(stat.after.clone()),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .min_w_0()
                        .text_size(zpx(11.5))
                        .when(!change.is_empty(), |d| {
                            d.child(crate::motion::settle(
                                div()
                                    .flex_none()
                                    .px_1p5()
                                    .rounded_full()
                                    .bg(c(tone).opacity(0.12))
                                    .text_color(c(tone))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(change),
                                format!("stat-chg-{}-{ix}", item.row_id()),
                                rc.reduced,
                            ))
                        })
                        .child(
                            div()
                                .truncate()
                                .text_color(c(t.text_faint))
                                .child(format!("from {}", stat.before)),
                        ),
                );
            crate::motion::stagger_in(card, format!("stat-{}-{ix}", item.row_id()), ix, rc.reduced)
        }))
}

/// The files a turn changed: a headline with the total lines added and
/// removed, then one row per file (status badge, path, counts and a
/// five-block diffstat), each opening the file.
fn render_file_changes(
    item: &TranscriptRowItem,
    card: &crate::file_changes::FileChanges,
    expanded: bool,
    rc: &RowContext,
) -> gpui::Div {
    let t = rc.tokens;
    let (added, removed) = card.totals();
    let counts = |added: Option<u32>, removed: Option<u32>| {
        div()
            .flex_none()
            .flex()
            .gap_1p5()
            .font_family(MONO)
            .text_size(zpx(11.5))
            .when_some(added.filter(|n| *n > 0), |d, n| {
                d.child(div().text_color(c(t.success)).child(format!("+{n}")))
            })
            .when_some(removed.filter(|n| *n > 0), |d, n| {
                d.child(div().text_color(c(t.danger)).child(format!("−{n}")))
            })
    };

    // If there is only 1 file changed, render a single file change component directly without collapse or expand behavior.
    if card.files.len() == 1 && card.more == 0 {
        let file = &card.files[0];
        let (badge, tone) = match file.status {
            'A' => ("A", t.success),
            'D' => ("D", t.danger),
            '?' => ("U", t.success),
            'R' => ("R", t.accent),
            'U' => ("!", t.warning),
            _ => ("M", t.warning),
        };
        let (dir, name) = match file.path.rsplit_once('/') {
            Some((dir, name)) => (format!("{dir}/"), name.to_string()),
            None => (String::new(), file.path.clone()),
        };
        let has_counts = file.added.is_some() || file.removed.is_some();
        let diffstat = has_counts.then(|| {
            let (green, red) =
                crate::file_changes::blocks(file.added.unwrap_or(0), file.removed.unwrap_or(0));
            div()
                .flex_none()
                .flex()
                .gap(px(2.))
                .children((0..5).map(|block| {
                    let color = if block < green {
                        c(t.success)
                    } else if block < green + red {
                        c(t.danger)
                    } else {
                        c(t.border)
                    };
                    div().size(px(7.)).rounded(px(1.5)).bg(color)
                }))
        });
        let path = file.path.clone();
        let on_action = rc.on_action.clone();
        let single_row = div()
            .id(ElementId::Name(
                format!("changed-{}-0", item.row_id()).into(),
            ))
            .h(zpx(FILES_HEAD_H))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .hover_fade(c(t.hover), Some(c(t.selected)))
            .active(|s| s.opacity(0.85))
            .on_click(move |_, _, cx| on_action(RowAction::OpenFileDiff { path: path.clone() }, cx))
            .child(
                div()
                    .flex_none()
                    .size(zpx(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(crate::style::RADIUS_SM)
                    .bg(c(tone).opacity(0.15))
                    .text_color(c(tone))
                    .font_family(MONO)
                    .text_size(zpx(10.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(badge),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .text_size(zpx(12.5))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(c(t.text_faint))
                            .child(dir),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(c(if file.status == 'D' {
                                t.text_faint
                            } else {
                                t.text
                            }))
                            .when(file.status == 'D', |d| d.line_through())
                            .child(name),
                    ),
            )
            .child(counts(file.added, file.removed))
            .children(diffstat);

        return div().w_full().py_1().child(
            div()
                .w_full()
                .rounded(RADIUS_LG)
                .border_1()
                .border_color(c(t.border))
                .bg(c(t.surface))
                .overflow_hidden()
                .child(single_row),
        );
    }
    let row_id = item.row_id().to_string();
    let on_action = rc.on_action.clone();
    let head = div()
        .id(ElementId::Name(
            format!("files-head-{}", item.row_id()).into(),
        ))
        .h(zpx(FILES_HEAD_H))
        .px_3()
        .flex()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .hover_fade(c(t.hover), Some(c(t.selected)))
        .active(|s| s.opacity(0.85))
        .on_click(move |_, _, cx| {
            on_action(
                RowAction::Toggle {
                    row_id: row_id.clone(),
                },
                cx,
            )
        })
        .child(Icon::FileDiff.el(zpx(13.)))
        .child(
            div()
                .flex_1()
                .text_size(zpx(13.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(c(t.text))
                .child(card.headline()),
        )
        .child(counts(Some(added), Some(removed)))
        .child(chevron(expanded, item.row_id(), rc));
    let rows = card.files.iter().enumerate().map(|(ix, file)| {
        let (badge, tone) = match file.status {
            'A' => ("A", t.success),
            'D' => ("D", t.danger),
            '?' => ("U", t.success),
            'R' => ("R", t.accent),
            'U' => ("!", t.warning),
            _ => ("M", t.warning),
        };
        let (dir, name) = match file.path.rsplit_once('/') {
            Some((dir, name)) => (format!("{dir}/"), name.to_string()),
            None => (String::new(), file.path.clone()),
        };
        let has_counts = file.added.is_some() || file.removed.is_some();
        let diffstat = has_counts.then(|| {
            let (green, red) =
                crate::file_changes::blocks(file.added.unwrap_or(0), file.removed.unwrap_or(0));
            div()
                .flex_none()
                .flex()
                .gap(px(2.))
                .children((0..5).map(|block| {
                    let color = if block < green {
                        c(t.success)
                    } else if block < green + red {
                        c(t.danger)
                    } else {
                        c(t.border)
                    };
                    div().size(px(7.)).rounded(px(1.5)).bg(color)
                }))
        });
        let (path, on_action) = (file.path.clone(), rc.on_action.clone());
        div()
            .id(ElementId::Name(
                format!("changed-{}-{ix}", item.row_id()).into(),
            ))
            .h(zpx(FILE_ROW_H))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .hover_fade(c(t.hover), Some(c(t.selected)))
            .active(|s| s.opacity(0.85))
            .on_click(move |_, _, cx| on_action(RowAction::OpenFileDiff { path: path.clone() }, cx))
            .child(
                div()
                    .flex_none()
                    .size(zpx(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(crate::style::RADIUS_SM)
                    .bg(c(tone).opacity(0.15))
                    .text_color(c(tone))
                    .font_family(MONO)
                    .text_size(zpx(10.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(badge),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .text_size(zpx(12.5))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(c(t.text_faint))
                            .child(dir),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(c(if file.status == 'D' {
                                t.text_faint
                            } else {
                                t.text
                            }))
                            .when(file.status == 'D', |d| d.line_through())
                            .child(name),
                    ),
            )
            .child(counts(file.added, file.removed))
            .children(diffstat)
    });
    let more = (card.more > 0).then(|| {
        div()
            .h(zpx(FILE_ROW_H))
            .px_3()
            .flex()
            .items_center()
            .text_size(zpx(12.))
            .text_color(c(t.text_faint))
            .child(format!("+{} more", card.more))
    });
    let body = div()
        .w_full()
        .flex()
        .flex_col()
        .border_t_1()
        .border_color(c(t.border))
        .children(rows)
        .children(more);
    let body_height =
        FILE_ROW_H * card.files.len() as f32 + if card.more > 0 { FILE_ROW_H } else { 0.0 };
    let body_animated = fold_body(body, item.row_id(), expanded, body_height, rc);
    div().w_full().py_1().child(
        div()
            .w_full()
            .rounded(RADIUS_LG)
            .border_1()
            .border_color(c(t.border))
            .bg(c(t.surface))
            .overflow_hidden()
            .child(head)
            .children(body_animated),
    )
}

const STAT_H: f32 = 84.;
const FILES_HEAD_H: f32 = 36.;
const FILE_ROW_H: f32 = 30.;
const MR_HEAD_H: f32 = 56.;
const MR_FOOT_H: f32 = 44.;
const TESTS_SUMMARY_H: f32 = 84.;
const CHOICE_HEADER_H: f32 = 30.;
const CHOICE_ROW_H: f32 = 40.;
const CALLOUT_H: f32 = 64.;
const COMPARE_HEAD_H: f32 = 36.;

/// Series colors: tone steps from ink, or accent for highlighted series.
fn series_color(t: Tokens, ix: usize, highlight: bool) -> Hsla {
    if highlight {
        return c(t.accent);
    }
    match ix {
        0 => c(t.text),
        1 => c(t.text).opacity(0.40),
        2 => c(t.text_muted),
        3 => c(t.text_faint),
        _ => c(t.text).opacity(0.20),
    }
}

#[derive(Clone)]
struct ChartState {
    created_at: Instant,
    prev_axis: (f64, f64, f64),
    target_axis: (f64, f64, f64),
    axis_start: Instant,
    hovered_group: Option<usize>,
    dev_hovered: Option<Option<usize>>,
    first_hover_at: Option<Instant>,
    tip_is_visible: bool,
    tip_curr_x: f32,
    tip_target_x: f32,
    tip_start_x: f32,
    tip_move_start: Instant,
    tip_opacity: f32,
    tip_opacity_target: f32,
    tip_opacity_from: f32,
    tip_opacity_start: Instant,
    tip_last_group: Option<usize>,
    hover_dim: f32,
    hover_dim_target: f32,
    hover_dim_from: f32,
    hover_dim_start: Instant,
    keyboard_focused: bool,
}

impl ChartState {
    fn new(now: Instant, axis: (f64, f64, f64)) -> Self {
        Self {
            created_at: now,
            prev_axis: axis,
            target_axis: axis,
            axis_start: now,
            hovered_group: None,
            dev_hovered: None,
            first_hover_at: None,
            tip_is_visible: false,
            tip_curr_x: 0.,
            tip_target_x: 0.,
            tip_start_x: 0.,
            tip_move_start: now,
            tip_opacity: 0.0,
            tip_opacity_target: 0.0,
            tip_opacity_from: 0.0,
            tip_opacity_start: now,
            tip_last_group: None,
            hover_dim: 1.0,
            hover_dim_target: 1.0,
            hover_dim_from: 1.0,
            hover_dim_start: now,
            keyboard_focused: false,
        }
    }
}

thread_local! {
    static CHART_STATES: std::cell::RefCell<HashMap<String, ChartState>> =
        std::cell::RefCell::new(HashMap::new());
}

pub(crate) fn dev_set_chart_hover(row_id: &str, group: Option<Option<usize>>) {
    CHART_STATES.with(|states| {
        let mut map = states.borrow_mut();
        let mut matched = false;
        for (k, state) in map.iter_mut() {
            if row_id.is_empty() || k == row_id || k.contains(row_id) || row_id.contains(k.as_str()) {
                matched = true;
                state.dev_hovered = group;
                if let Some(Some(ix)) = group {
                    state.tip_is_visible = true;
                    state.tip_last_group = Some(ix);
                    state.tip_opacity = 1.0;
                    state.tip_opacity_target = 1.0;
                    state.tip_opacity_from = 1.0;
                    state.tip_opacity_start = Instant::now();
                    state.hover_dim = 0.55;
                    state.hover_dim_target = 0.55;
                    state.hover_dim_from = 0.55;
                    state.hover_dim_start = Instant::now();
                } else if let Some(None) = group {
                    state.tip_is_visible = false;
                    state.hovered_group = None;
                    state.tip_last_group = None;
                    state.tip_opacity = 0.0;
                    state.tip_opacity_target = 0.0;
                    state.tip_opacity_from = 0.0;
                    state.hover_dim = 1.0;
                    state.hover_dim_target = 1.0;
                    state.hover_dim_from = 1.0;
                } else {
                    state.dev_hovered = None;
                }
            }
        }
        if !matched {
            let mut state = ChartState::new(Instant::now(), (0., 1., 0.25));
            state.dev_hovered = group;
            if let Some(Some(ix)) = group {
                state.tip_is_visible = true;
                state.tip_last_group = Some(ix);
                state.tip_opacity = 1.0;
                state.tip_opacity_target = 1.0;
                state.tip_opacity_from = 1.0;
                state.tip_opacity_start = Instant::now();
                state.hover_dim = 0.55;
                state.hover_dim_target = 0.55;
                state.hover_dim_from = 0.55;
                state.hover_dim_start = Instant::now();
            } else if let Some(None) = group {
                state.tip_is_visible = false;
                state.hovered_group = None;
                state.tip_last_group = None;
                state.tip_opacity = 0.0;
                state.tip_opacity_target = 0.0;
                state.tip_opacity_from = 0.0;
                state.hover_dim = 1.0;
                state.hover_dim_target = 1.0;
                state.hover_dim_from = 1.0;
            }
            map.insert(row_id.to_string(), state);
        }
    });
}

fn paint_chart_tick(
    text: &str,
    origin: gpui::Point<Pixels>,
    x: f32,
    y: f32,
    right_align: bool,
    color: Hsla,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) {
    if text.is_empty() {
        return;
    }
    let mut font = gpui::font(crate::style::SANS);
    font.features = gpui::FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)]));
    let run = TextRun {
        len: text.len(),
        font,
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window
        .text_system()
        .shape_line(SharedString::from(text.to_string()), px(11.), &[run], None);
    let draw_x = if right_align {
        x - f32::from(line.width)
    } else {
        x
    };
    let _ = line.paint(shift(origin, draw_x, y), px(14.), TextAlign::Left, None, window, cx);
}

fn paint_chart(
    chart: &crate::chart::Chart,
    row_id: &str,
    menu_open: bool,
    bounds: gpui::Bounds<Pixels>,
    hitbox: &gpui::Hitbox,
    active_series: &[bool],
    reduced: bool,
    t: Tokens,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) {
    let now = Instant::now();
    let origin = bounds.origin;
    let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let (left, top, plot_w, plot_h) = crate::chart::plot_box(w, h);
    let (target_lo, target_hi, target_step) = chart.axis_for(active_series);

    let mut state = CHART_STATES.with(|states| {
        states
            .borrow_mut()
            .entry(row_id.to_string())
            .or_insert_with(|| ChartState::new(now, (target_lo, target_hi, target_step)))
            .clone()
    });

    let mut needs_refresh = false;

    // Rescale axis smoothly if target changed
    if state.target_axis != (target_lo, target_hi, target_step) {
        state.prev_axis = state.target_axis;
        state.target_axis = (target_lo, target_hi, target_step);
        state.axis_start = now;
    }
    let (lo, hi, step) = if reduced {
        (target_lo, target_hi, target_step)
    } else {
        let t_axis = (now.duration_since(state.axis_start).as_secs_f32() / 0.20).clamp(0.0, 1.0);
        let ease = crate::motion::ease_out(t_axis);
        let lo = state.prev_axis.0 + (target_lo - state.prev_axis.0) * ease as f64;
        let hi = state.prev_axis.1 + (target_hi - state.prev_axis.1) * ease as f64;
        let step = state.prev_axis.2 + (target_step - state.prev_axis.2) * ease as f64;
        if t_axis < 1.0 {
            needs_refresh = true;
        }
        (lo, hi, step)
    };

    let pointer = window.mouse_position();
    let is_hovered = !menu_open && hitbox.is_hovered(window);
    let pointer_hover = is_hovered
        .then(|| {
            let x = f32::from(pointer.x - origin.x) - left;
            crate::chart::category_at(chart, plot_w, x)
        })
        .flatten();

    if is_hovered {
        window.set_cursor_style(gpui::CursorStyle::PointingHand, hitbox);
    }

    let hb = hitbox.clone();
    window.on_mouse_event(move |_: &gpui::MouseMoveEvent, phase, window, _| {
        if phase == gpui::DispatchPhase::Bubble && hb.is_hovered(window) {
            window.refresh();
        }
    });

    // Track hover transitions and 100ms first-hover delay
    let effective_hover = if menu_open {
        state.hovered_group = None;
        state.first_hover_at = None;
        state.tip_is_visible = false;
        state.tip_opacity = 0.0;
        state.tip_opacity_target = 0.0;
        state.tip_opacity_from = 0.0;
        state.hover_dim = 1.0;
        state.hover_dim_target = 1.0;
        state.hover_dim_from = 1.0;
        None
    } else {
        match state.dev_hovered {
            Some(forced) => {
                state.hovered_group = forced;
                if forced.is_none() {
                    state.first_hover_at = None;
                    state.tip_is_visible = false;
                }
                forced
            }
            None => {
                if pointer_hover.is_some() {
                    if state.hovered_group.is_none() && state.first_hover_at.is_none() {
                        state.first_hover_at = Some(now);
                    }
                    state.hovered_group = pointer_hover;
                } else if !state.keyboard_focused {
                    state.hovered_group = None;
                    state.first_hover_at = None;
                    state.tip_is_visible = false;
                }
                state.hovered_group
            }
        }
    };

    if let Some(ix) = effective_hover {
        state.tip_last_group = Some(ix);
    }

    // Hover dimming easing (120ms enter, 100ms exit)
    let target_dim = if !menu_open && effective_hover.is_some() { 0.55 } else { 1.0 };
    if (state.hover_dim_target - target_dim).abs() > 0.01 {
        state.hover_dim_from = state.hover_dim;
        state.hover_dim_target = target_dim;
        state.hover_dim_start = now;
    }
    if !reduced {
        let dim_dur = if target_dim < 0.9 { 0.12 } else { 0.10 };
        let dim_t = (now.duration_since(state.hover_dim_start).as_secs_f32() / dim_dur).clamp(0.0, 1.0);
        let ease = crate::motion::ease_out(dim_t);
        state.hover_dim = state.hover_dim_from + (state.hover_dim_target - state.hover_dim_from) * ease;
        if dim_t < 1.0 {
            needs_refresh = true;
        }
    } else {
        state.hover_dim = target_dim;
    }

    // Tooltip entrance (150ms) and exit (100ms) fade
    let delay_passed = !menu_open && effective_hover.is_some() && (
        state.tip_is_visible
            || matches!(state.dev_hovered, Some(Some(_)))
            || state.keyboard_focused
            || state.first_hover_at.map_or(false, |at| now.duration_since(at) >= Duration::from_millis(100))
    );
    if delay_passed {
        state.tip_is_visible = true;
    }
    let target_opacity = if !menu_open && state.tip_is_visible && effective_hover.is_some() {
        1.0
    } else {
        0.0
    };
    if (state.tip_opacity_target - target_opacity).abs() > 0.01 {
        state.tip_opacity_from = state.tip_opacity;
        state.tip_opacity_target = target_opacity;
        state.tip_opacity_start = now;
    }
    if !reduced && !state.keyboard_focused {
        let op_dur = if state.tip_opacity_target > 0.5 { 0.15 } else { 0.10 };
        let op_t = (now.duration_since(state.tip_opacity_start).as_secs_f32() / op_dur).clamp(0.0, 1.0);
        let ease = crate::motion::ease_out(op_t);
        state.tip_opacity = state.tip_opacity_from + (state.tip_opacity_target - state.tip_opacity_from) * ease;
        if op_t < 1.0 {
            needs_refresh = true;
        }
    } else {
        state.tip_opacity = target_opacity;
    }

    // Check entry animation status
    let entry_elapsed_ms = now.duration_since(state.created_at).as_secs_f32() * 1000.0;
    let total_entry_ms = match chart.kind {
        crate::chart::Kind::Bar => 240.0 + (chart.labels.len().saturating_sub(1) as f32 * 30.0).min(200.0),
        crate::chart::Kind::Line => 280.0,
    };
    if !reduced && entry_elapsed_ms < total_entry_ms {
        needs_refresh = true;
    }

    // Paint Gridlines and Y-axis Ticks
    let y_of = |v: f64| top + plot_h * (1. - ((v - lo) / (hi - lo).max(1e-6)) as f32);
    let mut tick = lo;
    while tick <= hi + step * 0.1 {
        let y = y_of(tick);
        let is_zero = tick.abs() < 1e-5;
        let grid_color = if is_zero {
            c(t.text).opacity(0.18)
        } else {
            c(t.text).opacity(0.08)
        };
        window.paint_quad(gpui::fill(
            gpui::Bounds::new(shift(origin, left, y.round()), gpui::size(px(plot_w), px(1.))),
            grid_color,
        ));
        let tick_str = chart.format_tick(tick);
        paint_chart_tick(&tick_str, origin, left - 8., y - 6., true, c(t.text_muted), window, cx);
        tick += step;
    }

    // Paint X-axis Category Labels
    let n = chart.labels.len();
    let slot = plot_w / n as f32;
    let every = (60. / slot).ceil().max(1.) as usize;
    for (ix, name) in chart.labels.iter().enumerate().step_by(every) {
        let x = left + slot * (ix as f32 + 0.5);
        let mut font = gpui::font(crate::style::SANS);
        font.features = gpui::FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)]));
        let run = TextRun {
            len: name.len(),
            font,
            color: c(t.text_muted),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = window
            .text_system()
            .shape_line(SharedString::from(name.to_string()), px(11.), &[run], None);
        let line_w = f32::from(line.width);
        let _ = line.paint(
            shift(origin, x - line_w / 2., top + plot_h + 8.),
            px(14.),
            TextAlign::Left,
            None,
            window,
            cx,
        );
    }

    let zero_y = y_of(0f64.clamp(lo, hi));

    match chart.kind {
        crate::chart::Kind::Bar => {
            let active_indices: Vec<usize> = active_series
                .iter()
                .enumerate()
                .filter(|(_, a)| **a)
                .map(|(i, _)| i)
                .collect();
            let k = active_indices.len() as f32;
            if k > 0. {
                let gw = slot * 0.60;
                let bar_w = ((gw - (k - 1.) * 2.0) / k).clamp(3.0, 36.0);
                let total_gw = k * bar_w + (k - 1.) * 2.0;

                for (ix, _) in chart.labels.iter().enumerate() {
                    let entry_progress = if reduced {
                        1.0
                    } else {
                        let stagger = (ix as f32 * 30.0).min(200.0);
                        let t = ((entry_elapsed_ms - stagger) / 240.0).clamp(0.0, 1.0);
                        crate::motion::ease_out(t)
                    };
                    if entry_progress <= 0.0 {
                        continue;
                    }

                    let is_target = effective_hover == Some(ix);
                    let dim = if effective_hover.is_some() && !is_target {
                        state.hover_dim
                    } else {
                        1.0
                    };
                    let lift = if is_target && !reduced {
                        ((1.0 - state.hover_dim) / 0.45).clamp(0.0, 1.0) * 1.5
                    } else {
                        0.0
                    };

                    for (pos, &si) in active_indices.iter().enumerate() {
                        let series = &chart.series[si];
                        let Some(Some(v)) = series.values.get(ix) else { continue };
                        let base_color = series_color(t, si, series.highlight);
                        let color = base_color.opacity(base_color.a * dim * entry_progress);
                        let bar_x = left + slot * (ix as f32 + 0.5) - total_gw / 2.0 + pos as f32 * (bar_w + 2.0);
                        let val_y = y_of(*v);
                        let (mut top_y, bottom_y) = (val_y.min(zero_y), val_y.max(zero_y));
                        let full_h = (bottom_y - top_y).max(1.0);
                        let h = (full_h * (0.08 + 0.92 * entry_progress)).max(1.0) + lift;
                        if *v >= 0. {
                            top_y = bottom_y - h;
                        } else {
                            top_y = zero_y;
                        }
                        let corners = if *v >= 0. {
                            gpui::Corners {
                                top_left: px(4.),
                                top_right: px(4.),
                                bottom_left: px(0.),
                                bottom_right: px(0.),
                            }
                        } else {
                            gpui::Corners {
                                top_left: px(0.),
                                top_right: px(0.),
                                bottom_left: px(4.),
                                bottom_right: px(4.),
                            }
                        };
                        window.paint_quad(
                            gpui::fill(
                                gpui::Bounds::new(shift(origin, bar_x, top_y), gpui::size(px(bar_w), px(h))),
                                color,
                            )
                            .corner_radii(corners),
                        );
                    }
                }
            }
        }
        crate::chart::Kind::Line => {
            let line_reveal_progress = if reduced {
                1.0
            } else {
                crate::motion::ease_out((entry_elapsed_ms / 280.0).clamp(0.0, 1.0))
            };
            let max_reveal_x = left + plot_w * line_reveal_progress;

            for (si, series) in chart.series.iter().enumerate() {
                if !active_series.get(si).copied().unwrap_or(true) {
                    continue;
                }
                let color = series_color(t, si, series.highlight);
                let points: Vec<Option<(f32, f32)>> = series
                    .values
                    .iter()
                    .enumerate()
                    .map(|(ix, v)| v.map(|v| (left + slot * (ix as f32 + 0.5), y_of(v))))
                    .collect();

                for run in points.split(Option::is_none) {
                    let pts: Vec<(f32, f32)> = run.iter().flatten().copied().collect();
                    if pts.len() < 2 {
                        continue;
                    }
                    let curve_pts = crate::chart::monotone_spline(&pts);

                    // Optional faint area fill for series 0 at ~6% opacity
                    if si == 0 && curve_pts[0].0 <= max_reveal_x {
                        let mut area = gpui::PathBuilder::fill();
                        area.move_to(shift(origin, curve_pts[0].0, zero_y));
                        let mut last_x = curve_pts[0].0;
                        for &(x, y) in &curve_pts {
                            if x > max_reveal_x {
                                break;
                            }
                            area.line_to(shift(origin, x, y));
                            last_x = x;
                        }
                        area.line_to(shift(origin, last_x, zero_y));
                        area.close();
                        if let Ok(p) = area.build() {
                            window.paint_path(p, color.opacity(0.06));
                        }
                    }

                    // Stroke curve
                    if curve_pts[0].0 <= max_reveal_x {
                        let mut stroke = gpui::PathBuilder::stroke(px(1.75));
                        stroke.move_to(shift(origin, curve_pts[0].0, curve_pts[0].1));
                        for &(x, y) in &curve_pts[1..] {
                            if x > max_reveal_x {
                                break;
                            }
                            stroke.line_to(shift(origin, x, y));
                        }
                        if let Ok(p) = stroke.build() {
                            window.paint_path(p, color);
                        }
                    }
                }
            }

            // Hover guide and dot with smooth fade and scaling
            let guide_alpha = if menu_open {
                0.0
            } else if reduced {
                if effective_hover.is_some() { 1.0 } else { 0.0 }
            } else {
                ((1.0 - state.hover_dim) / 0.45).clamp(0.0, 1.0)
            };

            if !menu_open && guide_alpha > 0.01 && let Some(ix) = effective_hover.or(state.tip_last_group) {
                let gx = left + slot * (ix as f32 + 0.5);
                window.paint_quad(gpui::fill(
                    gpui::Bounds::new(
                        shift(origin, gx - 0.5, top),
                        gpui::size(px(1.0), px(plot_h)),
                    ),
                    c(t.text).opacity(0.20 * guide_alpha),
                ));

                for (si, series) in chart.series.iter().enumerate() {
                    if !active_series.get(si).copied().unwrap_or(true) {
                        continue;
                    }
                    if let Some(Some(v)) = series.values.get(ix) {
                        let gy = y_of(*v);
                        let dot_color = series_color(t, si, series.highlight);
                        let r = 2.0 + 1.0 * guide_alpha;
                        window.paint_quad(
                            gpui::fill(
                                gpui::Bounds::new(
                                    shift(origin, gx - r, gy - r),
                                    gpui::size(px(r * 2.), px(r * 2.)),
                                ),
                                dot_color.opacity(guide_alpha),
                            )
                            .corner_radii(px(r)),
                        );
                    }
                }
            }
        }
    }

    // Keyboard support: Left/Right to step between groups
    let r_id = row_id.to_string();
    let n_groups = chart.labels.len();
    window.on_key_event(move |event: &gpui::KeyDownEvent, phase, window, _| {
        if phase != gpui::DispatchPhase::Bubble {
            return;
        }
        let key = event.keystroke.key.as_str();
        if key == "left" || key == "arrowleft" || key == "right" || key == "arrowright" {
            CHART_STATES.with(|states| {
                let mut map = states.borrow_mut();
                if let Some(state) = map.get_mut(&r_id) {
                    let has_focus_or_hover = state.keyboard_focused || state.hovered_group.is_some() || state.dev_hovered.is_some();
                    if has_focus_or_hover {
                        let cur = state.hovered_group.unwrap_or(0);
                        let next = if key == "left" || key == "arrowleft" {
                            (cur + n_groups - 1) % n_groups
                        } else {
                            (cur + 1) % n_groups
                        };
                        state.keyboard_focused = true;
                        state.hovered_group = Some(next);
                        state.dev_hovered = None;
                        state.tip_is_visible = true;
                        state.tip_last_group = Some(next);
                        state.tip_opacity = 1.0;
                        state.tip_opacity_target = 1.0;
                        state.tip_curr_x = state.tip_target_x; // instant jump, no animation per Emil rule
                        window.refresh();
                    }
                }
            });
        }
    });

    let r_id_mouse = row_id.to_string();
    let hb_mouse = hitbox.clone();
    window.on_mouse_event(move |_: &gpui::MouseDownEvent, phase, window, _| {
        if phase == gpui::DispatchPhase::Bubble && hb_mouse.is_hovered(window) {
            CHART_STATES.with(|states| {
                if let Some(state) = states.borrow_mut().get_mut(&r_id_mouse) {
                    state.keyboard_focused = true;
                }
            });
            window.refresh();
        }
    });

    // Tooltip handling
    if !menu_open
        && state.tip_opacity > 0.005
        && let Some(ix) = effective_hover.or(state.tip_last_group)
        && let Some((name, rows)) = crate::chart::hover_rows(chart, ix)
    {
        // Filter rows by active series
        let active_rows: Vec<_> = rows
            .into_iter()
            .filter(|(si, _, _)| active_series.get(*si).copied().unwrap_or(true))
            .collect();

        if !active_rows.is_empty() {
            paint_chart_tip(
                &name,
                &active_rows,
                &mut state,
                origin,
                left,
                top,
                plot_w,
                slot,
                ix,
                reduced,
                now,
                &mut needs_refresh,
                t,
                window,
                cx,
            );
        }
    }

    // Persist state updates
    CHART_STATES.with(|states| {
        states.borrow_mut().insert(row_id.to_string(), state);
    });

    if needs_refresh {
        window.refresh();
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_chart_tip(
    name: &str,
    rows: &[(usize, String, String)],
    state: &mut ChartState,
    origin: gpui::Point<Pixels>,
    left: f32,
    top: f32,
    plot_w: f32,
    slot: f32,
    ix: usize,
    reduced: bool,
    now: Instant,
    needs_refresh: &mut bool,
    t: Tokens,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) {
    let tip_alpha = state.tip_opacity;
    if tip_alpha <= 0.005 {
        return;
    }
    let dy = if reduced || state.keyboard_focused { 0. } else { (1.0 - tip_alpha) * 3.0 };

    const PAD_Y: f32 = 8.;
    const PAD_X: f32 = 10.;
    const ROW: f32 = 18.;
    const SWATCH: f32 = 7.;
    const GAP: f32 = 12.;

    let shape = |text: &str, size: f32, color: Hsla, bold: bool, window: &mut gpui::Window| {
        let mut font = gpui::font(crate::style::SANS);
        if bold {
            font.weight = gpui::FontWeight::SEMIBOLD;
        }
        font.features = gpui::FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)]));
        let run = TextRun {
            len: text.len(),
            font,
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        window.text_system().shape_line(
            SharedString::from(text.to_string()),
            px(size),
            &[run],
            None,
        )
    };

    let max_w = (plot_w - 16.).max(130.);
    let value_w = rows
        .iter()
        .map(|(_, _, v)| f32::from(shape(v, 11.5, c(t.text), true, window).width))
        .fold(0., f32::max);
    let name_room = max_w - 2. * PAD_X - SWATCH - 6. - GAP - value_w;

    let fit = |text: &str, window: &mut gpui::Window| -> String {
        if f32::from(shape(text, 11.5, c(t.text_muted), false, window).width) <= name_room {
            return text.to_string();
        }
        let mut cut: String = text.to_string();
        while !cut.is_empty() {
            cut.pop();
            let candidate = format!("{}…", cut.trim_end());
            if f32::from(shape(&candidate, 11.5, c(t.text_muted), false, window).width) <= name_room {
                return candidate;
            }
        }
        "…".to_string()
    };

    let names: Vec<String> = rows.iter().map(|(_, n, _)| fit(n, window)).collect();
    let name_w = names
        .iter()
        .map(|n| f32::from(shape(n, 11.5, c(t.text_muted), false, window).width))
        .fold(0., f32::max);
    let head_w = f32::from(shape(name, 12., c(t.text), true, window).width);
    let body_w = SWATCH + 6. + name_w + GAP + value_w;
    let wide = (head_w.max(body_w) + 2. * PAD_X).clamp(130., max_w);
    let high = PAD_Y * 2. + ROW * (rows.len() + 1) as f32;

    let at = left + slot * (ix as f32 + 0.5);
    let mut target_x = at + 12.;
    if target_x + wide > left + plot_w {
        target_x = (at - wide - 12.).max(left);
    }

    if (state.tip_target_x - target_x).abs() > 0.5 {
        if state.tip_start_x <= 0. || state.tip_curr_x <= 0. || state.keyboard_focused || reduced {
            state.tip_start_x = target_x;
            state.tip_curr_x = target_x;
            state.tip_target_x = target_x;
            state.tip_move_start = now;
        } else {
            state.tip_start_x = state.tip_curr_x;
            state.tip_target_x = target_x;
            state.tip_move_start = now;
        }
    }

    let x = if reduced || state.keyboard_focused {
        target_x
    } else {
        let t_move = (now.duration_since(state.tip_move_start).as_secs_f32() / 0.12).clamp(0.0, 1.0);
        let ease = crate::motion::ease_out(t_move);
        let cur = state.tip_start_x + (state.tip_target_x - state.tip_start_x) * ease;
        state.tip_curr_x = cur;
        if t_move < 1.0 {
            *needs_refresh = true;
        }
        cur
    };
    let y = top + 4. + dy;

    // Minimal shadow
    window.paint_quad(
        gpui::fill(
            gpui::Bounds::new(shift(origin, x, y + 2.), gpui::size(px(wide), px(high))),
            c(t.shadow).opacity(0.18 * tip_alpha),
        )
        .corner_radii(px(9.)),
    );

    // Solid surface one tone step up (c(t.surface)), no border, radius 8px
    window.paint_quad(
        gpui::fill(
            gpui::Bounds::new(shift(origin, x, y), gpui::size(px(wide), px(high))),
            c(t.surface).opacity(tip_alpha),
        )
        .corner_radii(px(8.)),
    );

    let line_y = |row: usize| y + PAD_Y + row as f32 * ROW;
    let _ = shape(name, 12., c(t.text).opacity(tip_alpha), true, window).paint(
        shift(origin, x + PAD_X, line_y(0)),
        px(ROW),
        TextAlign::Left,
        None,
        window,
        cx,
    );

    for (row, ((si, _, value), label)) in rows.iter().zip(&names).enumerate() {
        let ly = line_y(row + 1);
        window.paint_quad(
            gpui::fill(
                gpui::Bounds::new(
                    shift(origin, x + PAD_X, ly + (ROW - SWATCH) / 2.),
                    gpui::size(px(SWATCH), px(SWATCH)),
                ),
                series_color(t, *si, false).opacity(tip_alpha),
            )
            .corner_radii(px(2.)),
        );
        let _ = shape(label, 11.5, c(t.text_muted).opacity(tip_alpha), false, window).paint(
            shift(origin, x + PAD_X + SWATCH + 6., ly),
            px(ROW),
            TextAlign::Left,
            None,
            window,
            cx,
        );
        let v = shape(value, 11.5, c(t.text).opacity(tip_alpha), true, window);
        let vx = x + wide - PAD_X - f32::from(v.width);
        let _ = v.paint(
            shift(origin, vx, ly),
            px(ROW),
            TextAlign::Left,
            None,
            window,
            cx,
        );
    }
}

/// A mermaid diagram scaled to fit its bounds, centered. Inline the bounds
/// already have its aspect ratio; the lightbox hands it the whole window.
pub fn mermaid_canvas(
    source: String,
    t: Tokens,
    view: &'static str,
    on_action: OnRowAction,
) -> gpui::Canvas<Vec<(gpui::Hitbox, String)>> {
    // Where the diagram lands in `bounds`: scaled to fit, centered in lightbox, top-anchored inline.
    let fit = move |source: &str, bounds: gpui::Bounds<Pixels>| {
        let (w, h) = crate::mermaid::size(source)?;
        let s = if view == "inline" {
            crate::mermaid::inline_scale((w, h), f32::from(bounds.size.width))
        } else {
            (f32::from(bounds.size.width) / w).min(f32::from(bounds.size.height) / h)
        };
        let size = gpui::size(px(w * s), px(h * s));
        let at = if view == "inline" {
            gpui::Bounds::new(
                gpui::point(
                    bounds.origin.x + (bounds.size.width - size.width) / 2.,
                    bounds.origin.y,
                ),
                size,
            )
        } else {
            gpui::Bounds::new(
                bounds.center() - gpui::point(size.width / 2., size.height / 2.),
                size,
            )
        };
        Some((s, at))
    };
    let at_source = source.clone();
    gpui::canvas(
        // One hitbox per node, so a node can be clicked.
        move |bounds, window, _| {
            let Some((s, at)) = fit(&at_source, bounds) else {
                return Vec::new();
            };
            crate::mermaid::nodes(&at_source, &t)
                .into_iter()
                .map(|(label, x, y, w, h)| {
                    let node = gpui::Bounds::new(
                        shift(at.origin, x * s, y * s),
                        gpui::size(px(w * s), px(h * s)),
                    );
                    (
                        window.insert_hitbox(node, gpui::HitboxBehavior::Normal),
                        label,
                    )
                })
                .collect()
        },
        move |bounds, nodes, window, _| {
            clickable_nodes(nodes, on_action.clone(), window);
            let Some((s, at)) = fit(&source, bounds) else {
                return;
            };
            let Some((w, _)) = crate::mermaid::size(&source) else {
                return;
            };
            let raster = crate::mermaid::raster(&source, &t, w * s * window.scale_factor(), view);
            if let Some(stale) = raster.stale {
                let _ = window.drop_image(stale);
            }
            if raster.pending {
                window.request_animation_frame();
            }
            if let Some((_, image)) = raster.image {
                let _ = window.paint_image(at, Default::default(), image, 0, false);
            }
        },
    )
}

/// A small text button in a diagram's header bar (and the lightbox toolbar).
pub fn bar_button(
    id: String,
    icon: Icon,
    label: &'static str,
    color: u32,
    t: Tokens,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(ElementId::Name(id.into()))
        .h(zpx(22.))
        .px_2()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .rounded_full()
        .cursor_pointer()
        .text_color(c(color))
        .hover(|s| s.text_color(c(t.text)))
        .hover_fade(c(t.hover), Some(c(t.selected)))
        .active(|s| s.opacity(0.85))
        .child(icon.el(zpx(11.)))
        .when(!label.is_empty(), |d| d.child(label))
}

/// The text of the message `items[ix]` ends, as Markdown; `None` when the
/// next row is still the same message or it has no text (tools, images).
pub fn message_text(items: &[TranscriptRowItem], ix: usize) -> Option<String> {
    let id = &items.get(ix)?.message_id;
    if items.get(ix + 1).is_some_and(|next| next.message_id == *id) {
        return None;
    }
    let start = items[..ix]
        .iter()
        .rposition(|r| r.message_id != *id)
        .map_or(0, |p| p + 1);
    let parts: Vec<String> = items[start..=ix]
        .iter()
        .filter(|r| r.leaving.is_none())
        .filter_map(|r| block_markdown(&r.block))
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

fn block_markdown(block: &TranscriptBlock) -> Option<String> {
    let row = |cells: &[Inline]| {
        format!(
            "| {} |",
            cells
                .iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>()
                .join(" | ")
        )
    };
    Some(match block {
        TranscriptBlock::Paragraph { content, .. } => content.clone(),
        TranscriptBlock::Heading { level, content, .. } => {
            format!("{} {content}", "#".repeat(*level as usize))
        }
        TranscriptBlock::Quote { content, .. } => content
            .lines()
            .map(|l| format!("> {l}"))
            .collect::<Vec<_>>()
            .join("\n"),
        TranscriptBlock::Code {
            language, content, ..
        } => format!("```{}\n{content}\n```", language.as_deref().unwrap_or("")),
        TranscriptBlock::List { items, .. } => items
            .iter()
            .map(|it| {
                let task = match it.task {
                    Some(true) => "[x] ",
                    Some(false) => "[ ] ",
                    None => "",
                };
                format!("{}{} {task}{}", "  ".repeat(it.depth), it.marker, it.text)
            })
            .collect::<Vec<_>>()
            .join("\n"),
        TranscriptBlock::Table { header, rows, .. } => std::iter::once(row(header))
            .chain(std::iter::once(format!(
                "|{}",
                " --- |".repeat(header.len().max(1))
            )))
            .chain(rows.iter().map(|r| row(r)))
            .collect::<Vec<_>>()
            .join("\n"),
        TranscriptBlock::Rule { .. } => "---".into(),
        TranscriptBlock::Chart {
            replaces: Some(table),
            ..
        } => return block_markdown(table),
        TranscriptBlock::Chart { chart, .. } => return Some(chart.to_markdown()),
        // The narration between steps is part of the trace, not the answer:
        // Copy gives the same text whether the fold is open or closed.
        TranscriptBlock::Interim(_) => return None,
        _ => return None,
    })
}

/// The Copy button under a message; `text` from [`message_text`]. Shown while
/// the message is hovered (and while it says Copied); hidden, it keeps its room.
pub fn message_copy(
    item: &TranscriptRowItem,
    text: String,
    shown: bool,
    rc: &RowContext,
) -> AnyElement {
    let key = format!("{}#message", item.message_id);
    let copied = rc.copied.as_deref() == Some(key.as_str());
    let on_action = rc.on_action.clone();
    div()
        .w_full()
        .px_4()
        .flex()
        .when(item.role == "user", |d| d.justify_end())
        .when(!shown && !copied, |d| d.invisible())
        .child(
            bar_button(
                format!("copy-{key}"),
                if copied { Icon::Check } else { Icon::Copy },
                if copied { "Copied" } else { "Copy" },
                if copied {
                    rc.tokens.success
                } else {
                    rc.tokens.text_faint
                },
                rc.tokens,
            )
            .t_caption()
            .on_click(move |_, _, cx| {
                on_action(
                    RowAction::Copy {
                        row_id: key.clone(),
                        content: text.clone(),
                    },
                    cx,
                )
            }),
        )
        .into_any_element()
}

/// "Copied" key for a diagram's image copy; its code copy uses the row id.
pub fn image_copy_key(row_id: &str) -> String {
    format!("{row_id}#image")
}

fn diagram_header(
    item: &TranscriptRowItem,
    title: &str,
    copied: bool,
    menu_open: bool,
    menu_ix: usize,
    keyboard_opened: bool,
    group: gpui::SharedString,
    on_action: OnRowAction,
    row_id: String,
    source: String,
    t: Tokens,
) -> gpui::Div {
    let id = item.row_id();
    let badge = div()
        .min_w_0()
        .flex_1()
        .flex()
        .flex_row()
        .items_center()
        .gap(zpx(8.))
        .t_caption()
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(c(t.text_muted))
        .child(Icon::Network.el(zpx(14.)))
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_size(zpx(12.))
                .child(title.to_string()),
        );

    let (open_id, open_src) = (row_id.clone(), source.clone());
    let open = on_action.clone();
    let expand_btn = div()
        .id(ElementId::Name(format!("expand-{}", id).into()))
        .flex_none()
        .size(zpx(28.))
        .rounded(RADIUS_SM)
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .text_color(c(t.text_muted))
        .hover_fade(c(t.hover), Some(c(t.selected)))
        .hover(move |s| s.text_color(c(t.text)))
        .active(|s| s.opacity(0.85))
        .on_click(move |_, _, cx| {
            open(
                RowAction::OpenDiagram {
                    row_id: open_id.clone(),
                    source: open_src.clone(),
                },
                cx,
            );
        })
        .tooltip(move |_, cx| cx.new(|_| crate::view::Tip::new("Expand", t)).into())
        .tooltip_show_delay(crate::view::tooltip::delay())
        .child(Icon::Expand.el(zpx(13.)));

    let copy_trigger = copy_dropdown(
        id,
        row_id,
        source,
        copied,
        menu_open,
        menu_ix,
        keyboard_opened,
        t,
        on_action,
    );

    let toolbar_actions = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(zpx(4.))
        .child(expand_btn)
        .child(copy_trigger);

    let reveal = reveal_on_hover_curved(
        format!("diagram-actions-{}", id),
        group,
        if menu_open { 1. } else { 0. },
        1.,
        std::time::Duration::from_millis(150),
        std::time::Duration::from_millis(100),
        toolbar_actions,
    );

    div()
        .h(zpx(36.))
        .px_4()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .child(badge)
        .child(reveal)
}

fn copy_dropdown(
    id: &str,
    row_id: String,
    source: String,
    copied: bool,
    menu_open: bool,
    menu_ix: usize,
    keyboard_opened: bool,
    t: Tokens,
    on_action: OnRowAction,
) -> impl IntoElement {
    let action_copy_code = on_action.clone();
    let action_copy_img = on_action.clone();
    let action_close = on_action.clone();
    let action_toggle = on_action.clone();
    let (c_row, c_src) = (row_id.clone(), source.clone());
    let (i_row, i_src) = (row_id.clone(), source.clone());
    let t_row = row_id;

    let trigger = div()
        .id(ElementId::Name(format!("copy-trigger-{id}").into()))
        .flex_none()
        .size(zpx(28.))
        .rounded(RADIUS_SM)
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .text_color(if copied { c(t.accent) } else { c(t.text_muted) })
        .hover_fade(c(t.hover), Some(c(t.selected)))
        .hover(move |s| s.text_color(c(t.text)))
        .active(|s| s.opacity(0.85))
        .on_click(move |_, _, cx| {
            action_toggle(RowAction::ToggleDiagramMenu { row_id: t_row.clone() }, cx);
        })
        .tooltip(move |_, cx| {
            let tip = if copied { "Copied" } else { "Copy" };
            cx.new(|_| crate::view::Tip::new(tip, t)).into()
        })
        .tooltip_show_delay(crate::view::tooltip::delay())
        .child(if copied {
            div()
                .with_animation(
                    ElementId::Name(format!("inline-copy-check-{id}").into()),
                    gpui::Animation::new(std::time::Duration::from_millis(150))
                        .with_easing(crate::motion::ease_out),
                    |el, delta| el.opacity(delta),
                )
                .child(Icon::Check.el(zpx(13.)))
        } else {
            div()
                .with_animation(
                    ElementId::Name(format!("inline-copy-icon-{id}").into()),
                    gpui::Animation::new(std::time::Duration::from_millis(150))
                        .with_easing(crate::motion::ease_out),
                    |el, delta| el.opacity(delta),
                )
                .child(Icon::Copy.el(zpx(13.)))
        });

    let selected_code = keyboard_opened && menu_ix == 0;
    let selected_img = keyboard_opened && menu_ix == 1;

    let menu = if menu_open {
        let menu_el = div()
            .id(ElementId::Name(format!("copy-menu-{id}").into()))
            .absolute()
            .top(zpx(32.))
            .right_0()
            .w(zpx(136.))
            .occlude()
            .p(zpx(3.))
            .flex()
            .flex_col()
            .gap_0p5()
            .bg(c(t.floating))
            .rounded(RADIUS_MD)
            .border_1()
            .border_color(c(t.border).opacity(0.20))
            .shadow(vec![gpui::BoxShadow {
                color: c(t.shadow).opacity(0.35),
                offset: gpui::point(zpx(0.), zpx(4.)),
                blur_radius: zpx(10.),
                spread_radius: zpx(-2.),
                inset: false,
            }])
            .on_mouse_down_out(move |_, _, cx| {
                action_close(RowAction::CloseDiagramMenu, cx);
            })
            .child(
                dropdown_row(
                    format!("copy-code-{id}"),
                    Icon::Code,
                    "Copy as code",
                    selected_code,
                    t,
                    move |_, _, cx| {
                        action_copy_code(RowAction::Copy {
                            row_id: c_row.clone(),
                            content: c_src.clone(),
                        }, cx);
                    },
                ),
            )
            .child(
                dropdown_row(
                    format!("copy-image-{id}"),
                    Icon::Image,
                    "Copy as image",
                    selected_img,
                    t,
                    move |_, _, cx| {
                        action_copy_img(RowAction::CopyImage {
                            row_id: i_row.clone(),
                            source: i_src.clone(),
                            tokens: t,
                        }, cx);
                    },
                ),
            );

        Some(if keyboard_opened {
            menu_el.into_any_element()
        } else {
            menu_el
                .with_animation(
                    ElementId::Name(format!("copy-menu-in-{id}").into()),
                    gpui::Animation::new(std::time::Duration::from_millis(150))
                        .with_easing(crate::motion::ease_out),
                    |el, delta| {
                        el.opacity(delta)
                            .top(zpx(32. - 3. * (1. - delta)))
                    },
                )
                .into_any_element()
        })
    } else {
        None
    };

    div()
        .relative()
        .flex_none()
        .child(trigger)
        .children(menu)
}

fn dropdown_row(
    id: String,
    icon: Icon,
    label: &'static str,
    selected: bool,
    t: Tokens,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(ElementId::Name(id.into()))
        .h(zpx(26.))
        .px_2()
        .rounded(RADIUS_SM)
        .flex()
        .flex_row()
        .items_center()
        .gap(zpx(6.))
        .cursor_pointer()
        .text_size(zpx(12.))
        .font_weight(gpui::FontWeight::NORMAL)
        .text_color(c(t.text))
        .when(selected, |d| d.bg(c(t.selected)))
        .when(!selected, |d| d.hover_fade(c(t.hover), Some(c(t.selected))))
        .active(|s| s.opacity(0.85))
        .on_click(on_click)
        .child(
            div()
                .w(zpx(14.))
                .flex_none()
                .flex()
                .justify_center()
                .text_color(c(t.text_muted))
                .child(icon.el(zpx(12.))),
        )
        .child(div().whitespace_nowrap().child(label))
}

/// Header bar: subtle title badge on the left, floating segmented pill on the right.
fn diagram_bar(
    item: &TranscriptRowItem,
    label: &str,
    copied: Option<&str>,
    on_action: OnRowAction,
    row_id: String,
    text: String,
    mermaid: Option<Tokens>,
    t: Tokens,
) -> gpui::Div {
    let id = item.row_id();
    let code_copied = copied == Some(row_id.as_str());
    let image_copied = copied == Some(image_copy_key(&row_id).as_str());
    let tint = |done: bool| if done { t.success } else { t.text_faint };

    let display_label = if label == "mermaid" {
        "Diagram".to_string()
    } else {
        label.to_string()
    };
    let badge = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1p5()
        .child(Icon::Branch.el(zpx(12.)))
        .child(
            div()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(c(t.text_muted))
                .child(display_label),
        );

    let actions = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_0p5()
        .p_0p5()
        .rounded_full()
        .bg(c(t.surface))
        .border_1()
        .border_color(c(t.border));

    let actions = match mermaid {
        Some(tokens) => {
            let (open, image) = (on_action.clone(), on_action.clone());
            let (open_id, open_src, image_id, image_src) =
                (row_id.clone(), text.clone(), row_id.clone(), text.clone());
            actions
                .child(
                    bar_button(
                        format!("expand-{id}"),
                        Icon::Expand,
                        "Expand",
                        t.text_faint,
                        t,
                    )
                    .on_click(move |_, _, cx| {
                        open(
                            RowAction::OpenDiagram {
                                row_id: open_id.clone(),
                                source: open_src.clone(),
                            },
                            cx,
                        )
                    }),
                )
                .child(
                    bar_button(
                        format!("copy-image-{id}"),
                        if image_copied {
                            Icon::Check
                        } else {
                            Icon::Copy
                        },
                        if image_copied { "Copied" } else { "Copy image" },
                        tint(image_copied),
                        t,
                    )
                    .on_click(move |_, _, cx| {
                        image(
                            RowAction::CopyImage {
                                row_id: image_id.clone(),
                                source: image_src.clone(),
                                tokens,
                            },
                            cx,
                        )
                    }),
                )
        }
        None => actions,
    };
    let code_label = match (code_copied, mermaid.is_some()) {
        (true, _) => "Copied",
        (false, true) => "Copy code",
        (false, false) => "Copy",
    };
    let actions = actions.child(
        bar_button(
            format!("copy-{id}"),
            if code_copied { Icon::Check } else { Icon::Copy },
            code_label,
            tint(code_copied),
            t,
        )
        .on_click(move |_, _, cx| {
            on_action(
                RowAction::Copy {
                    row_id: row_id.clone(),
                    content: text.clone(),
                },
                cx,
            )
        }),
    );

    div()
        .h(zpx(32.))
        .px_3()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .t_caption()
        .text_color(c(t.text_faint))
        .child(badge)
        .child(actions)
}

fn paint_diagram(
    diagram: &crate::diagram::Diagram,
    placed: &crate::diagram::Placed,
    origin: gpui::Point<Pixels>,
    t: Tokens,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) {
    let ink = c(t.text);
    let faint = c(t.text_faint);
    let fill = c(t.surface);
    let border = c(t.border);
    let accent = c(t.accent);
    let box_of = |id: &str| placed.nodes.get(id).copied();
    for edge in &diagram.edges {
        let Some((x1, y1, w1, h1)) = box_of(&edge.from) else {
            continue;
        };
        let Some((x2, y2, w2, h2)) = box_of(&edge.to) else {
            continue;
        };
        let back = y2 + h2 <= y1 + 4.;
        let vertical =
            !back && y2 >= y1 + h1 - 4. && (x1 - x2).abs() < (y2 - (y1 + h1)).abs().max(8.);
        let (ax, ay, bx, by, dir) = if back {
            (
                x1,
                y1 + h1 / 2.,
                x2,
                y2 + h2 / 2.,
                crate::diagram::Dir::Left,
            )
        } else if vertical {
            (
                x1 + w1 / 2.,
                y1 + h1,
                x2 + w2 / 2.,
                y2,
                crate::diagram::Dir::Down,
            )
        } else if x2 >= x1 {
            (
                x1 + w1,
                y1 + h1 / 2.,
                x2,
                y2 + h2 / 2.,
                crate::diagram::Dir::Right,
            )
        } else {
            (
                x1,
                y1 + h1 / 2.,
                x2 + w2,
                y2 + h2 / 2.,
                crate::diagram::Dir::Left,
            )
        };
        if back {
            side_return(origin, ax, ay, bx, by, accent, window);
        } else {
            elbow(origin, ax, ay, bx, by, vertical, accent, window);
        }
        arrow_head(origin, bx, by, dir, accent, window);
        if !edge.label.is_empty() {
            paint_label(
                &edge.label,
                origin,
                (ax + bx) / 2. - 16.,
                (ay + by) / 2. - 14.,
                faint,
                window,
                cx,
            );
        }
    }
    for node in &diagram.nodes {
        let Some((x, y, w, h)) = box_of(&node.id) else {
            continue;
        };
        window.paint_quad(
            gpui::fill(
                gpui::Bounds::new(shift(origin, x, y), gpui::size(px(w), px(h))),
                fill,
            )
            .corner_radii(px(8.))
            .border_widths(px(1.))
            .border_color(border),
        );
        paint_label(
            &node.label,
            origin,
            x + 8.,
            y + (h - 14.) / 2.,
            ink,
            window,
            cx,
        );
    }
}

/// Diagram nodes as click targets: a pointer over each, and a click starts a
/// question about that node in the composer.
fn clickable_nodes(
    nodes: Vec<(gpui::Hitbox, String)>,
    on_action: OnRowAction,
    window: &mut gpui::Window,
) {
    for (hitbox, _) in &nodes {
        window.set_cursor_style(gpui::CursorStyle::PointingHand, hitbox);
    }
    window.on_mouse_event(move |event: &gpui::MouseDownEvent, phase, window, cx| {
        if phase != gpui::DispatchPhase::Bubble || event.button != gpui::MouseButton::Left {
            return;
        }
        if let Some((_, label)) = nodes.iter().find(|(hitbox, _)| hitbox.is_hovered(window)) {
            on_action(
                RowAction::AskAbout {
                    label: label.clone(),
                },
                cx,
            );
        }
    });
}

fn shift(origin: gpui::Point<Pixels>, x: f32, y: f32) -> gpui::Point<Pixels> {
    gpui::point(origin.x + px(x), origin.y + px(y))
}

/// A cycle edge goes out the left of the source and into the left of the target,
/// a few pixels off the column, instead of sweeping the whole pane.
fn side_return(
    origin: gpui::Point<Pixels>,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    color: Hsla,
    window: &mut gpui::Window,
) {
    let rail = x1.min(x2) - 14.;
    let mut path = gpui::PathBuilder::stroke(px(1.25));
    path.move_to(shift(origin, x1, y1));
    path.line_to(shift(origin, rail, y1));
    path.line_to(shift(origin, rail, y2));
    path.line_to(shift(origin, x2, y2));
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

fn elbow(
    origin: gpui::Point<Pixels>,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    vertical: bool,
    color: Hsla,
    window: &mut gpui::Window,
) {
    let mut path = gpui::PathBuilder::stroke(px(1.25));
    path.move_to(shift(origin, x1, y1));
    if vertical {
        let mid = (y1 + y2) / 2.;
        path.line_to(shift(origin, x1, mid));
        path.line_to(shift(origin, x2, mid));
    } else {
        let mid = (x1 + x2) / 2.;
        path.line_to(shift(origin, mid, y1));
        path.line_to(shift(origin, mid, y2));
    }
    path.line_to(shift(origin, x2, y2));
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

fn arrow_head(
    origin: gpui::Point<Pixels>,
    x: f32,
    y: f32,
    dir: crate::diagram::Dir,
    color: Hsla,
    window: &mut gpui::Window,
) {
    let (dx, dy) = match dir {
        crate::diagram::Dir::Right => (1., 0.),
        crate::diagram::Dir::Left => (-1., 0.),
        crate::diagram::Dir::Down => (0., 1.),
        crate::diagram::Dir::Up => (0., -1.),
    };
    let back = (x - dx * 7., y - dy * 7.);
    let mut path = gpui::PathBuilder::fill();
    path.move_to(shift(origin, x, y));
    path.line_to(shift(origin, back.0 - dy * 3.5, back.1 + dx * 3.5));
    path.line_to(shift(origin, back.0 + dy * 3.5, back.1 - dx * 3.5));
    path.close();
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

fn paint_label(
    text: &str,
    origin: gpui::Point<Pixels>,
    x: f32,
    y: f32,
    color: Hsla,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) {
    let text: String = text.chars().take(crate::diagram::LABEL_CHARS).collect();
    if text.is_empty() {
        return;
    }
    let run = TextRun {
        len: text.len(),
        font: gpui::font(MONO),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window
        .text_system()
        .shape_line(SharedString::from(text), px(12.), &[run], None);
    let _ = line.paint(
        shift(origin, x, y),
        px(16.),
        TextAlign::Left,
        None,
        window,
        cx,
    );
}

/// What a row is, for the space around it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flow {
    Text,
    Heading(u8),
    Trace,
    /// Narration inside an open fold, among the trace lines.
    Note,
    Block,
}

fn flow(block: &TranscriptBlock) -> Flow {
    match block {
        TranscriptBlock::Paragraph { .. }
        | TranscriptBlock::List { .. }
        | TranscriptBlock::Quote { .. } => Flow::Text,
        TranscriptBlock::Heading { level, .. } => Flow::Heading(*level),
        TranscriptBlock::ToolGroup { .. }
        | TranscriptBlock::Reasoning { .. }
        | TranscriptBlock::Plan { .. }
        | TranscriptBlock::TurnSummary { .. }
        | TranscriptBlock::Subagent { .. } => Flow::Trace,
        TranscriptBlock::Interim(_) => Flow::Note,
        _ => Flow::Block,
    }
}

/// Prose line height (px at zoom 1): 22.5 on the 14 px body, about 1.6.
pub const PROSE_LINE: f32 = 22.5;
/// Prose stops at a comfortable measure (about 85 characters); code,
/// tables and trace lines still use the whole column.
pub const PROSE_MAX_W: f32 = 660.;
pub const PROSE_COLLAPSED_MAX_W: f32 = 740.;

/// Space above a row, from what comes before it: paragraphs ~0.8em apart,
/// headings with more room above than below, trace lines tight among
/// themselves but clear of the text around them.
pub fn row_gap(prev: Option<&TranscriptRowItem>, cur: &TranscriptRowItem) -> f32 {
    if cur.role == "user" {
        return 12.;
    }
    let Some(prev) = prev else { return 4. };
    if prev.role != cur.role {
        return 14.;
    }
    let first = prev.message_id != cur.message_id
        && flow(&prev.block) != Flow::Trace
        && flow(&cur.block) != Flow::Trace;
    match (flow(&prev.block), flow(&cur.block)) {
        (_, Flow::Heading(level)) => match level {
            1 => 26.,
            2 => 22.,
            3 => 18.,
            _ => 14.,
        },
        (Flow::Heading(level), _) => match level {
            1 => 10.,
            2 => 8.,
            _ => 6.,
        },
        (Flow::Trace, Flow::Trace) => 2.,
        // Narration sits in the step list: a little air around it, tighter
        // than between paragraphs of the answer.
        (Flow::Trace | Flow::Note, Flow::Note) | (Flow::Note, Flow::Trace) => 6.,
        (Flow::Text, Flow::Text) if !first => 11.,
        _ => 14.,
    }
}

pub fn render_row(
    ix: usize,
    item: &TranscriptRowItem,
    prev: Option<&TranscriptRowItem>,
    rc: &RowContext,
) -> AnyElement {
    let reduced = rc.reduced;
    let rc = &RowContext {
        order: (ix as u64) << 20,
        ..rc.clone()
    };
    let gap = row_gap(prev, item);
    let row = render_row_body(item, rc).pt(zpx(gap));
    // The live header continues the waiting loader's line. Its words already
    // animate via StepSwap; a second row entrance would dim and shift both.
    if matches!(item.block, TranscriptBlock::TurnSummary { live: true, .. }) {
        return row.into_any_element();
    }
    let Some(look) = item.look(rc.now, reduced) else {
        return row.into_any_element();
    };
    // Timestamp-driven, in plain wrappers (no animation ids), so the row
    // keeps its identity: fade and rise inside, and the height gliding from
    // its measured natural height outside.
    let id = item.row_id().to_string();
    let heights = rc.row_heights.clone();
    let natural = heights
        .borrow()
        .get(&id)
        .copied()
        .or_else(|| match &item.block {
            TranscriptBlock::ToolGroup {
                tools, expanded, ..
            } if tools.len() == 1 && tools[0].is_receipt() => Some(
                32.0 + if *expanded {
                    receipt_body_height(&tools[0])
                } else {
                    0.0
                },
            ),
            TranscriptBlock::ToolGroup {
                id,
                tools,
                overrides,
                ..
            } => Some(tool_group_height(id, tools, overrides)),
            TranscriptBlock::Reasoning {
                content, expanded, ..
            } => Some(
                crate::trace_view::LINE_H
                    + if *expanded {
                        content.lines().count() as f32 * 18.0
                    } else {
                        0.0
                    },
            ),
            TranscriptBlock::Plan {
                entries,
                live,
                expanded,
                ..
            } => Some(
                crate::trace_view::LINE_H
                    * (1 + if expanded.unwrap_or(*live) {
                        entries.len()
                    } else {
                        0
                    }) as f32
                    + 4.0,
            ),
            TranscriptBlock::TurnSummary { .. } => Some(crate::trace_view::LINE_H),
            TranscriptBlock::Paragraph { content, .. } => {
                let lines = content.lines().count().max(1);
                Some((lines as f32) * 22.0 + 12.0)
            }
            TranscriptBlock::Interim(inner) => match inner.as_ref() {
                TranscriptBlock::Paragraph { content, .. } => {
                    Some((content.lines().count().max(1) as f32) * 22.0 + 12.0)
                }
                _ => None,
            },
            TranscriptBlock::Heading { level, .. } => Some(match level {
                1 => 52.0,
                2 => 44.0,
                _ => 38.0,
            }),
            _ => None,
        });
    let measure = gpui::canvas(
        move |bounds, _, _| {
            heights
                .borrow_mut()
                .insert(id, f32::from(bounds.size.height));
        },
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .w_full()
    .h_full();
    let inner = div()
        .relative()
        .flex_none()
        .w_full()
        .opacity(look.opacity)
        .top(zpx(look.dy))
        .child(row)
        .child(measure);
    let height = match natural {
        Some(h) => Some(h * look.height),
        // Entering, not measured yet: start closed. Leaving: one natural frame.
        None if item.leaving.is_none() => Some(0.),
        None => None,
    };
    div()
        .w_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .when_some(height, |d, h| d.h(px(h)))
        .child(inner)
        .into_any_element()
}

fn render_row_body(item: &TranscriptRowItem, rc: &RowContext) -> gpui::Stateful<gpui::Div> {
    if let TranscriptBlock::Interim(inner) = &item.block {
        // The same prose, muted: same type and wrap, so moving into the fold
        // only changes its color.
        let inner = TranscriptRowItem {
            block: inner.as_ref().clone(),
            ..item.clone()
        };
        let rc = RowContext {
            muted: true,
            ..rc.clone()
        };
        return render_row_body(&inner, &rc);
    }
    let (t, syntax) = (rc.tokens, rc.syntax);
    let role_is_user = item.role == "user";
    let role_is_error = item.role == "error";

    let is_resume_error = role_is_error && match &item.block {
        TranscriptBlock::Paragraph { content, .. } => content.contains("Couldn't resume this session"),
        _ => false,
    };
    if is_resume_error {
        let (on_retry, on_fresh) = (rc.on_action.clone(), rc.on_action.clone());
        return div()
            .id(ElementId::Name(format!("row-{}", item.row_id()).into()))
            .w_full()
            .px_4()
            .pb(zpx(2.))
            .child(
                div()
                    .w_full()
                    .p_4()
                    .rounded(RADIUS_LG)
                    .border_1()
                    .border_color(c(t.border))
                    .bg(c(t.surface))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_color(c(t.warning))
                                    .child(Icon::Alert.el(zpx(16.))),
                            )
                            .child(
                                div()
                                    .t_body()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(c(t.text))
                                    .child("Couldn't resume this session"),
                            ),
                    )
                    .child(
                        div()
                            .t_small()
                            .text_color(c(t.text_muted))
                            .child("The saved conversation could not be reloaded. Your message has been kept in the composer below."),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .pt_1()
                            .child(
                                div()
                                    .id("resume-retry-btn")
                                    .px_3()
                                    .py_1p5()
                                    .rounded(RADIUS_MD)
                                    .border_1()
                                    .border_color(c(t.border))
                                    .bg(c(t.surface))
                                    .hover(|s| s.bg(c(t.hover)))
                                    .active(|s| s.opacity(0.8))
                                    .cursor_pointer()
                                    .t_small()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(c(t.text))
                                    .child("Retry")
                                    .on_click(move |_, _, cx| on_retry(RowAction::ResumeRetry, cx)),
                            )
                            .child(
                                div()
                                    .id("resume-fresh-btn")
                                    .px_3()
                                    .py_1p5()
                                    .rounded(RADIUS_MD)
                                    .bg(c(t.accent))
                                    .hover(|s| s.opacity(0.9))
                                    .active(|s| s.opacity(0.8))
                                    .cursor_pointer()
                                    .t_small()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(c(t.text_on_accent))
                                    .child("Start fresh")
                                    .on_click(move |_, _, cx| on_fresh(RowAction::ResumeStartFresh, cx)),
                            ),
                    ),
            );
    }
    // Body text a touch dimmer than headings and bold, so they lead.
    let prose = |d: gpui::Div| {
        d.w_full()
            .max_w(zpx(rc.prose_max_w))
            .text_size(zpx(READING.size))
            .line_height(TranscriptState::prose_line_height())
            .text_color(if rc.muted {
                c(t.text_muted)
            } else {
                c(t.text).opacity(0.88)
            })
    };
    let content_elem = match &item.block {
        TranscriptBlock::Code {
            language,
            content,
            spans,
            closed,
            run,
            ..
        } => {
            if let Some(diagram) = crate::diagram::parse(language.as_deref(), content) {
                return render_diagram(item, language, content, &diagram, rc);
            }
            let is_mermaid_lang = matches!(
                language.as_deref().map(str::trim),
                Some("mermaid" | "diagram")
            );
            if is_mermaid_lang && *closed && crate::mermaid::renders(content, &t) {
                return render_mermaid(item, language, content, rc);
            }
            let mut props = fence_props(item, language, content, spans, *closed, run.is_some(), rc);
            let streaming_mermaid = is_mermaid_lang && !*closed;
            if is_mermaid_lang && *closed && !crate::mermaid::renders(content, &t) {
                props.variant = crate::code_block::CodeVariant::Destructive;
                props.title = Some(crate::mermaid::diagram_title(content));
                let line_count = content.lines().count().max(1);
                let mut variants = vec![crate::code_block::LineVariant::Default; line_count];
                if let Some((err_line, _)) = crate::mermaid::parse_error(content) {
                    if err_line < variants.len() {
                        variants[err_line] = crate::code_block::LineVariant::Removed;
                    }
                }
                props.line_variants = Some(variants);
            }
            let (on_action, row_id, text, run) = (
                rc.on_action.clone(),
                item.row_id().to_string(),
                content.clone(),
                run.clone(),
            );
            let more = props.expanded;
            let on_code: crate::code_block::OnCodeAction = std::rc::Rc::new(move |action, cx| {
                use crate::code_block::CodeAction;
                let toggle = |key: &str, open: bool| RowAction::Toggle {
                    row_id: format!("{row_id}~{key}={}", u8::from(!open)),
                };
                match action {
                    CodeAction::Copy { .. } => on_action(
                        RowAction::Copy {
                            row_id: row_id.clone(),
                            content: text.clone(),
                        },
                        cx,
                    ),
                    CodeAction::Insert { text } => {
                        on_action(RowAction::InsertInTerminal { text }, cx)
                    }
                    CodeAction::ShowMore => on_action(toggle("more", more), cx),
                    CodeAction::Run => {
                        if let Some(run) = &run {
                            on_action(
                                RowAction::Run {
                                    row_id: row_id.clone(),
                                    command: run.command.clone(),
                                    risky: run.risky,
                                },
                                cx,
                            );
                        }
                    }
                    CodeAction::ShowFull {
                        content,
                        title,
                        error,
                    }
                    | CodeAction::OpenViewer {
                        content,
                        title,
                        error,
                    } => {
                        on_action(
                            RowAction::OpenFullOutput {
                                title,
                                content,
                                error,
                            },
                            cx,
                        );
                    }
                }
            });
            div()
                .relative()
                .w_full()
                .my_1()
                .when(streaming_mermaid, |d| d.opacity(0.6))
                .child(crate::code_block::render(
                    &props,
                    t,
                    &syntax,
                    rc.viewport_h,
                    on_code,
                ))
                .children(rc.probe.as_ref().and_then(|p| p(RowProbe::Code)))
        }
        TranscriptBlock::Heading {
            level,
            content,
            spans,
            ..
        } => {
            let (size, line, weight) = match level {
                1 => (20., 28., gpui::FontWeight::SEMIBOLD),
                2 => (17., 24., gpui::FontWeight::SEMIBOLD),
                3 => (15., 22., gpui::FontWeight::SEMIBOLD),
                4 => (14., 21., gpui::FontWeight::SEMIBOLD),
                _ => (13., 20., gpui::FontWeight::MEDIUM),
            };
            div()
                .w_full()
                .max_w(zpx(rc.prose_max_w))
                .text_size(zpx(size))
                .line_height(zpx(line))
                .font_weight(weight)
                .text_color(c(if *level >= 5 || rc.muted { t.text_muted } else { t.text }))
                .child(inline_text(
                    format!("h-{}", item.row_id()),
                    content,
                    spans,
                    0,
                    rc,
                ))
        }
        // Unwrapped at the top of this fn.
        TranscriptBlock::Interim(_) => div(),
        TranscriptBlock::Paragraph { content, spans, .. } => prose(div()).child(inline_text(
            format!("p-{}", item.row_id()),
            content,
            spans,
            0,
            rc,
        )),
        TranscriptBlock::Quote { content, spans, .. } => prose(div())
            .pl_3()
            .border_l_2()
            .border_color(c(t.border))
            .text_color(c(t.text_muted))
            .child(inline_text(
                format!("q-{}", item.row_id()),
                content,
                spans,
                0,
                rc,
            )),
        TranscriptBlock::Rule { .. } => div()
            .w_full()
            .py_2()
            .child(div().w_full().h(px(1.)).bg(c(t.border))),
        TranscriptBlock::Table {
            align,
            header,
            rows,
            ..
        } => render_table(item, align, header, rows, rc),
        TranscriptBlock::List { items, .. } => prose(div())
            .flex()
            .flex_col()
            .gap(zpx(4.))
            .children(items.iter().enumerate().map(|(ix, it)| {
                let marker = match it.task {
                    Some(checked) => div()
                        .flex_none()
                        .mt(zpx(5.))
                        .size(zpx(13.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(crate::style::RADIUS_SM)
                        .border_1()
                        .border_color(c(if checked { t.accent } else { t.text_faint }))
                        .when(checked, |d| {
                            d.bg(c(t.accent))
                                .text_color(c(t.text_on_accent))
                                .child(Icon::Check.el(zpx(9.)))
                        })
                        .into_any_element(),
                    None => div()
                        .flex_none()
                        .min_w(zpx(16.))
                        .text_color(c(t.text_muted))
                        .child(list_marker(&it.marker, it.depth))
                        .into_any_element(),
                };
                div()
                    .flex()
                    .flex_row()
                    .gap(zpx(6.))
                    .pl(zpx(4. + 20.0 * it.depth as f32))
                    .child(marker)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .when(it.task == Some(true), |d| d.text_color(c(t.text_muted)))
                            .child(inline_text(
                                format!("li-{}-{ix}", item.row_id()),
                                &it.text,
                                &it.spans,
                                ix as u64,
                                rc,
                            )),
                    )
            })),
        TranscriptBlock::ToolGroup {
            tools, expanded, ..
        } if tools.len() == 1 && tools[0].is_receipt() => {
            render_receipt(item, &tools[0], *expanded, rc)
        }
        TranscriptBlock::ToolGroup {
            tools, overrides, ..
        } => crate::trace_view::render_tool_group(item, tools, overrides, rc)
            .relative()
            .children(rc.probe.as_ref().and_then(|p| p(RowProbe::Tool))),
        TranscriptBlock::Subagent {
            subagent_id,
            name,
            state,
            summary,
            ..
        } => crate::trace_view::render_subagent(item, subagent_id, name, state, summary, rc),
        TranscriptBlock::Reasoning {
            content, expanded, ..
        } => crate::trace_view::render_reasoning(item, content, *expanded, rc),
        TranscriptBlock::Plan {
            entries,
            live,
            expanded,
            ..
        } => crate::trace_view::render_plan(item, entries, *live, *expanded, rc),
        TranscriptBlock::TurnSummary {
            label,
            failed,
            expanded,
            live,
            ..
        } => crate::trace_view::render_turn_summary(item, label, *failed, *expanded, *live, rc),
        TranscriptBlock::Chart {
            chart, replaces, ..
        } => render_chart(item, chart, replaces, rc),
        TranscriptBlock::Choices { choices, .. } => render_choices(item, choices, rc),
        TranscriptBlock::Tests { run, .. } => render_tests(item, run, rc),
        TranscriptBlock::Stats { items, .. } => render_stats(item, items, rc),
        TranscriptBlock::FileChanges { card, expanded, .. } => {
            render_file_changes(item, card, *expanded, rc)
        }
        TranscriptBlock::MrCard {
            card,
            message_id,
            part_id,
            ..
        } => render_mr_card(item, card, message_id, part_id, rc),
        TranscriptBlock::Steps { steps, .. } => render_steps(item, steps, rc),
        TranscriptBlock::Callout { callout, .. } => render_callout(item, callout, rc),
        TranscriptBlock::Compare { columns, .. } => render_compare(item, columns, rc),
        TranscriptBlock::PlanCard { plan, .. } => crate::plan::render(
            item,
            plan,
            rc.plan_cards.get(item.row_id()),
            rc.plan_running,
            rc.reduced,
            rc.on_action.clone(),
            rc.tokens,
        ),
        TranscriptBlock::Files { items, .. } => div()
            .w_full()
            .py_1()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap_1p5()
            .children(
                items
                    .iter()
                    .enumerate()
                    .map(|(ix, file)| file_chip(item, ix, file, rc)),
            ),
        TranscriptBlock::Shots { paths, .. } => {
            div().w_full().py_1().flex().flex_col().gap_2().children(
                paths
                    .iter()
                    .enumerate()
                    .map(|(ix, path)| shot(item, ix, path, rc)),
            )
        }
        TranscriptBlock::Images { items, .. } => div()
            .flex()
            .flex_row()
            .flex_wrap()
            .justify_end()
            .gap_1p5()
            .children(
                items
                    .iter()
                    .enumerate()
                    .map(|(ix, image)| thumbnail(item, ix, image, rc)),
            ),
    };

    div()
        .id(ElementId::Name(format!("row-{}", item.row_id()).into()))
        .w_full()
        .px_4()
        .pb(zpx(2.))
        .flex()
        .flex_col()
        .when(role_is_user, |s| s.items_end().pt_3())
        .when(!role_is_user, |s| s.items_start())
        .child(
            div()
                .when(role_is_user, |s| {
                    s.max_w(relative(0.85))
                        .when_some(rc.bubble_max_w, |s, w| s.max_w(px(w)))
                        .min_w_0()
                        .overflow_hidden()
                        .px_3()
                        .py_2()
                        .rounded(RADIUS_LG)
                        .bg(c(t.surface))
                })
                .when(!role_is_user, |s| s.w_full())
                .when(role_is_error, |s| {
                    s.px_3()
                        .py_2()
                        .rounded(RADIUS_MD)
                        .border_l_2()
                        .border_color(c(t.danger))
                        .bg(c(t.surface))
                        .text_color(c(t.danger))
                })
                .child(content_elem),
        )
}

/// Bullets by depth (• ◦ ▪); numbers stay as written.
fn list_marker(marker: &str, depth: usize) -> String {
    if marker != "•" {
        return marker.to_string();
    }
    match depth {
        0 => "•",
        1 => "◦",
        _ => "▪",
    }
    .to_string()
}

/// Extracts host domain from a URL string (stripping protocol and www).
fn url_host(url: &str) -> String {
    let clean = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let host = clean.split('/').next().unwrap_or(clean);
    let host = host.split('?').next().unwrap_or(host);
    let host = host.split('#').next().unwrap_or(host);
    let host = host.strip_prefix("www.").unwrap_or(host);
    if host.is_empty() {
        url.to_string()
    } else {
        host.to_string()
    }
}

fn favicon_cache_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("keel-favicons");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn host_favicon_path(host: &str) -> std::path::PathBuf {
    let safe_host: String = host
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    favicon_cache_dir().join(format!("{safe_host}.png"))
}

static FAVICON_FETCHES: std::sync::OnceLock<std::sync::RwLock<std::collections::HashSet<String>>> =
    std::sync::OnceLock::new();

fn start_favicon_fetch(host: &str) {
    if host.is_empty()
        || host == "localhost"
        || host.starts_with("127.")
        || host.starts_with("0.0.")
        || host.ends_with(".local")
    {
        return;
    }

    let dest = host_favicon_path(host);
    if dest.exists() {
        return;
    }

    let fetches =
        FAVICON_FETCHES.get_or_init(|| std::sync::RwLock::new(std::collections::HashSet::new()));
    {
        let mut set = match fetches.write() {
            Ok(s) => s,
            Err(_) => return,
        };
        if set.contains(host) {
            return;
        }
        set.insert(host.to_string());
    }

    let host_owned = host.to_string();
    std::thread::spawn(move || {
        if let Some(img) = download_favicon_image(&host_owned) {
            let tmp = dest.with_extension(format!("tmp.{}", uuid::Uuid::new_v4()));
            if img.save_with_format(&tmp, image::ImageFormat::Png).is_ok() {
                let _ = std::fs::rename(&tmp, &dest);
            }
        }
    });
}

fn fetch_url_bytes(url: &str) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let resp = ureq::get(url)
        .timeout(std::time::Duration::from_secs(4))
        .set(
            "User-Agent",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
        )
        .set(
            "Accept",
            "image/avif,image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8",
        )
        .call()
        .ok()?;
    if resp.status() < 200 || resp.status() >= 300 {
        return None;
    }
    let mut reader = resp.into_reader().take(5 * 1024 * 1024);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).ok()?;
    if bytes.is_empty() {
        return None;
    }
    Some(bytes)
}

fn decode_image_bytes(bytes: &[u8]) -> Option<image::DynamicImage> {
    if let Ok(img) = image::load_from_memory(bytes) {
        let (w, h) = (img.width(), img.height());
        if w > 0 && h > 0 {
            let resized = if w > 64 || h > 64 {
                img.resize(64, 64, image::imageops::FilterType::Lanczos3)
            } else {
                img
            };
            return Some(resized);
        }
    }

    // If it's an SVG icon, rasterize with resvg
    if let Ok(svg_str) = std::str::from_utf8(bytes) {
        if svg_str.contains("<svg") {
            let options = resvg::usvg::Options::default();
            if let Ok(tree) = resvg::usvg::Tree::from_str(svg_str, &options) {
                let mut pixmap = resvg::tiny_skia::Pixmap::new(64, 64)?;
                let sx = 64.0 / tree.size().width();
                let sy = 64.0 / tree.size().height();
                let scale = sx.min(sy);
                let transform = resvg::tiny_skia::Transform::from_scale(scale, scale);
                resvg::render(&tree, transform, &mut pixmap.as_mut());
                let rgba = image::RgbaImage::from_raw(64, 64, pixmap.take())?;
                return Some(image::DynamicImage::ImageRgba8(rgba));
            }
        }
    }

    None
}

fn download_favicon_image(host: &str) -> Option<image::DynamicImage> {
    // 1. Direct favicon.ico from the site itself (follows redirects like gitlab.dot.co.id's custom dot-gitlab.png)
    let ico_url = format!("https://{host}/favicon.ico");
    if let Some(bytes) = fetch_url_bytes(&ico_url) {
        if let Some(img) = decode_image_bytes(&bytes) {
            return Some(img);
        }
    }

    // 2. Fallback to DuckDuckGo icon service
    let ddg_url = format!("https://icons.duckduckgo.com/ip3/{host}.ico");
    if let Some(bytes) = fetch_url_bytes(&ddg_url) {
        if let Some(img) = decode_image_bytes(&bytes) {
            return Some(img);
        }
    }

    // 3. Fallback to Google favicon service
    let g_url = format!(
        "https://t3.gstatic.com/faviconV2?client=SOCIAL&type=FAVICON&fallback_opts=TYPE,SIZE,URL&url=https://{host}&size=64"
    );
    if let Some(bytes) = fetch_url_bytes(&g_url) {
        if let Some(img) = decode_image_bytes(&bytes) {
            return Some(img);
        }
    }

    None
}

/// Renders a site's real favicon using the domain, with a crisp fallback icon while downloading or offline.
fn favicon_badge(host: &str, size_px: f32, t: Tokens) -> AnyElement {
    let fallback_icon = if host.contains("gitlab") {
        Icon::GitLab
    } else {
        Icon::Globe
    };

    if host == "localhost"
        || host.starts_with("127.")
        || host.starts_with("0.0.")
        || host.ends_with(".local")
        || host.is_empty()
    {
        return div()
            .size(zpx(size_px))
            .flex()
            .items_center()
            .justify_center()
            .text_color(c(t.info))
            .child(fallback_icon.el(zpx(size_px - 2.)))
            .into_any_element();
    }

    let path = host_favicon_path(host);
    if path.exists() {
        return gpui::img(path)
            .size(zpx(size_px))
            .rounded(px(2.))
            .flex_none()
            .into_any_element();
    }

    start_favicon_fetch(host);

    div()
        .size(zpx(size_px))
        .flex()
        .items_center()
        .justify_center()
        .text_color(c(t.info))
        .child(fallback_icon.el(zpx(size_px - 2.)))
        .into_any_element()
}

static PREVIEW_CACHE: std::sync::OnceLock<std::sync::RwLock<HashMap<String, String>>> =
    std::sync::OnceLock::new();

fn get_cached_preview(url: &str) -> Option<String> {
    PREVIEW_CACHE.get()?.read().ok()?.get(url).cloned()
}

fn cache_preview(url: String, title: String) {
    let cache = PREVIEW_CACHE.get_or_init(|| std::sync::RwLock::new(HashMap::new()));
    if let Ok(mut lock) = cache.write() {
        lock.insert(url, title);
    }
}

static PREVIEW_FETCHES: std::sync::OnceLock<std::sync::RwLock<std::collections::HashSet<String>>> =
    std::sync::OnceLock::new();

fn fetch_preview_title(url: String) {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return;
    }
    let fetches =
        PREVIEW_FETCHES.get_or_init(|| std::sync::RwLock::new(std::collections::HashSet::new()));
    {
        let mut set = match fetches.write() {
            Ok(s) => s,
            Err(_) => return,
        };
        if set.contains(&url) {
            return;
        }
        set.insert(url.clone());
    }

    std::thread::spawn(move || {
        use std::io::Read as _;
        if let Ok(resp) = ureq::get(&url)
            .timeout(std::time::Duration::from_secs(3))
            .set(
                "User-Agent",
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)",
            )
            .call()
        {
            let mut reader = resp.into_reader().take(64 * 1024);
            let mut body = String::new();
            if reader.read_to_string(&mut body).is_ok() {
                if let Some(title) = extract_html_title(&body) {
                    cache_preview(url, title);
                }
            }
        }
    });
}

fn extract_html_title(html: &str) -> Option<String> {
    if let Some(og_pos) = html
        .find("property=\"og:title\"")
        .or_else(|| html.find("property='og:title'"))
        .or_else(|| html.find("name=\"twitter:title\""))
    {
        let window = &html[og_pos.saturating_sub(100)..html.len().min(og_pos + 300)];
        if let Some(content_pos) = window.find("content=\"") {
            let rest = &window[content_pos + 9..];
            if let Some(end) = rest.find('"') {
                let title = rest[..end].trim();
                if !title.is_empty() {
                    return Some(clean_html_entities(title));
                }
            }
        }
    }

    let lower = html.to_lowercase();
    let start_tag = lower.find("<title>")?;
    let start = start_tag + 7;
    let end = lower[start..].find("</title>")? + start;
    let title = html[start..end].trim();
    if !title.is_empty() {
        Some(clean_html_entities(title))
    } else {
        None
    }
}

fn clean_html_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&nbsp;", " ")
        .trim()
        .to_string()
}

struct UrlPreviewInfo {
    host: String,
    category: Option<String>,
    title: Option<String>,
    subtitle: Option<String>,
}

fn parse_url_preview(url: &str, anchor_title: Option<&str>) -> UrlPreviewInfo {
    let host = url_host(url);
    let clean = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);

    let path = clean.split_once('/').map(|(_, p)| p).unwrap_or("");
    let path_no_query = path
        .split('?')
        .next()
        .unwrap_or(path)
        .split('#')
        .next()
        .unwrap_or(path);
    let parts: Vec<&str> = path_no_query.split('/').filter(|s| !s.is_empty()).collect();

    // GitLab or GitHub MR/PR/Issues
    if host.contains("gitlab") || host.contains("github") {
        if let Some(pos) = parts
            .iter()
            .position(|&p| p == "merge_requests" || p == "pull" || p == "issues")
        {
            let (kind, badge_name) = match parts[pos] {
                "merge_requests" => ("Merge Request", "MR"),
                "pull" => ("Pull Request", "PR"),
                "issues" => ("Issue", "Issue"),
                _ => ("Resource", "Resource"),
            };
            let number = parts.get(pos + 1).map(|id| format!("{badge_name} #{id}"));
            let repo = if pos >= 2 && parts[pos - 1] == "-" {
                parts[..pos - 1].join("/")
            } else if pos >= 2 {
                parts[..pos].join("/")
            } else {
                host.clone()
            };

            let title = match anchor_title {
                Some(t) if !t.is_empty() && t != url && !t.starts_with("http") => {
                    Some(t.to_string())
                }
                _ => number.clone(),
            };

            return UrlPreviewInfo {
                host,
                category: number.or_else(|| Some(kind.to_string())),
                title,
                subtitle: Some(repo),
            };
        }
    }

    // Meaningful slug from path if not a known git service
    let title = match anchor_title {
        Some(t) if !t.is_empty() && t != url && !t.starts_with("http") => Some(t.to_string()),
        _ => {
            if let Some(slug) = parts.last() {
                if slug.len() > 3 && slug.contains('-') {
                    let formatted: String = slug
                        .split('-')
                        .filter(|w| !w.is_empty() && !w.chars().all(|c| c.is_ascii_digit()))
                        .map(|w| {
                            let mut chars = w.chars();
                            match chars.next() {
                                None => String::new(),
                                Some(first) => {
                                    first.to_uppercase().collect::<String>() + chars.as_str()
                                }
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    if !formatted.is_empty() {
                        Some(formatted)
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        }
    };

    let subtitle = if !parts.is_empty() && parts.len() <= 3 {
        Some(parts.join(" › "))
    } else {
        None
    };

    UrlPreviewInfo {
        host,
        category: None,
        title,
        subtitle,
    }
}

/// Formats the chip label: keeps clean anchor text, or formats long URLs nicely.
fn url_chip_label(label: &str, url: &str) -> String {
    let _ = url;
    let trimmed = label.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        let clean = trimmed
            .strip_prefix("https://")
            .or_else(|| trimmed.strip_prefix("http://"))
            .unwrap_or(trimmed);
        let clean = clean.strip_prefix("www.").unwrap_or(clean);
        if clean.len() > 32 {
            let host = clean.split('/').next().unwrap_or(clean);
            if clean.len() > host.len() + 12 {
                format!("{host}/…")
            } else {
                clean.to_string()
            }
        } else {
            clean.to_string()
        }
    } else {
        trimmed.to_string()
    }
}

/// A floating popover tooltip with favicon, domain, and rich content preview.
struct UrlTip {
    url: String,
    title: Option<String>,
    tokens: Tokens,
}

impl UrlTip {
    fn new(url: &str, title: Option<String>, tokens: Tokens, cx: &mut Context<Self>) -> Self {
        fetch_preview_title(url.to_string());
        let host = url_host(url);
        start_favicon_fetch(&host);

        let url_key = url.to_string();
        let path = host_favicon_path(&host);
        let need_poll = get_cached_preview(url).is_none() || !path.exists();
        if need_poll {
            cx.spawn(async move |this, cx| {
                for _ in 0..10 {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(200))
                        .await;
                    let has_title = get_cached_preview(&url_key).is_some();
                    let has_icon = path.exists();
                    if has_title && has_icon {
                        let _ = this.update(cx, |_, cx| cx.notify());
                        return;
                    }
                }
                let _ = this.update(cx, |_, cx| cx.notify());
            })
            .detach();
        }

        Self {
            url: url.to_string(),
            title,
            tokens,
        }
    }
}

impl Render for UrlTip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let t = self.tokens;
        let preview = parse_url_preview(&self.url, self.title.as_deref());
        let cached = get_cached_preview(&self.url);

        let display_title = cached
            .or(preview.title)
            .or_else(|| self.title.clone())
            .filter(|title| !title.is_empty() && *title != preview.host && *title != self.url);

        div()
            .relative()
            .flex()
            .flex_col()
            .gap(zpx(6.))
            .px(zpx(12.))
            .py(zpx(9.))
            .max_w(zpx(360.))
            .min_w(zpx(220.))
            .rounded(RADIUS_MD)
            .border_1()
            .border_color(c(t.border))
            .shadow(vec![gpui::BoxShadow {
                color: c(t.shadow),
                offset: gpui::point(px(0.), zpx(5.)),
                blur_radius: zpx(16.),
                spread_radius: px(0.),
                inset: false,
            }])
            .bg(c(t.floating))
            // 1. Header: Favicon + Domain name + "Open ↗" action
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap(zpx(8.))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(zpx(7.))
                            .child(
                                div()
                                    .size(zpx(20.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(4.))
                                    .bg(c(t.hover))
                                    .border_1()
                                    .border_color(c(t.border))
                                    .child(favicon_badge(&preview.host, 14., t)),
                            )
                            .child(
                                div()
                                    .text_size(zpx(12.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(c(t.text))
                                    .child(preview.host.clone()),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(zpx(3.))
                            .text_color(c(t.text_faint))
                            .text_size(zpx(11.))
                            .child("Open")
                            .child(Icon::External.el(zpx(10.))),
                    ),
            )
            // 2. Content Preview Section
            .when(
                display_title.is_some() || preview.category.is_some() || preview.subtitle.is_some(),
                |d| {
                    let category = preview.category.clone();
                    let subtitle = preview.subtitle.clone();
                    let has_category = category.is_some();
                    d.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(zpx(3.))
                            .py(zpx(5.))
                            .border_y_1()
                            .border_color(c(t.border))
                            .when_some(category, |d, cat| {
                                d.child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap(zpx(5.))
                                        .child(
                                            div()
                                                .px(zpx(5.))
                                                .py(zpx(1.))
                                                .rounded(px(3.))
                                                .bg(c(t.info).opacity(0.15))
                                                .text_color(c(t.info))
                                                .text_size(zpx(10.))
                                                .font_weight(gpui::FontWeight::MEDIUM)
                                                .child(cat),
                                        )
                                        .when_some(subtitle.as_ref(), |d, sub| {
                                            d.child(
                                                div()
                                                    .text_size(zpx(11.))
                                                    .text_color(c(t.text_muted))
                                                    .child(sub.clone()),
                                            )
                                        }),
                                )
                            })
                            .when_some(display_title, |d, title| {
                                d.child(
                                    div()
                                        .text_size(zpx(12.5))
                                        .line_height(zpx(17.))
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(c(t.text))
                                        .child(title),
                                )
                            })
                            .when(!has_category && subtitle.is_some(), |d| {
                                d.child(
                                    div()
                                        .text_size(zpx(11.))
                                        .text_color(c(t.text_muted))
                                        .child(subtitle.unwrap()),
                                )
                            }),
                    )
                },
            )
            // 3. Full URL Footer
            .child(
                div()
                    .text_size(zpx(10.5))
                    .line_height(zpx(14.))
                    .text_color(c(t.text_faint))
                    .truncate()
                    .child(self.url.clone()),
            )
    }
}

/// A sleek clickable link chip with favicon icon, text, and rich URL tooltip.
fn link_chip(
    id: &str,
    text: &str,
    url: &str,
    t: Tokens,
    on_action: OnRowAction,
) -> gpui::Stateful<gpui::Div> {
    let url_click = url.to_string();
    let tip_url = url.to_string();
    let tip_text = text.to_string();
    let host = url_host(url);

    div()
        .id(ElementId::Name(format!("link-chip-{id}").into()))
        .h(zpx(22.))
        .px(zpx(8.))
        .my(zpx(1.))
        .flex()
        .flex_row()
        .items_center()
        .gap(zpx(5.))
        .rounded(RADIUS_SM)
        .border_1()
        .border_color(c(t.border))
        .bg(c(t.code))
        .text_color(c(t.text))
        .text_size(zpx(12.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .cursor_pointer()
        .hover(|s| {
            s.border_color(c(t.info).opacity(0.55))
                .text_color(c(t.text))
        })
        .hover_fade(c(t.hover), Some(c(t.selected)))
        .child(favicon_badge(&host, 13., t))
        .child(
            div()
                .flex_none()
                .whitespace_nowrap()
                .child(text.to_string()),
        )
        .child(
            div()
                .flex_none()
                .text_color(c(t.text_faint).opacity(0.7))
                .child(Icon::External.el(zpx(9.))),
        )
        .tooltip(move |_, cx| {
            cx.new(|cx| {
                UrlTip::new(
                    &tip_url,
                    (!tip_text.is_empty()).then(|| tip_text.clone()),
                    t,
                    cx,
                )
            })
            .into()
        })
        .on_click(move |_, _, cx| {
            on_action(
                RowAction::OpenUrl {
                    url: url_click.clone(),
                },
                cx,
            );
        })
}

fn split_trailing_punct(word: &str) -> (&str, &str) {
    let trimmed = word.trim_end_matches(['.', ':', ';', '!', '?', ',']);
    (trimmed, &word[trimmed.len()..])
}

fn looks_like_file_path(s: &str, is_code: bool) -> Option<String> {
    const EXTENSIONS: &[&str] = &[
        "go",
        "rs",
        "ts",
        "tsx",
        "js",
        "jsx",
        "py",
        "md",
        "json",
        "toml",
        "yaml",
        "yml",
        "html",
        "htm",
        "css",
        "scss",
        "sh",
        "bash",
        "zsh",
        "c",
        "h",
        "cpp",
        "hpp",
        "cc",
        "sql",
        "proto",
        "txt",
        "lock",
        "svg",
        "png",
        "jpg",
        "jpeg",
        "gif",
        "webp",
        "xml",
        "csv",
        "tsv",
        "env",
        "log",
        "java",
        "kt",
        "swift",
        "rb",
        "php",
        "vue",
        "svelte",
        "lua",
        "graphql",
        "gql",
        "tf",
        "dockerfile",
        "makefile",
    ];
    const SPECIAL_FILES: &[&str] = &[
        "makefile",
        "dockerfile",
        "gemfile",
        "procfile",
        "license",
        "licence",
    ];
    const SYSTEM_ROOTS: &[&str] = &[
        "/Users/",
        "/home/",
        "/private/",
        "/tmp/",
        "/var/",
        "/opt/",
        "/usr/",
        "/Applications/",
        "/Volumes/",
    ];

    let s = s.trim();
    if s.is_empty() || s.contains("://") || s.starts_with("http") {
        return None;
    }
    // Web endpoints, query params, route parameters, or fragments are not files
    if s.contains('?') || s.contains('#') || s.contains('{') || s.contains('}') {
        return None;
    }

    let s = s.trim_matches(|c: char| {
        matches!(
            c,
            '`' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ',' | ';'
        )
    });
    let trimmed = s.trim_end_matches(['.', ':', ';', '!', '?', ',']);
    if trimmed.is_empty() {
        return None;
    }

    // Paths starting with `/` are web endpoints (e.g. `/llms.txt`, `/robots.txt`, `/api/users`),
    // unless they are absolute filesystem paths starting with a known system root.
    if trimmed.starts_with('/') && !SYSTEM_ROOTS.iter().any(|root| trimmed.starts_with(root)) {
        return None;
    }

    let parts: Vec<&str> = trimmed.split('/').filter(|p| !p.is_empty()).collect();
    let filename = parts.last()?;

    if SPECIAL_FILES
        .iter()
        .any(|f| filename.eq_ignore_ascii_case(f))
    {
        return Some(trimmed.to_string());
    }
    if filename.starts_with('.') && filename.len() > 1 && !filename.starts_with("..") {
        return Some(trimmed.to_string());
    }

    if let Some((stem, ext)) = filename.rsplit_once('.') {
        let ext_lower = ext.to_ascii_lowercase();
        if stem.is_empty()
            || ext.chars().all(|c| c.is_ascii_digit())
            || stem.chars().all(|c| c.is_ascii_digit())
        {
            return None;
        }
        if parts.len() > 1 {
            if (1..=8).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric()) {
                return Some(trimmed.to_string());
            }
        } else if is_code || EXTENSIONS.iter().any(|e| ext_lower == *e) {
            if EXTENSIONS.iter().any(|e| ext_lower == *e) {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

struct FileTip {
    path: String,
    tokens: Tokens,
}

impl Render for FileTip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let t = self.tokens;
        let kind = crate::files::FileKind::of(std::path::Path::new(&self.path));
        let icon = kind.icon();
        div()
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(6.))
            .px(zpx(10.))
            .py(zpx(6.))
            .rounded(RADIUS_MD)
            .border_1()
            .border_color(c(t.border))
            .bg(c(t.surface))
            .floating_surface(t)
            .child(
                div()
                    .flex_none()
                    .text_color(c(t.accent))
                    .child(icon.el(zpx(13.))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .t_small()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(c(t.text))
                            .child(self.path.clone()),
                    )
                    .child(
                        div()
                            .t_caption()
                            .text_color(c(t.text_muted))
                            .child("Click to view in right panel"),
                    ),
            )
    }
}

/// An interactive file chip rendered inline in response prose.
/// `max_w` caps the chip so a long path ellipsizes inside a bubble or prose column.
fn file_inline_chip(
    id: &str,
    path: &str,
    t: Tokens,
    max_w: gpui::Pixels,
    on_action: OnRowAction,
) -> gpui::Stateful<gpui::Div> {
    let file_kind = crate::files::FileKind::of(std::path::Path::new(path));
    let icon = file_kind.icon();
    let display_path = path.to_string();
    let click_path = path.to_string();
    let tip_path = path.to_string();

    div()
        .id(ElementId::Name(format!("fchip-{id}").into()))
        .flex_none()
        .max_w(max_w)
        .min_w_0()
        .h(zpx(20.))
        .px(zpx(6.))
        .flex()
        .flex_row()
        .items_center()
        .gap(zpx(4.))
        .rounded(RADIUS_SM)
        .bg(c(t.surface))
        .border_1()
        .border_color(c(t.border))
        .overflow_hidden()
        .cursor_pointer()
        .hover(|s| {
            s.border_color(c(t.accent).opacity(0.6))
                .text_color(c(t.text))
        })
        .hover_fade(c(t.hover), Some(c(t.selected)))
        .child(
            div()
                .flex_none()
                .text_color(c(t.accent).opacity(0.85))
                .child(icon.el(zpx(11.))),
        )
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis_middle()
                .font_family(crate::style::MONO)
                .text_size(zpx(11.5))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(c(t.text))
                .child(display_path),
        )
        .tooltip(move |_, cx| {
            let path = tip_path.clone();
            cx.new(move |_| FileTip { path, tokens: t }).into()
        })
        .on_click(move |_, _, cx| {
            on_action(
                RowAction::OpenFile {
                    path: click_path.clone(),
                },
                cx,
            );
        })
}

/// Whether a paragraph needs the chip layout (links or file paths).
/// Path-like tokens inside a multi-word code span do not count: inline code
/// stays one text run unless the whole span is a file path.
fn needs_inline_chips(text: &str, spans: &[InlineSpan]) -> bool {
    if spans.iter().any(|s| s.link.is_some()) {
        return true;
    }
    if spans
        .iter()
        .any(|s| s.style.code && looks_like_file_path(&text[s.range.clone()], true).is_some())
    {
        return true;
    }
    let mut cursor = 0;
    for span in spans {
        if span.range.start > cursor {
            let plain = &text[cursor..span.range.start];
            if plain
                .split_whitespace()
                .any(|w| looks_like_file_path(w, false).is_some())
            {
                return true;
            }
        }
        cursor = span.range.end;
    }
    if cursor < text.len() {
        let plain = &text[cursor..];
        if plain
            .split_whitespace()
            .any(|w| looks_like_file_path(w, false).is_some())
        {
            return true;
        }
    }
    false
}

/// Pixel cap for an inline file chip. `bubble_max_w` is already window
/// pixels (`px`); `prose_max_w` is design pixels (`zpx`). Hard-capped like
/// the block file chip so a shrink-to-fit bubble cannot grow past the path.
fn inline_chip_max_w(rc: &RowContext) -> gpui::Pixels {
    const CHIP_CHROME: f32 = 32.;
    const HARD_CAP: f32 = 340.;
    let from_prose = ((rc.prose_max_w - CHIP_CHROME).max(120.)) * crate::style::zoom();
    let from_bubble = rc
        .bubble_max_w
        .map(|w| (w - CHIP_CHROME).max(120.))
        .unwrap_or(from_prose);
    px(from_bubble.min(from_prose).min(HARD_CAP * crate::style::zoom()))
}

/// Text with its inline styles: bold, italic, strikethrough, inline code,
/// links, and clickable file chips. Inline code stays in the text run, a
/// step heavier than the sentence. A code-block fill here is a line-tall
/// slab, so a short span reads as a highlighter.
fn inline_text(
    id: String,
    text: &str,
    spans: &[InlineSpan],
    order: u64,
    rc: &RowContext,
) -> AnyElement {
    let t = rc.tokens;
    let chip_max = inline_chip_max_w(rc);

    if !needs_inline_chips(text, spans) {
        let key = format!("sel-{id}");
        if spans.is_empty() {
            return selectable(
                key,
                text,
                order,
                rc,
                |s| s.into_any_element(),
                gpui::StyledText::new(text.to_string()),
            );
        }
        let highlights = spans.iter().map(|span| {
            let style = gpui::HighlightStyle {
                font_weight: if span.style.bold {
                    Some(gpui::FontWeight::SEMIBOLD)
                } else if span.style.code {
                    Some(gpui::FontWeight::MEDIUM)
                } else {
                    None
                },
                font_style: span.style.italic.then_some(gpui::FontStyle::Italic),
                strikethrough: span.style.strike.then(|| gpui::StrikethroughStyle {
                    thickness: px(1.),
                    color: None,
                }),
                // Bold and code lead by brightness as much as weight (body text is dimmer).
                color: if span.style.code || span.style.bold {
                    Some(c(t.text))
                } else {
                    None
                },
                ..Default::default()
            };
            (span.range.clone(), style)
        });
        let styled = gpui::StyledText::new(text.to_string()).with_highlights(highlights);
        return selectable(key, text, order, rc, |s| s.into_any_element(), styled);
    }

    // Paragraphs with links or file chips render as a cohesive inline flow of words and sleek chips
    let mut slices: Vec<(Range<usize>, crate::markdown::InlineStyle, Option<String>)> = Vec::new();
    let mut cursor = 0;
    for span in spans {
        if span.range.start > cursor {
            slices.push((
                cursor..span.range.start,
                crate::markdown::InlineStyle::default(),
                None,
            ));
        }
        slices.push((span.range.clone(), span.style, span.link.clone()));
        cursor = span.range.end;
    }
    if cursor < text.len() {
        slices.push((
            cursor..text.len(),
            crate::markdown::InlineStyle::default(),
            None,
        ));
    }

    let mut elements: Vec<AnyElement> = Vec::new();
    let mut item_ix = 0;
    for (range, style, link) in slices {
        if let Some(url) = link {
            let label = &text[range];
            if url.starts_with("file://") || looks_like_file_path(&url, true).is_some() {
                let file_path = url.strip_prefix("file://").unwrap_or(&url);
                elements.push(
                    file_inline_chip(
                        &format!("{id}-{item_ix}"),
                        file_path,
                        t,
                        chip_max,
                        rc.on_action.clone(),
                    )
                    .into_any_element(),
                );
                item_ix += 1;
                continue;
            }
            let host = url_host(&url);
            start_favicon_fetch(&host);
            fetch_preview_title(url.clone());
            let display_label = url_chip_label(label, &url);
            elements.push(
                link_chip(
                    &format!("{id}-{item_ix}"),
                    &display_label,
                    &url,
                    t,
                    rc.on_action.clone(),
                )
                .into_any_element(),
            );
            item_ix += 1;
        } else {
            let slice_str = &text[range];
            if style.code {
                if let Some(file_path) = looks_like_file_path(slice_str.trim(), true) {
                    elements.push(
                        file_inline_chip(
                            &format!("{id}-{item_ix}"),
                            &file_path,
                            t,
                            chip_max,
                            rc.on_action.clone(),
                        )
                        .into_any_element(),
                    );
                    item_ix += 1;
                    continue;
                }
                // Multi-word (or non-path) inline code stays one mono run.
                elements.push(
                    div()
                        .flex_none()
                        .max_w_full()
                        .min_w_0()
                        .px(zpx(4.))
                        .py(zpx(0.5))
                        .rounded(RADIUS_SM)
                        .bg(c(t.code))
                        .text_color(c(t.text))
                        .font_family(crate::style::MONO)
                        .text_size(zpx(12.))
                        .child(slice_str.to_string())
                        .into_any_element(),
                );
                item_ix += 1;
                continue;
            }

            for word in slice_str.split_whitespace() {
                let is_bullet = word == "·" || word == "•" || word == "◦" || word == "▪";
                let is_punct = word == ","
                    || word == "."
                    || word == ":"
                    || word == ";"
                    || word == "!"
                    || word == "?";
                let (trimmed_word, trailing_punct) = split_trailing_punct(word);

                if let Some(file_path) = looks_like_file_path(trimmed_word, false) {
                    elements.push(
                        file_inline_chip(
                            &format!("{id}-{item_ix}"),
                            &file_path,
                            t,
                            chip_max,
                            rc.on_action.clone(),
                        )
                        .into_any_element(),
                    );
                    item_ix += 1;
                    if !trailing_punct.is_empty() {
                        elements.push(
                            div()
                                .flex_none()
                                .ml(zpx(-3.))
                                .text_color(c(t.text).opacity(0.88))
                                .child(trailing_punct.to_string())
                                .into_any_element(),
                        );
                        item_ix += 1;
                    }
                } else if is_bullet {
                    elements.push(
                        div()
                            .flex_none()
                            .px(zpx(1.))
                            .text_color(c(t.text_faint))
                            .child(word.to_string())
                            .into_any_element(),
                    );
                    item_ix += 1;
                } else {
                    let el = div()
                        .flex_none()
                        .when(is_punct, |d| d.ml(zpx(-3.)))
                        .when(style.bold, |d| {
                            d.font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(c(t.text))
                        })
                        .when(!style.bold, |d| d.text_color(c(t.text).opacity(0.88)))
                        .when(style.italic, |d| d.italic())
                        .when(style.strike, |d| d.line_through())
                        .child(word.to_string());
                    elements.push(el.into_any_element());
                    item_ix += 1;
                }
            }
        }
    }

    let flow = div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .gap_x(zpx(5.))
        .gap_y(zpx(4.))
        .children(elements);

    flow.into_any_element()
}

/// Text that joins the window's drag selection (Cmd-C copies it). `wrap`
/// builds the painted element around `styled`, e.g. to keep links clickable.
/// `order` ranks it within its row.
// ponytail: rows scrolled out of the list drop out of a selection; register
// content keys (`resolve_content_key_with`) if long cross-screen drags matter.
fn selectable(
    key: String,
    text: &str,
    order: u64,
    rc: &RowContext,
    wrap: impl FnOnce(gpui::StyledText) -> AnyElement,
    styled: gpui::StyledText,
) -> AnyElement {
    Selectable {
        key: key.into(),
        text: text.to_string().into(),
        layout: styled.layout().clone(),
        child: wrap(styled),
        order: rc.order + order,
        color: c(rc.tokens.selection),
    }
    .into_any_element()
}

struct Selectable {
    key: SharedString,
    text: SharedString,
    layout: gpui::TextLayout,
    child: AnyElement,
    order: u64,
    color: Hsla,
}

impl IntoElement for Selectable {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl gpui::Element for Selectable {
    type RequestLayoutState = ();
    type PrepaintState = (gpui_base::TextSelectionHandle, gpui::Hitbox);

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) -> (gpui::LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: gpui::Bounds<Pixels>,
        _: &mut (),
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) -> (gpui_base::TextSelectionHandle, gpui::Hitbox) {
        self.child.prepaint(window, cx);
        let handle = window
            .use_keyed_state(ElementId::Name(self.key.clone()), cx, |_, cx| {
                gpui_base::TextSelectionHandle::new("", cx)
            })
            .read(cx)
            .clone();
        let hitbox = window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal);
        handle.register(
            gpui_base::TextSelectionRegistration::new(hitbox.clone(), bounds)
                .with_document_order(self.order)
                .with_text_bounds(vec![bounds]),
            window,
            cx,
        );
        (handle, hitbox)
    }

    fn paint(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: gpui::Bounds<Pixels>,
        _: &mut (),
        (handle, hitbox): &mut (gpui_base::TextSelectionHandle, gpui::Hitbox),
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) {
        window.set_cursor_style(gpui::CursorStyle::IBeam, hitbox);
        // Text first, the (translucent) selection over it: under it, inline
        // code's own background covered the selection, leaving holes.
        self.child.paint(window, cx);
        let run = gpui_base::TextSelectionRun::new(self.text.clone(), self.layout.clone(), bounds);
        let range = handle
            .update_runs(&[run], cx)
            .ranges()
            .first()
            .cloned()
            .flatten();
        if let Some(range) = range
            && let (Some(start), Some(end)) = (
                self.layout.position_for_index(range.start),
                self.layout.position_for_index(range.end),
            )
        {
            let lh = self.layout.line_height();
            let quad = |a: gpui::Point<Pixels>, b: gpui::Point<Pixels>| {
                gpui::fill(gpui::Bounds::from_corners(a, b), self.color)
            };
            if start.y == end.y {
                window.paint_quad(quad(start, gpui::point(end.x, end.y + lh)));
            } else {
                window.paint_quad(quad(start, gpui::point(bounds.right(), start.y + lh)));
                if end.y > start.y + lh {
                    window.paint_quad(quad(
                        gpui::point(bounds.left(), start.y + lh),
                        gpui::point(bounds.right(), end.y),
                    ));
                }
                window.paint_quad(quad(
                    gpui::point(bounds.left(), end.y),
                    gpui::point(end.x, end.y + lh),
                ));
            }
        }
    }
}

/// A GFM table: hairline grid on a rounded frame, bold header row (hidden
/// when every header cell is empty), faint zebra rows, column alignment.
/// Columns share the width by content length and cells wrap; a table with
/// many columns keeps a minimum column width and scrolls sideways.
fn render_table(
    item: &TranscriptRowItem,
    align: &[ColumnAlign],
    header: &[Inline],
    rows: &[Vec<Inline>],
    rc: &RowContext,
) -> gpui::Div {
    let t = rc.tokens;
    let cols = header
        .len()
        .max(rows.iter().map(Vec::len).max().unwrap_or(0))
        .max(1);
    let weights: Vec<f32> = (0..cols)
        .map(|col| {
            let longest = header
                .get(col)
                .into_iter()
                .chain(rows.iter().filter_map(|r| r.get(col)))
                .map(|cell| cell.text.chars().count())
                .max()
                .unwrap_or(0);
            longest.clamp(4, 48) as f32
        })
        .collect();
    const MIN_COL: f32 = 72.;
    let show_header = header.iter().any(|cell| !cell.text.is_empty());
    let empty = Inline::default();
    let row_el = |key: &str, order: u64, cells: &[Inline], head: bool, zebra: bool, last: bool| {
        div()
            .w_full()
            .flex()
            .flex_row()
            .when(!last, |d| d.border_b_1().border_color(c(t.border)))
            .when(head, |d| {
                d.bg(c(t.surface))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(c(t.text))
            })
            .when(zebra, |d| d.bg(c(t.hover)))
            .children((0..cols).map(|col| {
                let cell = cells.get(col).unwrap_or(&empty);
                // Columns share the row's width by their longest cell (no
                // fixed fractions plus padding that add up past the edge);
                // long text wraps inside its column.
                let mut column = div().flex_basis(zpx(0.)).min_w(zpx(MIN_COL));
                column.style().flex_grow = Some(weights[col]);
                column
                    .when(col + 1 < cols, |d| d.border_r_1().border_color(c(t.border)))
                    .child(
                        div()
                            .px_2p5()
                            .py(zpx(5.))
                            .when(align.get(col) == Some(&ColumnAlign::Center), |d| {
                                d.text_center()
                            })
                            .when(align.get(col) == Some(&ColumnAlign::Right), |d| {
                                d.text_right()
                            })
                            .child(inline_text(
                                format!("td-{}-{key}-{col}", item.row_id()),
                                &cell.text,
                                &cell.spans,
                                order + col as u64,
                                rc,
                            )),
                    )
            }))
    };
    let mut grid = div()
        .w_full()
        .min_w(zpx(MIN_COL * cols as f32))
        .flex()
        .flex_col()
        .text_size(zpx(13.))
        .line_height(zpx(19.))
        .text_color(c(t.text));
    if show_header {
        grid = grid.child(row_el("h", 0, header, true, false, rows.is_empty()));
    }
    grid = grid.children(rows.iter().enumerate().map(|(ix, r)| {
        row_el(
            &ix.to_string(),
            ((ix + 1) * cols) as u64,
            r,
            false,
            ix % 2 == 1,
            ix + 1 == rows.len(),
        )
    }));
    let mut scroller = div()
        .id(ElementId::Name(format!("table-{}", item.row_id()).into()))
        .w_full()
        .overflow_x_scroll()
        .child(grid);
    // A vertical wheel over the table scrolls the transcript, not the table.
    scroller.style().restrict_scroll_to_axis = Some(true);
    div()
        .w_full()
        .my_1()
        .rounded(RADIUS_MD)
        .border_1()
        .border_color(c(t.border))
        .overflow_hidden()
        .child(scroller)
}

/// Transcript thumbnail size (Zeron's 112×80).
const THUMB_W: f32 = 112.;
const THUMB_H: f32 = 80.;

/// One image in a user bubble: the file when it is on this device, a named
/// placeholder otherwise. Click opens the lightbox.
fn thumbnail(
    item: &TranscriptRowItem,
    ix: usize,
    image: &ImageRef,
    rc: &RowContext,
) -> gpui::Stateful<gpui::Div> {
    let t = rc.tokens;
    let frame = div()
        .id(ElementId::Name(
            format!("thumb-{}-{ix}", item.row_id()).into(),
        ))
        .w(zpx(THUMB_W))
        .h(zpx(THUMB_H))
        .flex_none()
        .rounded(RADIUS_MD)
        .overflow_hidden()
        .border_1()
        .border_color(c(t.border))
        .bg(c(t.surface));
    let placeholder = {
        let name = image.name.clone();
        move || {
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_1()
                .text_color(c(t.text_faint))
                .child(Icon::Image.el(zpx(18.)))
                .child(
                    div()
                        .t_caption()
                        .max_w(zpx(THUMB_W - 12.))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(name.clone()),
                )
                .into_any_element()
        }
    };
    match image.path.clone() {
        Some(path) => {
            let on_action = rc.on_action.clone();
            let open = path.clone();
            frame
                .cursor_pointer()
                .hover(|s| s.border_color(c(t.focus)))
                .active(|s| s.border_color(c(t.focus)).opacity(0.85))
                .on_click(move |_, _, cx| {
                    on_action(RowAction::OpenImage { path: open.clone() }, cx)
                })
                .child(
                    gpui::img(crate::thumbs::source(
                        path,
                        THUMB_W * crate::style::zoom(),
                        THUMB_H * crate::style::zoom(),
                        true,
                    ))
                    .size_full()
                    .object_fit(gpui::ObjectFit::Cover)
                    .with_fallback(placeholder),
                )
        }
        None => frame.child(placeholder()),
    }
}

/// A tool screenshot or markdown image: at most 360 wide, 16:10 frame.
const SHOT_W: f32 = 360.;
const SHOT_H: f32 = 225.;

/// One large image under a tool row or in assistant text. Click previews
/// it like a file chip (browser pane, else the system viewer).
fn shot(
    item: &TranscriptRowItem,
    ix: usize,
    path: &str,
    rc: &RowContext,
) -> gpui::Stateful<gpui::Div> {
    let t = rc.tokens;
    let on_action = rc.on_action.clone();
    let open = path.to_string();
    let name = std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    div()
        .id(ElementId::Name(
            format!("shot-{}-{ix}", item.row_id()).into(),
        ))
        .w(zpx(SHOT_W))
        .max_w_full()
        .h(zpx(SHOT_H))
        .flex_none()
        .rounded(RADIUS_MD)
        .overflow_hidden()
        .border_1()
        .border_color(c(t.border))
        .bg(c(t.surface))
        .cursor_pointer()
        .hover(|s| s.border_color(c(t.focus)))
        .active(|s| s.border_color(c(t.focus)).opacity(0.90))
        .on_click(move |_, _, cx| on_action(RowAction::OpenFile { path: open.clone() }, cx))
        .child(
            // Downscaled once, off the main thread, not the full bitmap.
            gpui::img(crate::thumbs::source(
                path,
                SHOT_W * crate::style::zoom(),
                SHOT_H * crate::style::zoom(),
                false,
            ))
            .size_full()
            .object_fit(gpui::ObjectFit::Contain)
            .with_fallback(move || {
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .text_color(c(t.text_faint))
                    .child(Icon::Image.el(zpx(18.)))
                    .child(div().t_caption().child(name.clone()))
                    .into_any_element()
            }),
        )
}

const FILE_CHIP_H: f32 = 32.;

/// A file the turn produced: a small preview (images) or type icon, the
/// name, and its extension. Click previews it in the browser pane when the
/// pane can show it, else opens it with the system app.
fn file_chip(
    item: &TranscriptRowItem,
    ix: usize,
    file: &FileRef,
    rc: &RowContext,
) -> gpui::Stateful<gpui::Div> {
    let t = rc.tokens;
    let kind = file.kind();
    let ext = std::path::Path::new(&file.name)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_uppercase())
        .unwrap_or_else(|| "FILE".into());
    let badge = div()
        .size(zpx(20.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(crate::style::RADIUS_SM)
        .overflow_hidden()
        .bg(c(t.accent & 0xFFFF_FF00 | 0x24))
        .text_color(c(t.accent));
    let badge = match kind {
        FileKind::Image | FileKind::Svg => {
            let icon = kind.icon();
            badge.child(
                gpui::img(std::path::PathBuf::from(&file.path))
                    .size_full()
                    .object_fit(gpui::ObjectFit::Cover)
                    .with_fallback(move || icon.el(zpx(12.)).into_any_element()),
            )
        }
        _ => badge.child(kind.icon().el(zpx(12.))),
    };
    let on_action = rc.on_action.clone();
    let path = file.path.clone();
    div()
        .id(ElementId::Name(
            format!("file-{}-{ix}", item.row_id()).into(),
        ))
        .h(zpx(FILE_CHIP_H))
        .max_w(zpx(340.))
        .pl(zpx(6.))
        .pr_2p5()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .rounded(RADIUS_MD)
        .border_1()
        .border_color(c(t.border))
        .bg(c(t.surface))
        .cursor_pointer()
        .hover(|s| s.border_color(c(t.focus)))
        .hover_fade(c(t.hover), Some(c(t.selected)))
        .active(|s| s.border_color(c(t.focus)).opacity(0.85))
        .on_click(move |_, _, cx| on_action(RowAction::OpenFile { path: path.clone() }, cx))
        .child(badge)
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .t_small()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(c(t.text))
                .child(file.name.clone()),
        )
        .child(
            div()
                .flex_none()
                .t_caption()
                .text_color(c(t.text_faint))
                .child(ext),
        )
        .child(
            div().flex_none().text_color(c(t.text_faint)).child(
                (if kind.previewable() {
                    Icon::Globe
                } else {
                    Icon::External
                })
                .el(zpx(11.)),
            ),
        )
}

/// One of Keel's browser tools as the chat shows it: "Taking screenshot"
/// while it runs, "Took screenshot" after, and its icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrowserStep {
    pub doing: &'static str,
    pub done: &'static str,
    pub icon: Icon,
}

/// `mcp__keel_browser__browser_screenshot` (or a bare `browser_screenshot`)
/// as a [`BrowserStep`]; `None` for any other tool.
pub fn browser_step(name: &str) -> Option<BrowserStep> {
    let base = crate::browser_trace::tool_base(name)?;
    let icon = crate::browser_trace::icon(crate::browser_trace::op_of(&base));
    let step = |doing, done, _icon: Icon| Some(BrowserStep { doing, done, icon });
    match base.trim_start_matches("browser_") {
        "open_pane" => step("Opening browser", "Opened browser", Icon::Monitor),
        "navigate" => step("Opening page", "Opened page", Icon::Globe),
        "screenshot" => step("Taking screenshot", "Took screenshot", Icon::Camera),
        "snapshot" => step("Reading page", "Read page", Icon::Eye),
        "click" => step("Clicking", "Clicked", Icon::PointerClick),
        "type" => step("Typing", "Typed", Icon::Keyboard),
        "scroll" => step("Scrolling", "Scrolled", Icon::Mouse),
        "request" => step("Asking you", "Asked you", Icon::Comment),
        "complete" => step("Finishing up", "Finished in browser", Icon::Check),
        _ => step("Using browser", "Used browser", Icon::Globe),
    }
}

/// A tool's name as the chat shows it: browser steps by what they did.
fn tool_label(tool: &ToolEntry) -> &str {
    match browser_step(&tool.name) {
        Some(step) if tool.state == "running" => step.doing,
        Some(step) => step.done,
        None => &tool.name,
    }
}

/// "Running bash" / "Ran bash" / "Ran 3 tools", and the distinct tool names in order.
pub fn tool_group_labels(tools: &[ToolEntry]) -> (String, Vec<&str>) {
    let title = match tools.iter().rev().find(|tool| tool.state == "running") {
        Some(tool) if browser_step(&tool.name).is_some() => tool_label(tool).to_string(),
        Some(tool) => format!("Running {}", tool.name),
        None if tools.len() == 1 => browser_step(&tools[0].name)
            .map_or_else(|| format!("Ran {}", tools[0].name), |s| s.done.to_string()),
        None => format!("Ran {} tools", tools.len()),
    };
    let mut names: Vec<&str> = Vec::new();
    for tool in tools {
        let label = browser_step(&tool.name).map_or(tool.name.as_str(), |s| s.done);
        if !names.contains(&label) {
            names.push(label);
        }
    }
    (title, names)
}

/// Collapsed: one line, "Ran 3 tools" with names, or the tool now running.
/// Expanded: each call with its status and first line of output.
/// Horizontal padding inside a folded row's (tools, thinking) clickable
/// header. The row is shifted left by the same amount so its content
/// shares the left edge of assistant text and the inline loader.
const FOLD_INSET: f32 = 8.;

/// Expanded receipt: its arguments block (and answer), read-only.
fn receipt_body_height(tool: &ToolEntry) -> f32 {
    let Some(receipt) = keel_proto::Receipt::from_summary(&tool.summary) else {
        return 0.0;
    };
    let args = receipt.tool.map(|t| t.args).unwrap_or_default();
    let lines =
        crate::approval::arg_lines(&args, 72) + receipt.answer.map_or(0, |a| a.lines().count());
    if lines == 0 {
        0.0
    } else {
        lines as f32 * 22.0 + 28.0
    }
}

/// A settled question: `✓ Allowed · Navigate browser · example.com`, styled
/// like a tool row. Clicking shows what was asked, read-only.
fn render_receipt(
    item: &TranscriptRowItem,
    tool: &ToolEntry,
    expanded: bool,
    rc: &RowContext,
) -> gpui::Div {
    let t = rc.tokens;
    let receipt = keel_proto::Receipt::from_summary(&tool.summary);
    let line = receipt
        .as_ref()
        .map(crate::approval::receipt_line)
        .unwrap_or(crate::approval::ReceiptLine {
            mark: crate::approval::Mark::None,
            verb: "Question settled".to_string(),
            title: None,
            arg: None,
        });
    let args = receipt
        .as_ref()
        .and_then(|r| r.tool.as_ref())
        .map(|t| t.args.clone())
        .unwrap_or_default();
    let answer = receipt
        .as_ref()
        .and_then(|r| r.answer.clone())
        .filter(|a| !a.trim().is_empty());
    let has_body = !args.is_empty() || answer.is_some();
    let on_action = rc.on_action.clone();
    let row_id = item.row_id().to_string();
    let header = div()
        .id(ElementId::Name(format!("receipt-{}", item.row_id()).into()))
        .h(zpx(28.))
        .px(zpx(FOLD_INSET))
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .rounded(RADIUS_MD)
        .when(has_body, |d| d.hover_fade(c(t.hover), None))
        .child(crate::view::receipt_line_el(&line, t))
        .when(has_body, |d| {
            d.cursor_pointer()
                .on_click(move |_, _, cx| {
                    on_action(
                        RowAction::Toggle {
                            row_id: row_id.clone(),
                        },
                        cx,
                    )
                })
                .child(chevron(expanded, item.row_id(), rc))
        });
    let body = div()
        .ml(zpx(FOLD_INSET))
        .py_1()
        .flex()
        .flex_col()
        .gap_1()
        .when(!args.is_empty(), |d| {
            d.child(crate::view::receipt_args_el(&args, t))
        })
        .children(answer.map(|a| {
            div()
                .t_small()
                .text_color(c(t.text_muted))
                .whitespace_normal()
                .child(a)
        }));
    let height = tool_receipt_height(tool);
    div()
        .w_full()
        .ml(zpx(-FOLD_INSET))
        .flex()
        .flex_col()
        .child(header)
        .children(fold_body(body, item.row_id(), expanded, height, rc))
}

fn tool_receipt_height(tool: &ToolEntry) -> f32 {
    receipt_body_height(tool)
}

/// How long after a toggle a folded row is still animating.
const FOLD_WINDOW: std::time::Duration = std::time::Duration::from_millis(260);

/// The toggle number when `row_id` was opened or closed just now.
fn recent_toggle(row_id: &str, rc: &RowContext) -> Option<u64> {
    rc.toggles
        .get(row_id)
        .filter(|(_, at)| !rc.reduced && at.elapsed() < FOLD_WINDOW)
        .map(|(n, _)| *n)
}

/// Right when folded, down when open; turns between the two after a toggle.
fn chevron(expanded: bool, row_id: &str, rc: &RowContext) -> gpui::Div {
    use gpui::AnimationExt as _;
    let turned = |icon: crate::icons::IconSvg, open: f32| {
        icon.with_transformation(gpui::Transformation::rotate(gpui::radians(
            -std::f32::consts::FRAC_PI_2 * (1. - open),
        )))
    };
    let icon = Icon::ChevronDown.el(zpx(12.));
    let icon = match recent_toggle(row_id, rc) {
        Some(n) => icon
            .with_animation(
                ElementId::Name(format!("chevron-{row_id}-{n}").into()),
                gpui::Animation::new(crate::motion::BASE).with_easing(crate::motion::ease_out),
                move |icon, delta| turned(icon, if expanded { delta } else { 1. - delta }),
            )
            .into_any_element(),
        None => turned(icon, if expanded { 1. } else { 0. }).into_any_element(),
    };
    div()
        .flex_none()
        .text_color(c(rc.tokens.text_faint))
        .child(icon)
}

/// A folded row's body. Opening grows it from nothing to about `height` px
/// while it fades in (200 ms, decelerating); closing shrinks and fades it out
/// (160 ms, accelerating), after which it is gone. `height` is an estimate:
/// once open, the body takes its real height.
fn fold_body(
    body: gpui::Div,
    row_id: &str,
    expanded: bool,
    height: f32,
    rc: &RowContext,
) -> Option<AnyElement> {
    use gpui::AnimationExt as _;
    let Some(n) = recent_toggle(row_id, rc) else {
        return expanded.then(|| body.into_any_element());
    };
    let (duration, easing): (_, fn(f32) -> f32) = if expanded {
        (crate::motion::SLOW, crate::motion::ease_out)
    } else {
        (crate::motion::BASE, crate::motion::ease_in_out)
    };
    let height = height.max(24.);
    Some(
        div()
            .overflow_hidden()
            .child(body)
            .with_animation(
                ElementId::Name(format!("fold-{row_id}-{n}").into()),
                gpui::Animation::new(duration).with_easing(easing),
                move |el, delta| {
                    if expanded && delta >= 1. {
                        return el;
                    }
                    let shown = if expanded { delta } else { 1. - delta };
                    el.max_h(px(height * shown)).opacity(shown)
                },
            )
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::MessageBlockState;

    fn para(id: &str, content: &str) -> TranscriptBlock {
        TranscriptBlock::Paragraph {
            id: id.to_string(),
            content: content.to_string(),
            spans: Vec::new(),
        }
    }

    /// A transcript that opened a while ago, so live rows may animate.
    fn live_transcript() -> TranscriptState {
        let mut transcript = TranscriptState::new(false);
        transcript.opened_at = std::time::Instant::now() - std::time::Duration::from_secs(5);
        transcript
    }

    #[test]
    fn resume_failure_row_identifies_error_with_actions() {
        let item = TranscriptRowItem::new("err1", "error", para("err1#0", "⚠ Couldn't resume this session"));
        assert_eq!(item.role, "error");
        let is_resume_error = item.role == "error" && match &item.block {
            TranscriptBlock::Paragraph { content, .. } => content.contains("Couldn't resume this session"),
            _ => false,
        };
        assert!(is_resume_error, "resume error must be detected for custom card rendering");
    }

    #[test]
    fn history_and_a_fresh_open_render_rows_without_motion() {
        let mut idle = live_transcript();
        idle.upsert_message("a", "assistant", vec![para("a#0", "old")]);
        assert!(
            idle.items[0].enter.is_none(),
            "an idle chat's rows don't animate"
        );
        let mut opening = TranscriptState::new(false);
        opening.animate_new = true;
        opening.upsert_message("a", "assistant", vec![para("a#0", "loaded")]);
        assert!(
            opening.items[0].enter.is_none(),
            "rows right after open are the load, not live"
        );
        let mut reduced = TranscriptState::new(true);
        reduced.opened_at = std::time::Instant::now() - std::time::Duration::from_secs(5);
        reduced.animate_new = true;
        reduced.upsert_message("a", "assistant", vec![para("a#0", "x")]);
        assert!(reduced.items[0].enter.is_none(), "reduced motion");
    }

    #[test]
    fn live_rows_enter_once_and_keep_their_identity() {
        let mut transcript = live_transcript();
        transcript.animate_new = true;
        transcript.upsert_message("a", "assistant", vec![para("a#0", "one")]);
        let first = transcript.items[0].enter.expect("a live row enters");
        // Content updates keep the stamp: the entrance never replays.
        transcript.upsert_message("a", "assistant", vec![para("a#0", "one two")]);
        assert_eq!(transcript.items[0].enter, Some(first));
        // A burst staggers 30 ms apart, three steps at most.
        transcript.upsert_message(
            "a",
            "assistant",
            vec![
                para("a#0", "one two"),
                para("a#1", "b"),
                para("a#2", "c"),
                para("a#3", "d"),
                para("a#4", "e"),
            ],
        );
        let stamps: Vec<_> = transcript.items.iter().map(|r| r.enter.unwrap()).collect();
        assert_eq!(stamps[0], first);
        let gap = |i: usize| stamps[i].saturating_duration_since(stamps[1]);
        assert_eq!(gap(2), crate::motion::ROW_STAGGER);
        assert_eq!(gap(4), gap(3), "stagger is capped");
        // The live→saved swap redelivers the same ids: nothing new enters.
        transcript.animate_new = false;
        let before: Vec<_> = transcript.items.iter().map(|r| r.enter).collect();
        transcript.upsert_message(
            "a",
            "assistant",
            vec![
                para("a#0", "one two"),
                para("a#1", "b"),
                para("a#2", "c"),
                para("a#3", "d"),
                para("a#4", "e"),
            ],
        );
        assert_eq!(
            transcript.items.iter().map(|r| r.enter).collect::<Vec<_>>(),
            before
        );
    }

    #[test]
    fn removed_live_rows_fade_out_then_are_reaped() {
        let mut transcript = live_transcript();
        transcript.animate_new = true;
        transcript.upsert_message(
            "a",
            "assistant",
            vec![para("a#0", "keep"), para("a#1", "go")],
        );
        transcript.upsert_message("a", "assistant", vec![para("a#0", "keep")]);
        assert_eq!(
            transcript.items.len(),
            2,
            "the dropped row stays as a ghost"
        );
        assert!(transcript.items[1].leaving.is_some());
        assert_eq!(message_text(&transcript.items, 1).as_deref(), Some("keep"));
        let now = std::time::Instant::now();
        assert!(transcript.reap(now), "still moving");
        assert_eq!(transcript.items.len(), 2);
        transcript.reap(now + crate::motion::ROW_EXIT + std::time::Duration::from_millis(400));
        assert_eq!(transcript.items.len(), 1);
        assert_eq!(transcript.items[0].row_id(), "a#0");
        // A row that returns before it's reaped is drawn anew, beside the ghost.
        transcript.upsert_message(
            "a",
            "assistant",
            vec![para("a#0", "keep"), para("a#1", "go")],
        );
        transcript.upsert_message("a", "assistant", vec![para("a#0", "keep")]);
        transcript.upsert_message(
            "a",
            "assistant",
            vec![para("a#0", "keep"), para("a#1", "back")],
        );
        let live: Vec<_> = transcript
            .items
            .iter()
            .filter(|r| r.leaving.is_none())
            .map(|r| r.row_id().to_string())
            .collect();
        assert_eq!(live, vec!["a#0", "a#1"]);
    }

    #[test]
    fn user_points_are_the_first_row_of_each_user_message() {
        let mut transcript = TranscriptState::new(true);
        transcript.upsert_message(
            "a",
            "user",
            vec![para("a1", "fix the login"), para("a2", "please")],
        );
        transcript.upsert_message("b", "assistant", vec![para("b1", "done")]);
        transcript.upsert_message("c", "user", vec![para("c1", "  now\nadd a test  ")]);
        assert_eq!(
            transcript.user_points(),
            vec![
                (0, "fix the login".to_string()),
                (3, "now add a test".to_string())
            ]
        );
        transcript.scroll_to_row(3);
        assert!(!transcript.stick_to_bottom);
        assert_eq!(transcript.list_state.logical_scroll_top().item_ix, 3);
        transcript.scroll_to_row(99);
        assert_eq!(transcript.list_state.logical_scroll_top().item_ix, 3);
    }

    #[test]
    fn image_refs_parse_with_and_without_local_paths() {
        let view = serde_json::json!({"attachments": [
            {"id": "x", "name": "shot.png", "mime": "image/png", "path": "/data/attachments/x"},
            {"id": "y", "name": "remote.png", "mime": "image/png", "path": null}
        ]});
        let refs = ImageRef::parse_list(&view);
        assert_eq!(
            refs[0],
            ImageRef {
                name: "shot.png".into(),
                path: Some("/data/attachments/x".into())
            }
        );
        assert_eq!(refs[1].path, None);
        assert!(ImageRef::parse_list(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn tool_group_labels_and_parse() {
        let view = serde_json::json!({"tools": [
            {"tool_id": "1", "name": "bash", "state": "done", "summary": "ok"},
            {"tool_id": "2", "name": "read", "state": "error", "summary": "no such file"},
            {"tool_id": "3", "name": "bash", "state": "running", "summary": ""}
        ]});
        let tools = ToolEntry::parse_list(&view);
        assert_eq!(tools.len(), 3);
        assert_eq!(tools[1].state, "error");
        assert_eq!(
            tool_group_labels(&tools),
            ("Running bash".to_string(), vec!["bash", "read"])
        );
        assert_eq!(tool_group_labels(&tools[..1]).0, "Ran bash");
        assert_eq!(tool_group_labels(&tools[..2]).0, "Ran 2 tools");
        assert!(ToolEntry::parse_list(&serde_json::json!({"text": "x"})).is_empty());
    }

    #[test]
    fn browser_tools_read_as_steps_not_mcp_names() {
        let tool = |name: &str, state: &str| ToolEntry {
            tool_id: name.into(),
            name: name.into(),
            state: state.into(),
            summary: String::new(),
            ..Default::default()
        };
        let shot = browser_step("mcp__keel_browser__browser_screenshot").unwrap();
        assert_eq!(
            (shot.doing, shot.done, shot.icon),
            ("Taking screenshot", "Took screenshot", Icon::Camera)
        );
        assert_eq!(
            browser_step("mcp__keel-browser__navigate").unwrap().done,
            "Opened page"
        );
        assert_eq!(
            browser_step("browser_open_pane").unwrap().doing,
            "Opening browser"
        );
        assert_eq!(browser_step("mcp__github__create_issue"), None);
        assert_eq!(browser_step("Bash"), None);

        let run = [
            tool("mcp__keel_browser__browser_navigate", "done"),
            tool("mcp__keel_browser__browser_click", "done"),
            tool("mcp__keel_browser__browser_click", "done"),
            tool("mcp__keel_browser__browser_screenshot", "running"),
        ];
        assert_eq!(tool_group_labels(&run).0, "Taking screenshot");

        let mixed = [
            tool("Bash", "done"),
            tool("mcp__keel_browser__browser_screenshot", "done"),
        ];
        assert_eq!(
            tool_group_labels(&mixed),
            ("Ran 2 tools".to_string(), vec!["Bash", "Took screenshot"])
        );
        assert_eq!(tool_group_labels(&mixed[1..]).0, "Took screenshot");
    }

    #[test]
    fn near_bottom_wheel_down_does_not_switch_the_list_to_tail() {
        let mut transcript = TranscriptState::new(false);
        transcript.handle_scroll(px(200.0), zpx(10.0));
        transcript.set_stick(transcript.stick_to_bottom);
        assert!(!transcript.list_state.is_following_tail());
        // Scrolling down to 30 px above the bottom: the user's position stands.
        transcript.handle_scroll(px(30.0), zpx(-12.0));
        transcript.set_stick(transcript.stick_to_bottom);
        assert!(!transcript.stick_to_bottom);
        assert!(
            !transcript.list_state.is_following_tail(),
            "Tail would snap to the end"
        );
    }

    #[test]
    fn wheel_up_with_nothing_to_scroll_stays_sticky() {
        let mut transcript = TranscriptState::new(false);
        transcript.upsert_message("m", "assistant", vec![para("m#0", "short")]);
        transcript.on_wheel(zpx(40.));
        assert!(transcript.stick_to_bottom);
    }

    #[test]
    fn rows_keep_their_age_when_a_token_updates_them() {
        let mut transcript = TranscriptState::new(false);
        transcript.upsert_message("m", "assistant", vec![para("m#0", "a")]);
        let born = transcript.items[0].born;
        std::thread::sleep(std::time::Duration::from_millis(5));
        transcript.upsert_message("m", "assistant", vec![para("m#0", "ab")]);
        assert_eq!(transcript.items[0].born, born);
        transcript.upsert_message("m", "assistant", vec![para("m#0", "ab"), para("m#1", "c")]);
        assert!(transcript.items[1].born > born);
    }

    #[test]
    fn three_token_deltas_remeasures_one_row() {
        let mut transcript = TranscriptState::new(false);
        let mut msg = MessageBlockState::new("msg_turn".into());
        msg.append_delta("Intro paragraph.\n\nStart");
        transcript.upsert_message("msg_turn", "assistant", msg.all_blocks());
        assert_eq!(transcript.items.len(), 2);
        let initial = transcript.measure_count;

        for delta in [" word1", " word2", " word3"] {
            msg.append_delta(delta);
            transcript.upsert_message("msg_turn", "assistant", msg.all_blocks());
            assert_eq!(transcript.last_measured_row.as_deref(), Some("msg_turn#1"));
        }

        // Exactly one row (the tail) remeasured per delta; the first row untouched.
        assert_eq!(transcript.items.len(), 2);
        assert_eq!(transcript.measure_count, initial + 3);
        assert_eq!(
            transcript.items[1].block,
            para("msg_turn#1", "Start word1 word2 word3")
        );
    }

    /// A reopened chat publishes rows as they are ready: the tool-only turn
    /// first, the user message (after its parse) second. Chat order wins.
    #[test]
    fn late_message_slots_in_before_later_ones() {
        let mut transcript = TranscriptState::new(false);
        let later = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        transcript.upsert_message_before(
            "a1",
            "assistant",
            vec![para("a1#0", "tools")],
            &later(&["u2", "a2"]),
        );
        transcript.upsert_message_before("a2", "assistant", vec![para("a2#0", "more")], &[]);
        transcript.upsert_message_before(
            "u1",
            "user",
            vec![para("u1#0", "summon"), para("u1#1", "it")],
            &later(&["a1", "u2", "a2"]),
        );
        transcript.upsert_message_before(
            "u2",
            "user",
            vec![para("u2#0", "issue?")],
            &later(&["a2"]),
        );
        let order: Vec<_> = transcript
            .items
            .iter()
            .map(|r| r.row_id().to_string())
            .collect();
        assert_eq!(order, ["u1#0", "u1#1", "a1#0", "u2#0", "a2#0"]);
    }

    #[test]
    fn live_and_persisted_rows_for_same_message_merge() {
        let mut transcript = TranscriptState::new(false);
        transcript.upsert_message("u1", "user", vec![para("u1#0", "hi")]);
        // Live streaming rows for the assistant message.
        transcript.upsert_message("m1", "assistant", vec![para("m1#0", "Hel")]);
        // Persisted message (MirrorDiff) with the same id and full text.
        transcript.upsert_message(
            "m1",
            "assistant",
            vec![para("m1#0", "Hello"), para("m1#1", "World")],
        );
        // Re-delivery of the persisted user message is a no-op.
        let before = transcript.measure_count;
        transcript.upsert_message("u1", "user", vec![para("u1#0", "hi")]);
        assert_eq!(transcript.measure_count, before);

        let ids: Vec<_> = transcript
            .items
            .iter()
            .map(|r| r.row_id().to_string())
            .collect();
        assert_eq!(ids, vec!["u1#0", "m1#0", "m1#1"]);
        assert_eq!(transcript.list_state.item_count(), 3);

        // Shrinking removes surplus rows.
        transcript.upsert_message("m1", "assistant", vec![para("m1#0", "Hello")]);
        assert_eq!(transcript.items.len(), 2);
        assert_eq!(transcript.list_state.item_count(), 2);
    }

    #[test]
    fn code_block_height_memoized_and_independent_of_highlight() {
        let mut transcript = TranscriptState::new(false);
        let block = TranscriptBlock::Code {
            id: "m_code#0".to_string(),
            language: Some("rust".to_string()),
            content: "fn test() {\n    1 + 1;\n}".to_string(),
            line_count: 3,
            spans: Vec::new(),
            closed: true,
            run: None,
        };
        let width = zpx(600.0);
        let h1 = transcript.compute_block_height(&block, width);
        // Header 28 + 2×10 padding + 3×18 lines + border 2, plus 8 margin.
        assert_eq!(h1, zpx(112.0));
        let h2 = transcript
            .height_memo
            .get("m_code#0", block.content_len(), width);
        assert_eq!(h2, Some(h1));
    }

    #[test]
    fn tool_group_height_accounts_for_errors_and_overrides() {
        let mut normal_tool = crate::markdown::ToolEntry::default();
        normal_tool.tool_id = "t1".into();
        normal_tool.state = "done".into();
        normal_tool.output_tail = "some normal output".into();

        let h_collapsed = tool_group_height("group", &[normal_tool.clone()], &[]);
        assert_eq!(h_collapsed, crate::trace_view::LINE_H * 1.0 + 4.0);

        let mut error_tool = crate::markdown::ToolEntry::default();
        error_tool.tool_id = "t2".into();
        error_tool.state = "error".into();
        error_tool.output_tail = "Error: module not found\n  at line 10\n  at line 20".into();

        let h_error = tool_group_height("group", &[error_tool.clone()], &[]);
        // Base line height + body height
        assert!(h_error > h_collapsed + 50.0);

        // When user overrides to collapsed (=0), it returns base height
        let h_overridden_closed = tool_group_height(
            "group",
            &[error_tool.clone()],
            &[("group~t2".into(), false)],
        );
        assert_eq!(h_overridden_closed, crate::trace_view::LINE_H * 1.0 + 4.0);
    }

    #[test]
    fn scroll_interrupt_and_re_engage_only_at_the_bottom() {
        let mut transcript = TranscriptState::new(false);
        assert!(transcript.stick_to_bottom);

        transcript.handle_scroll(zpx(150.0), zpx(20.0));
        assert!(!transcript.stick_to_bottom);

        transcript.handle_scroll(zpx(100.0), zpx(-10.0));
        assert!(!transcript.stick_to_bottom);

        transcript.handle_scroll(zpx(65.0), zpx(-10.0));
        assert!(!transcript.stick_to_bottom);

        // A zero-delta event near the bottom (trackpad gesture end) is not a re-engage.
        transcript.handle_scroll(zpx(10.0), zpx(0.0));
        assert!(!transcript.stick_to_bottom);

        // Near the bottom is not the bottom: no follow, so no snap down.
        transcript.handle_scroll(px(35.0), zpx(-10.0));
        assert!(!transcript.stick_to_bottom);
        transcript.handle_scroll(px(3.0), zpx(-10.0));
        assert!(!transcript.stick_to_bottom);

        transcript.handle_scroll(px(2.0), zpx(-10.0));
        assert!(transcript.stick_to_bottom);
        transcript.handle_scroll(px(20.0), zpx(10.0));
        transcript.handle_scroll(px(0.0), zpx(-10.0));
        assert!(transcript.stick_to_bottom);

        transcript.stick_to_bottom = false;
        transcript.on_own_send();
        assert!(transcript.stick_to_bottom);
    }

    #[test]
    fn narration_folding_behind_the_step_line_leaves_at_once() {
        let mut transcript = live_transcript();
        transcript.animate_new = true;
        transcript.upsert_message("a", "assistant", vec![para("a#0", "I'll look up the news.")]);
        let summary = TranscriptBlock::TurnSummary {
            id: "a#worked".into(),
            label: "Searching".into(),
            failed: false,
            expanded: false,
            live: true,
        };
        transcript.upsert_message("a", "assistant", vec![summary]);
        assert_eq!(transcript.items.len(), 1, "no ghost of the narration");
        assert!(transcript.items.iter().all(|r| r.leaving.is_none()));
    }

    #[test]
    fn narration_already_exiting_is_removed_when_the_trace_appears() {
        let mut transcript = live_transcript();
        transcript.animate_new = true;
        transcript.upsert_message("a", "assistant", vec![para("a#0", "I'll look up the news.")]);
        transcript.upsert_message("a", "assistant", vec![]);
        assert!(transcript.items[0].leaving.is_some());
        transcript.upsert_message("a", "assistant", vec![TranscriptBlock::TurnSummary {
            id: "a#worked".into(),
            label: "Searching".into(),
            failed: false,
            expanded: false,
            live: true,
        }]);
        assert_eq!(transcript.items.len(), 1, "even an existing exit ghost must not paint");
        assert_eq!(transcript.items[0].row_id(), "a#worked");
    }

    #[test]
    fn narration_in_the_fold_is_as_tall_as_its_prose() {
        let mut transcript = TranscriptState::new(false);
        let text = "Found the design docs and wiki pages.\nReading those next.";
        let plain = transcript.compute_block_height(&para("a#0", text), px(600.));
        let note = TranscriptBlock::Interim(Box::new(para("a#0", text)));
        assert_eq!(note.id(), "a#0");
        assert_eq!(transcript.compute_block_height(&note, px(600.)), plain);
        // Copy gives the answer, not the trace's narration.
        assert_eq!(block_markdown(&note), None);
    }

    #[test]
    fn rows_are_spaced_by_what_they_follow() {
        let row =
            |m: &str, role: &str, block: TranscriptBlock| TranscriptRowItem::new(m, role, block);
        let h = |level: u8| TranscriptBlock::Heading {
            id: "h".into(),
            level,
            content: "Where things stand".into(),
            spans: Vec::new(),
        };
        let tools = || TranscriptBlock::ToolGroup {
            id: "t".into(),
            tools: Vec::new(),
            expanded: false,
            overrides: Vec::new(),
        };
        let p1 = row("a", "assistant", para("a#0", "one"));
        let p2 = row("a", "assistant", para("a#1", "two"));
        let h2 = row("a", "assistant", h(2));
        let tg = row("a", "assistant", tools());
        let tg2 = row("a", "assistant", tools());
        // Paragraphs ~0.8em apart; a heading has more room above than below.
        assert_eq!(row_gap(Some(&p1), &p2), 11.);
        assert!(row_gap(Some(&p1), &h2) > row_gap(Some(&h2), &p2) * 2.);
        // Text never butts against trace lines; trace lines stay tight together.
        assert!(row_gap(Some(&p1), &tg) >= 14.);
        assert!(row_gap(Some(&tg), &p2) >= 14.);
        assert_eq!(row_gap(Some(&tg), &tg2), 2.);
        // Narration inside an open fold sits among the trace lines, closer
        // than paragraphs; the answer after the trace still gets its room.
        let note = row("a", "assistant", TranscriptBlock::Interim(Box::new(para("a#2", "next"))));
        assert_eq!(row_gap(Some(&tg), &note), 6.);
        assert_eq!(row_gap(Some(&note), &tg2), 6.);
        assert!(row_gap(Some(&note), &p2) >= 14.);
        // A new message's text after another's reads as a new block.
        let next = row("b", "assistant", para("b#0", "three"));
        assert!(row_gap(Some(&p2), &next) > row_gap(Some(&p1), &p2));
        assert_eq!(list_marker("•", 1), "◦");
        assert_eq!(list_marker("3.", 1), "3.");
    }

    #[test]
    fn clear_drops_rows_and_resets_stickiness() {
        let mut transcript = TranscriptState::new(false);
        transcript.upsert_message("m", "assistant", vec![para("m#0", "x")]);
        transcript.stick_to_bottom = false;
        transcript.clear();
        assert!(transcript.items.is_empty());
        assert_eq!(transcript.list_state.item_count(), 0);
        assert!(transcript.stick_to_bottom);
    }

    #[test]
    fn message_text_joins_a_message_on_its_last_row() {
        let rows = vec![
            TranscriptRowItem::new("u", "user", para("u#0", "hi")),
            TranscriptRowItem::new(
                "a",
                "assistant",
                TranscriptBlock::Heading {
                    id: "a#0".into(),
                    level: 2,
                    content: "Plan".into(),
                    spans: Vec::new(),
                },
            ),
            TranscriptRowItem::new(
                "a",
                "assistant",
                TranscriptBlock::ToolGroup {
                    id: "a#1".into(),
                    tools: Vec::new(),
                    expanded: false,
                    overrides: Vec::new(),
                },
            ),
            TranscriptRowItem::new("a", "assistant", para("a#2", "done")),
        ];
        assert_eq!(message_text(&rows, 0).as_deref(), Some("hi"));
        assert_eq!(message_text(&rows, 1), None);
        assert_eq!(message_text(&rows, 3).as_deref(), Some("## Plan\n\ndone"));
    }

    #[test]
    fn url_host_extracts_clean_domain() {
        assert_eq!(url_host("https://apnews.com/article/123"), "apnews.com");
        assert_eq!(url_host("https://www.bbc.co.uk/news"), "bbc.co.uk");
        assert_eq!(url_host("http://localhost:3000/app"), "localhost:3000");
        assert_eq!(
            url_host("https://github.com/rust-lang/rust?tab=readme"),
            "github.com"
        );
    }

    #[test]
    fn url_chip_label_preserves_anchor_or_formats_url() {
        assert_eq!(url_chip_label("AP", "https://apnews.com"), "AP");
        assert_eq!(url_chip_label("BBC News", "https://bbc.com"), "BBC News");
        assert_eq!(
            url_chip_label(
                "https://example.com/very/long/nested/path/to/resource",
                "https://example.com"
            ),
            "example.com/…"
        );
    }

    #[test]
    fn host_favicon_path_creates_valid_file_path() {
        let path = host_favicon_path("gitlab.dot.co.id");
        assert!(path.ends_with("gitlab.dot.co.id.png"));
    }

    #[test]
    fn download_favicon_image_retrieves_site_icon() {
        let img = download_favicon_image("gitlab.dot.co.id");
        assert!(img.is_some());
        let img = img.unwrap();
        assert!(img.width() <= 64 && img.height() <= 64);
        assert!(img.width() > 0 && img.height() > 0);

        start_favicon_fetch("gitlab.dot.co.id");
        let path = host_favicon_path("gitlab.dot.co.id");
        for _ in 0..30 {
            if path.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(path.exists());
    }

    #[test]
    fn looks_like_file_path_identifies_relative_and_typed_files() {
        assert_eq!(
            looks_like_file_path("interface/http/handler/auth_handler.go", false).as_deref(),
            Some("interface/http/handler/auth_handler.go")
        );
        assert_eq!(
            looks_like_file_path("app/auth/usecase/auth_usecase.go", false).as_deref(),
            Some("app/auth/usecase/auth_usecase.go")
        );
        assert_eq!(
            looks_like_file_path("docs/wiki/how-to-login-oidc-manual.md", false).as_deref(),
            Some("docs/wiki/how-to-login-oidc-manual.md")
        );
        assert_eq!(
            looks_like_file_path("Cargo.toml", true).as_deref(),
            Some("Cargo.toml")
        );
        assert_eq!(
            looks_like_file_path("main.rs", true).as_deref(),
            Some("main.rs")
        );
        assert_eq!(
            looks_like_file_path("Makefile", false).as_deref(),
            Some("Makefile")
        );
        assert_eq!(
            looks_like_file_path(".gitignore", false).as_deref(),
            Some(".gitignore")
        );
        assert_eq!(looks_like_file_path("v1.2.3", true), None);
        assert_eq!(looks_like_file_path("3.14", true), None);
        assert_eq!(
            looks_like_file_path("https://example.com/foo.go", true),
            None
        );
        // Web endpoints should not be confused with file paths:
        assert_eq!(looks_like_file_path("/llms.txt", true), None);
        assert_eq!(looks_like_file_path("/robots.txt", true), None);
        assert_eq!(looks_like_file_path("/sitemap.xml", true), None);
        assert_eq!(looks_like_file_path("/manifest.webmanifest", true), None);
        assert_eq!(looks_like_file_path("/api/v1/users", true), None);
        assert_eq!(looks_like_file_path("/api/v1/users?page=1", true), None);
        assert_eq!(looks_like_file_path("/users/{id}", true), None);
        // Absolute filesystem path to a real path prefix:
        assert_eq!(
            looks_like_file_path("/Users/user/project/main.rs", true).as_deref(),
            Some("/Users/user/project/main.rs")
        );
        // npm-style temp dirs and log paths that overflowed user bubbles:
        assert_eq!(
            looks_like_file_path(
                "/Users/kevinananda/Library/pnpm/nodejs/20.20.2/lib/node_modules/@xai-official/.grok-1",
                false
            )
            .as_deref(),
            Some(
                "/Users/kevinananda/Library/pnpm/nodejs/20.20.2/lib/node_modules/@xai-official/.grok-1"
            )
        );
        assert_eq!(
            looks_like_file_path(
                "/Users/kevinananda/.npm/_logs/2026-11-06T09_43_20_101Z-debug-0.log",
                false
            )
            .as_deref(),
            Some("/Users/kevinananda/.npm/_logs/2026-11-06T09_43_20_101Z-debug-0.log")
        );
        // A bare package name is not a file:
        assert_eq!(
            looks_like_file_path(
                "/Users/kevinananda/Library/pnpm/nodejs/20.20.2/lib/node_modules/@xai-official/grok",
                false
            ),
            None
        );
    }

    fn code_span(_text: &str, start: usize, end: usize) -> InlineSpan {
        InlineSpan {
            range: start..end,
            style: crate::markdown::InlineStyle {
                code: true,
                ..Default::default()
            },
            link: None,
        }
    }

    #[test]
    fn needs_inline_chips_keeps_multiword_commands_as_text() {
        let text = "Run them with go run main.go --migration true --exec up if ready.";
        let start = text.find("go run").unwrap();
        let end = text.find(" if ready").unwrap();
        let spans = vec![code_span(text, start, end)];
        assert!(
            !needs_inline_chips(text, &spans),
            "main.go inside a multi-word code span must not open the chip layout"
        );
    }

    #[test]
    fn needs_inline_chips_for_whole_span_paths_and_plain_paths() {
        let text = "See main.go for details.";
        let start = text.find("main.go").unwrap();
        let end = start + "main.go".len();
        assert!(needs_inline_chips(
            text,
            &[code_span(text, start, end)],
        ));
        assert!(needs_inline_chips("Open src/main.go please.", &[]));
        assert!(!needs_inline_chips("No paths here.", &[]));
        let linked = InlineSpan {
            range: 0..4,
            style: Default::default(),
            link: Some("https://example.com".into()),
        };
        assert!(needs_inline_chips("here", &[linked]));
    }


}

/// The markdown fence as a full [`crate::code_block`]: show-more lives in
/// the view's toggle set as `{row}~more`.
fn fence_props(
    item: &TranscriptRowItem,
    language: &Option<String>,
    content: &str,
    spans: &crate::syntax::Spans,
    closed: bool,
    runnable: bool,
    rc: &RowContext,
) -> crate::code_block::CodeBlockProps {
    let row = item.row_id();
    let more = rc
        .open_lines
        .get(&format!("{row}~more"))
        .copied()
        .unwrap_or(false);
    let mut props =
        crate::code_block::CodeBlockProps::new(row, content, crate::code_block::CodeSize::Full);
    props.language = language.clone();
    props.spans = Some(spans.clone());
    props.closed = closed;
    props.expanded = more;
    props.copied = rc.copied.as_deref() == Some(row);
    props.copied_at = rc.copied_at.filter(|_| props.copied);
    props.toggled_at = rc.toggles.get(&format!("{row}~more")).map(|(_, at)| *at);
    props.reduced = rc.reduced;
    props.runnable = runnable;
    props
}


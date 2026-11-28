//! Isolated GPUI view hosting the transcript `gpui::list`.
//!
//! Keeping the `list` inside its own small `Entity` (separate from `KeelWindow`
//! and `ChatTimelineView`) ensures:
//! - Wheel events only notify this view via `ListState::scroll`, allowing
//!   `KeelWindow` to splice cleanly instead of re-rendering sidebar/header/composer.
//! - Timeline and chrome animations on `KeelWindow` / `ChatTimelineView` never
//!   trigger `owner_animating` (`Decision::Bypass`) on `gpui-fast`'s scroll layer.
//! - Row hover uses `.on_hover` so `gpui-fast`'s `hover_freeze` holds hovers
//!   still during active scroll without per-frame `request_animation_frame` or
//!   `window.refresh()` invalidations.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, Context, ElementId, Entity, EntityId, InteractiveElement, IntoElement, ListState,
    ParentElement, Pixels, Render, ScrollEvent, StatefulInteractiveElement, Styled, WeakEntity,
    Window, div, list, prelude::*, px,
};
use keel_theme::Tokens;

use super::main_pane::{InlineLoader, TAIL_ROOM};
use crate::style::{HEADER_HEIGHT, zpx};
use crate::transcript::{OnRowAction, RowContext, TranscriptRowItem, TranscriptState, render_row};

/// Isolated Copy button entity per message so hovering across rows notifies
/// only the 1–2 affected copy views instead of rebuilding `TranscriptListView`.
pub(super) struct MessageCopyView {
    item: TranscriptRowItem,
    items: Rc<Vec<TranscriptRowItem>>,
    items_gen: u64,
    ix: usize,
    shown: bool,
    copied: Option<String>,
    tokens: Tokens,
    on_action: OnRowAction,
}

impl MessageCopyView {
    fn sync(
        &mut self,
        item: TranscriptRowItem,
        items: Rc<Vec<TranscriptRowItem>>,
        items_gen: u64,
        ix: usize,
        shown: bool,
        copied: Option<String>,
        tokens: Tokens,
        on_action: OnRowAction,
        cx: &mut Context<Self>,
    ) {
        if self.items_gen == items_gen
            && self.ix == ix
            && self.shown == shown
            && self.copied == copied
            && self.tokens == tokens
        {
            return;
        }
        self.item = item;
        self.items = items;
        self.items_gen = items_gen;
        self.ix = ix;
        self.shown = shown;
        self.copied = copied;
        self.tokens = tokens;
        self.on_action = on_action;
        cx.notify();
    }

    fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        if self.shown != shown {
            self.shown = shown;
            cx.notify();
        }
    }
}

impl Render for MessageCopyView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        crate::hover_fade::in_list_scope(|| {
            crate::transcript::message_copy_parts(
                &self.item,
                self.items.clone(),
                self.ix,
                self.shown,
                self.copied.as_deref(),
                self.tokens,
                self.on_action.clone(),
            )
        })
    }
}

/// Tracks which transcript row/message is under the pointer for the `Copy` action.
#[derive(Default)]
pub(super) struct RowHover {
    row: Cell<Option<usize>>,
    msg_id: RefCell<Option<String>>,
    copy_views: RefCell<HashMap<String, WeakEntity<MessageCopyView>>>,
}

impl RowHover {
    pub(super) fn clear(&self) {
        self.row.set(None);
        *self.msg_id.borrow_mut() = None;
        self.copy_views.borrow_mut().clear();
    }

    pub(super) fn is_msg_hovered(&self, message_id: &str) -> bool {
        self.msg_id.borrow().as_deref() == Some(message_id)
    }

    fn register_copy(&self, msg_id: &str, view: &Entity<MessageCopyView>) {
        let mut views = self.copy_views.borrow_mut();
        views.insert(msg_id.to_string(), view.downgrade());
    }

    fn prune_copy_views(&self, items: &[TranscriptRowItem]) {
        if items.is_empty() {
            self.copy_views.borrow_mut().clear();
            return;
        }
        let mut views = self.copy_views.borrow_mut();
        if views.len() > items.len().saturating_add(16) {
            views.retain(|_, v| v.upgrade().is_some());
        }
    }

    fn notify_message(&self, msg_id: Option<&str>, shown: bool, cx: &mut gpui::App) {
        let Some(msg_id) = msg_id else { return };
        let weak = self.copy_views.borrow().get(msg_id).cloned();
        if let Some(weak) = weak {
            let _ = weak.update(cx, |view, cx| view.set_shown(shown, cx));
        }
    }

    /// Updates hovered row/message on enter; returns `true` when the hovered message changed.
    fn enter(&self, ix: usize, msg_id: Option<&str>) -> bool {
        self.row.set(Some(ix));
        let mut current = self.msg_id.borrow_mut();
        if current.as_deref() == msg_id {
            return false;
        }
        *current = msg_id.map(str::to_string);
        true
    }

    /// Clears hover when leaving `ix` if `ix` is still the active hovered row;
    /// returns `true` when the hovered message changed.
    fn leave(&self, ix: usize) -> bool {
        if self.row.get() != Some(ix) {
            return false;
        }
        self.row.set(None);
        self.msg_id.borrow_mut().take().is_some()
    }

    fn enter_and_notify(&self, ix: usize, msg_id: Option<&str>, cx: &mut gpui::App) {
        let prev = self.msg_id.borrow().clone();
        if !self.enter(ix, msg_id) {
            return;
        }
        if prev.as_deref() != msg_id {
            self.notify_message(prev.as_deref(), false, cx);
            self.notify_message(msg_id, true, cx);
        }
    }

    fn leave_and_notify(&self, ix: usize, cx: &mut gpui::App) {
        let prev = self.msg_id.borrow().clone();
        if !self.leave(ix) {
            return;
        }
        self.notify_message(prev.as_deref(), false, cx);
    }
}

pub(super) struct TranscriptListProps {
    pub(super) list_state: ListState,
    pub(super) items: Rc<Vec<TranscriptRowItem>>,
    pub(super) items_gen: u64,
    pub(super) rc: Rc<RowContext>,
    pub(super) column: Pixels,
    pub(super) dock_h: f32,
    pub(super) loader_row: Option<usize>,
    pub(super) loader: Option<InlineLoader>,
}

pub(super) struct TranscriptListView {
    pub(super) props: Option<TranscriptListProps>,
    hovered_row: Rc<RowHover>,
    copy_views: Rc<RefCell<HashMap<String, Entity<MessageCopyView>>>>,
    timeline_id: EntityId,
    timeline_scroll_until: Rc<Cell<Option<Instant>>>,
    stick_flag: Rc<Cell<bool>>,
    glide_active: Rc<Cell<bool>>,
    on_wheel_sync: Rc<dyn Fn(Pixels, &mut gpui::App)>,
}

impl TranscriptListView {
    fn is_animating(props: &TranscriptListProps, now: Instant, now_ms: i64) -> bool {
        let rc = &props.rc;
        let fold_window = if rc.reduced {
            Duration::from_millis(160)
        } else {
            Duration::from_millis(360)
        };
        if rc
            .toggles
            .values()
            .any(|(_, at)| now.saturating_duration_since(*at) < fold_window)
        {
            return true;
        }
        if rc
            .copied_at
            .is_some_and(|at| !rc.reduced && now.saturating_duration_since(at) < Duration::from_millis(1600))
        {
            return true;
        }
        if rc.step_from.as_ref().is_some_and(|s| s.running(now))
            || rc.tool_from.values().any(|s| s.running(now))
        {
            return true;
        }
        if !rc.reduced
            && props.items.iter().any(|r| {
                r.look(now, rc.reduced).is_some()
                    || crate::trace_view::row_animating_at(r, now_ms)
            })
        {
            return true;
        }
        false
    }

    /// Build the frame's `RowContext` using current timestamps even after
    /// animations finish so folds and row entrances stay settled instead of
    /// rewinding to `props.rc.now`.
    fn frame_rc(props: &TranscriptListProps, now: Instant, now_ms: i64) -> Rc<RowContext> {
        Rc::new(RowContext {
            now,
            now_ms,
            ..(*props.rc).clone()
        })
    }

    pub(super) fn new(
        hovered_row: Rc<RowHover>,
        timeline_id: EntityId,
        timeline_scroll_until: Rc<Cell<Option<Instant>>>,
        stick_flag: Rc<Cell<bool>>,
        glide_active: Rc<Cell<bool>>,
        on_wheel_sync: Rc<dyn Fn(Pixels, &mut gpui::App)>,
    ) -> Self {
        Self {
            props: None,
            hovered_row,
            copy_views: Rc::new(RefCell::new(HashMap::new())),
            timeline_id,
            timeline_scroll_until,
            stick_flag,
            glide_active,
            on_wheel_sync,
        }
    }
}

impl Render for TranscriptListView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(props) = self.props.as_ref() else {
            return div().id("transcript-list-host").size_full();
        };

        let items = props.items.clone();
        let items_gen = props.items_gen;
        let now = Instant::now();
        let now_ms = crate::trace_view::now_ms();
        if Self::is_animating(props, now, now_ms) {
            window.request_animation_frame();
        }
        let rc = Self::frame_rc(props, now, now_ms);
        let column = props.column;
        let dock_h = props.dock_h;
        let loader_row = props.loader_row;
        let loader = props.loader;
        let hover_cell = self.hovered_row.clone();
        if items.is_empty() {
            self.copy_views.borrow_mut().clear();
        } else if self.copy_views.borrow().len() > items.len().saturating_add(16) {
            self.copy_views
                .borrow_mut()
                .retain(|id, _| items.iter().any(|r| r.message_id == *id));
        }
        hover_cell.prune_copy_views(&items);
        let copy_views = self.copy_views.clone();

        div()
            .id("transcript-list-host")
            .size_full()
            .on_scroll(cx.listener(|this, event: &ScrollEvent, window, cx| {
                let dy = event.delta.pixel_delta(window.line_height()).y;
                if dy.abs() > px(0.5) {
                    this.timeline_scroll_until
                        .set(Some(Instant::now() + Duration::from_millis(420)));
                    gpui::App::notify(&mut **cx, this.timeline_id);
                }
                let Some(props) = this.props.as_ref() else {
                    return;
                };
                let max = props.list_state.max_offset_for_scrollbar().y;
                let was_sticky = this.stick_flag.get();
                let next_sticky = if max <= zpx(0.) {
                    true
                } else if dy > zpx(0.0) {
                    false
                } else if dy < zpx(0.0) {
                    let current = -props.list_state.scroll_px_offset_for_scrollbar().y;
                    if (max - current).max(zpx(0.)) <= TranscriptState::RE_ENGAGE_THRESHOLD {
                        true
                    } else {
                        was_sticky
                    }
                } else {
                    was_sticky
                };
                if next_sticky != was_sticky || (dy != px(0.) && this.glide_active.get()) {
                    (this.on_wheel_sync)(dy, cx);
                }
            }))
            .child(
                list(props.list_state.clone(), move |ix, _window, cx| {
                    let (row, row_id_key, row_msg_id) = match items.get(ix) {
                        Some(item) => {
                            let loader = loader.filter(|_| loader_row == Some(ix));
                            let has_copy = crate::transcript::has_message_text(&items, ix);
                            let shown = hover_cell.is_msg_hovered(&item.message_id);
                            let copy_el: Option<AnyElement> = if has_copy {
                                let existing = copy_views.borrow().get(&item.message_id).cloned();
                                let entity = if let Some(entity) = existing {
                                    let needs_sync = {
                                        let cur = entity.read(cx);
                                        cur.items_gen != items_gen
                                            || cur.ix != ix
                                            || cur.shown != shown
                                            || cur.copied != rc.copied
                                            || cur.tokens != rc.tokens
                                    };
                                    if needs_sync {
                                        entity.update(cx, |v, cx| {
                                            v.sync(
                                                item.clone(),
                                                items.clone(),
                                                items_gen,
                                                ix,
                                                shown,
                                                rc.copied.clone(),
                                                rc.tokens,
                                                rc.on_action.clone(),
                                                cx,
                                            );
                                        });
                                    }
                                    entity
                                } else {
                                    let entity = cx.new(|_| MessageCopyView {
                                        item: item.clone(),
                                        items: items.clone(),
                                        items_gen,
                                        ix,
                                        shown,
                                        copied: rc.copied.clone(),
                                        tokens: rc.tokens,
                                        on_action: rc.on_action.clone(),
                                    });
                                    hover_cell.register_copy(&item.message_id, &entity);
                                    copy_views
                                        .borrow_mut()
                                        .insert(item.message_id.clone(), entity.clone());
                                    entity
                                };
                                Some(entity.into_any_element())
                            } else {
                                None
                            };
                            let prev = ix.checked_sub(1).and_then(|p| items.get(p));
                            let rendered = crate::hover_fade::in_list_scope(|| {
                                if loader.is_none() && copy_el.is_none() {
                                    render_row(ix, item, prev, &rc)
                                } else {
                                    div()
                                        .w_full()
                                        .flex()
                                        .flex_col()
                                        .child(render_row(ix, item, prev, &rc))
                                        .children(copy_el)
                                        .children(
                                            loader.map(|loader| {
                                                loader.render(rc.bot_theme.as_ref(), rc.clock_epoch)
                                            }),
                                        )
                                        .into_any()
                                }
                            });
                            (
                                rendered,
                                Some(item.row_id().to_string()),
                                Some(item.message_id.clone()),
                            )
                        }
                        None => (div().into_any(), None, None),
                    };

                    let hover = hover_cell.clone();
                    let head_pad = (ix == 0).then(|| HEADER_HEIGHT + zpx(24.));
                    let tail_pad = (ix + 1 == items.len()).then(|| zpx(TAIL_ROOM) + px(dock_h));
                    let content = div().w_full().max_w(column).child(row);
                    match row_id_key {
                        Some(id) => div()
                            .id(ElementId::Name(format!("trow-{id}").into()))
                            .relative()
                            .w_full()
                            .flex()
                            .justify_center()
                            .on_hover(move |hovered: &bool, _window, cx| {
                                if *hovered {
                                    hover.enter_and_notify(ix, row_msg_id.as_deref(), cx);
                                } else {
                                    hover.leave_and_notify(ix, cx);
                                }
                            })
                            .when_some(head_pad, |d, pad| d.pt(pad))
                            .when_some(tail_pad, |d, pad| d.pb(pad))
                            .child(content)
                            .into_any(),
                        None => div()
                            .relative()
                            .w_full()
                            .flex()
                            .justify_center()
                            .when_some(head_pad, |d, pad| d.pt(pad))
                            .when_some(tail_pad, |d, pad| d.pb(pad))
                            .child(content)
                            .into_any(),
                    }
                })
                .w_full()
                .h_full(),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::RowHover;

    #[test]
    fn row_hover_tracks_message_across_rows() {
        let hover = RowHover::default();
        assert!(hover.enter(0, Some("m1")));
        assert!(hover.is_msg_hovered("m1"));
        // Moving to another row of the same message keeps message hover without churn.
        assert!(!hover.enter(1, Some("m1")));
        assert!(hover.is_msg_hovered("m1"));
        // Leaving row 0 after already entering row 1 does not clear row 1's hover.
        assert!(!hover.leave(0));
        assert!(hover.is_msg_hovered("m1"));
        // Entering a different message changes the hovered message.
        assert!(hover.enter(2, Some("m2")));
        assert!(!hover.is_msg_hovered("m1"));
        assert!(hover.is_msg_hovered("m2"));
        // Leaving active row clears hover.
        assert!(hover.leave(2));
        assert!(!hover.is_msg_hovered("m2"));
    }

    #[test]
    fn fold_stays_settled_after_animation_window_ends() {
        use std::cell::RefCell;
        use std::collections::{HashMap, HashSet};

        let t0 = std::time::Instant::now();
        let mut toggles = HashMap::new();
        toggles.insert("m1#reasoning0".to_string(), (1_u64, t0));

        let rc = std::rc::Rc::new(crate::transcript::RowContext {
            tokens: keel_theme::Tokens::dark(),
            syntax: keel_theme::SyntaxPaintMap::dark(),
            reduced: false,
            diagram_menu: None,
            diagram_menu_ix: 0,
            diagram_menu_keyboard: false,
            toggles: std::rc::Rc::new(toggles),
            copied: None,
            copied_at: None,
            on_action: std::rc::Rc::new(|_, _| {}),
            order: 0,
            live_choices: None,
            merges: std::rc::Rc::new(HashMap::new()),
            mr_fetches: std::rc::Rc::new(HashMap::new()),
            armed_runs: std::rc::Rc::new(HashSet::new()),
            run_at: None,
            step_from: None,
            tool_from: std::rc::Rc::new(HashMap::new()),
            muted: false,
            now: t0,
            now_ms: 1_000,
            clock_epoch: t0,
            viewport_h: 800.,
            open_lines: std::rc::Rc::new(HashMap::new()),
            row_heights: std::rc::Rc::new(RefCell::new(HashMap::new())),
            prose_max_w: 720.,
            column_w: 720.,
            bubble_max_w: None,
            probe: None,
            plan_running: false,
            plan_cards: std::rc::Rc::new(HashMap::new()),
            citations: std::rc::Rc::new(HashMap::new()),
            current_citations: None,
            subagent_info: std::rc::Rc::new(HashMap::new()),
            bot_theme: None,
        });

        let props = super::TranscriptListProps {
            list_state: gpui::ListState::new(0, gpui::ListAlignment::Bottom, gpui::px(400.)),
            items: std::rc::Rc::new(Vec::new()),
            items_gen: 1,
            rc,
            column: gpui::px(720.),
            dock_h: 80.,
            loader_row: None,
            loader: None,
        };

        // Mid-animation: is_animating is true and Fold progresses toward open.
        let t_mid = t0 + std::time::Duration::from_millis(120);
        assert!(super::TranscriptListView::is_animating(&props, t_mid, 1_120));
        let mid_rc = super::TranscriptListView::frame_rc(&props, t_mid, 1_120);
        let mid_fold = crate::trace_view::Fold::at(
            true,
            mid_rc
                .toggles
                .get("m1#reasoning0")
                .map(|(_, at)| mid_rc.now.saturating_duration_since(*at).as_secs_f32() * 1000.),
            false,
        );
        assert!(mid_fold.height > 0.1 && mid_fold.height < 1.0);

        // Once the fold window ends, is_animating stops, and frame_rc must keep `now` current
        // rather than rewinding to `props.rc.now` (which would snap the fold shut to t=0).
        let t_end = t0 + std::time::Duration::from_millis(400);
        assert!(!super::TranscriptListView::is_animating(&props, t_end, 1_400));
        let end_rc = super::TranscriptListView::frame_rc(&props, t_end, 1_400);
        let end_fold = crate::trace_view::Fold::at(
            true,
            end_rc
                .toggles
                .get("m1#reasoning0")
                .map(|(_, at)| end_rc.now.saturating_duration_since(*at).as_secs_f32() * 1000.),
            false,
        );
        assert!(end_fold.settled());
        assert!((end_fold.height - 1.0).abs() < 1e-4);
    }
}

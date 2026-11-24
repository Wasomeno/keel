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
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    Context, ElementId, EntityId, InteractiveElement, IntoElement, ListState, ParentElement,
    Pixels, Render, ScrollEvent, StatefulInteractiveElement, Styled, Window, div, list, prelude::*,
    px,
};

use super::main_pane::{InlineLoader, TAIL_ROOM};
use crate::style::{HEADER_HEIGHT, zpx};
use crate::transcript::{RowContext, TranscriptRowItem, TranscriptState, render_row};

/// Tracks which transcript row/message is under the pointer for the `Copy` action.
#[derive(Default)]
pub(super) struct RowHover {
    row: Cell<Option<usize>>,
    msg_id: RefCell<Option<String>>,
}

impl RowHover {
    pub(super) fn clear(&self) {
        self.row.set(None);
        *self.msg_id.borrow_mut() = None;
    }

    pub(super) fn is_msg_hovered(&self, message_id: &str) -> bool {
        self.msg_id.borrow().as_deref() == Some(message_id)
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
}

pub(super) struct TranscriptListProps {
    pub(super) list_state: ListState,
    pub(super) items: Rc<Vec<TranscriptRowItem>>,
    pub(super) rc: Rc<RowContext>,
    pub(super) column: Pixels,
    pub(super) dock_h: f32,
    pub(super) loader_row: Option<usize>,
    pub(super) loader: Option<InlineLoader>,
}

pub(super) struct TranscriptListView {
    pub(super) props: Option<TranscriptListProps>,
    hovered_row: Rc<RowHover>,
    timeline_id: EntityId,
    timeline_scroll_until: Rc<Cell<Option<Instant>>>,
    stick_flag: Rc<Cell<bool>>,
    glide_active: Rc<Cell<bool>>,
    on_wheel_sync: Rc<dyn Fn(Pixels, &mut gpui::App)>,
}

impl TranscriptListView {
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
            timeline_id,
            timeline_scroll_until,
            stick_flag,
            glide_active,
            on_wheel_sync,
        }
    }
}

impl Render for TranscriptListView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(props) = self.props.as_ref() else {
            return div().id("transcript-list-host").size_full();
        };

        let items = props.items.clone();
        let rc = props.rc.clone();
        let column = props.column;
        let dock_h = props.dock_h;
        let loader_row = props.loader_row;
        let loader = props.loader;
        let hover_cell = self.hovered_row.clone();
        let list_weak = cx.entity().downgrade();

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
                list(props.list_state.clone(), move |ix, _, _| {
                    let (row, row_id_key, row_msg_id) = match items.get(ix) {
                        Some(item) => {
                            let loader = loader.filter(|_| loader_row == Some(ix));
                            let has_copy = crate::transcript::has_message_text(&items, ix);
                            let shown = hover_cell.is_msg_hovered(&item.message_id);
                            let prev = ix.checked_sub(1).and_then(|p| items.get(p));
                            let rendered = if loader.is_none() && !has_copy {
                                render_row(ix, item, prev, &rc)
                            } else {
                                div()
                                    .w_full()
                                    .flex()
                                    .flex_col()
                                    .child(render_row(ix, item, prev, &rc))
                                    .when(has_copy, |d| {
                                        d.child(crate::transcript::message_copy(
                                            item,
                                            items.clone(),
                                            ix,
                                            shown,
                                            &rc,
                                        ))
                                    })
                                    .children(loader.map(|loader| loader.render()))
                                    .into_any()
                            };
                            (
                                rendered,
                                Some(item.row_id().to_string()),
                                Some(item.message_id.clone()),
                            )
                        }
                        None => (div().into_any(), None, None),
                    };

                    let hover = hover_cell.clone();
                    let list_weak = list_weak.clone();
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
                                let changed = if *hovered {
                                    hover.enter(ix, row_msg_id.as_deref())
                                } else {
                                    hover.leave(ix)
                                };
                                if changed {
                                    let _ = list_weak.update(cx, |_, cx| cx.notify());
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
}

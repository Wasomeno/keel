//! Docked todo tray attached to the top of the main chat composer.
//!
//! Reads the latest turn's `Plan` (`todowrite` / harness `plan`) or
//! `StepKind::Task` (`task_steps`) items, defaulting to a single collapsed row
//! showing the currently running item with a spinner, and expands smoothly to
//! reveal completed checkmarks and remaining items.

use std::time::{Duration, Instant};

use gpui::{
    AnyElement, Context, FontFeatures, FontWeight, IntoElement, ParentElement, Styled,
    Transformation, Window, div, prelude::*,
};
use keel_proto::ChatStatus;
use keel_theme::Tokens;

use crate::hover_fade::HoverFadeExt;
use crate::icons::Icon;
use crate::steps::StepKind as UiStepKind;
use crate::style::{RADIUS_SM, c, zpx};
use crate::trace::PlanItem;
use crate::trace_view::Fold;
use crate::turn::Step;

use super::{KeelWindow, MsgEntry};

const HEAD_H: f32 = 34.;
const ROW_H: f32 = 28.;
const BODY_PAD_Y: f32 = 4.;
const MAX_VISIBLE_ROWS: usize = 7;
const DISMISS_MS: f32 = 140.;
const SWAP_MS: f32 = 150.;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TodoStatus {
    Pending,
    Running,
    Done,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TodoItem {
    pub label: String,
    pub status: TodoStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ComposerTodo {
    pub source_id: String,
    pub items: Vec<TodoItem>,
}

impl ComposerTodo {
    pub fn done_count(&self) -> usize {
        self.items
            .iter()
            .filter(|i| i.status == TodoStatus::Done)
            .count()
    }

    pub fn active_item(&self) -> Option<(usize, &TodoItem)> {
        self.items
            .iter()
            .enumerate()
            .find(|(_, i)| i.status == TodoStatus::Running)
    }

    pub fn has_running(&self) -> bool {
        self.active_item().is_some()
    }

    pub fn all_done(&self) -> bool {
        !self.items.is_empty() && self.done_count() == self.items.len()
    }

    pub fn first_error(&self) -> Option<&TodoItem> {
        self.items.iter().find(|i| i.status == TodoStatus::Error)
    }

    pub fn first_pending(&self) -> Option<&TodoItem> {
        self.items.iter().find(|i| i.status == TodoStatus::Pending)
    }

    /// Text shown in the single-line header when collapsed.
    pub fn collapsed_label(&self) -> String {
        if let Some((_, item)) = self.active_item() {
            return item.label.clone();
        }
        if self.all_done() {
            let n = self.items.len();
            return if n == 1 {
                "1 task completed".into()
            } else {
                format!("All {n} tasks completed")
            };
        }
        if let Some(err) = self.first_error() {
            return err.label.clone();
        }
        if let Some(pending) = self.first_pending() {
            return pending.label.clone();
        }
        "Tasks".into()
    }
}

#[derive(Debug, Default)]
pub(super) struct TodoTrayState {
    pub expanded: bool,
    pub toggle_at: Option<Instant>,
    pub last_source: Option<String>,
    pub appear_gen: u64,
    pub dismissed_source: Option<String>,
    pub dismiss_at: Option<(String, Instant)>,
    pub last_active: Option<(String, String)>,
    pub swap: Option<(String, Instant)>,
}

impl TodoTrayState {
    pub fn reset(&mut self) {
        self.expanded = false;
        self.toggle_at = None;
        self.last_source = None;
        self.dismissed_source = None;
        self.dismiss_at = None;
        self.last_active = None;
        self.swap = None;
    }

    pub fn toggle(&mut self, now: Instant) {
        self.expanded = !self.expanded;
        self.toggle_at = Some(now);
    }

    pub fn dismiss(&mut self, source_id: String, now: Instant, reduced: bool) {
        self.dismissed_source = Some(source_id.clone());
        if reduced {
            self.dismiss_at = None;
            self.expanded = false;
            self.toggle_at = None;
        } else {
            self.dismiss_at = Some((source_id, now));
        }
    }

    pub fn animating(&self, now: Instant, reduced: bool) -> bool {
        if reduced {
            return false;
        }
        let folding = self
            .toggle_at
            .is_some_and(|at| now.saturating_duration_since(at) < Duration::from_millis(260));
        let dismissing = self
            .dismiss_at
            .as_ref()
            .is_some_and(|(_, at)| now.saturating_duration_since(*at) < Duration::from_millis(180));
        let swapping = self
            .swap
            .as_ref()
            .is_some_and(|(_, at)| now.saturating_duration_since(*at) < Duration::from_millis(180));
        folding || dismissing || swapping
    }
}

fn status_from_plan(status: &str) -> TodoStatus {
    match status {
        "completed" | "done" | "complete" => TodoStatus::Done,
        "in_progress" | "inProgress" | "running" => TodoStatus::Running,
        "failed" | "error" => TodoStatus::Error,
        _ => TodoStatus::Pending,
    }
}

/// Resolves an item's display status against whether the chat turn is still
/// running, so stopping/interrupting a chat immediately stops task loaders.
pub(super) fn effective_status(status: TodoStatus, turn_running: bool) -> TodoStatus {
    match status {
        TodoStatus::Running if !turn_running => TodoStatus::Pending,
        other => other,
    }
}

/// Header status icon is only shown while collapsed; when expanded, `"Tasks"`
/// acts as a section title and each row below carries its own status icon.
pub(super) fn show_header_icon(expanded: bool) -> bool {
    !expanded
}

fn from_plan_entries(source_id: String, entries: &[PlanItem]) -> Option<ComposerTodo> {
    let items: Vec<TodoItem> = entries
        .iter()
        .filter_map(|e| {
            let label = e.content.trim();
            (!label.is_empty()).then(|| TodoItem {
                label: label.to_string(),
                status: status_from_plan(&e.status),
            })
        })
        .collect();
    (!items.is_empty()).then_some(ComposerTodo { source_id, items })
}

/// Extract the latest turn's todo list (`Step::Plan` or `StepKind::Task` `Step::Steps`).
pub(super) fn extract_latest(
    msg_order: &[String],
    messages: &std::collections::HashMap<String, MsgEntry>,
) -> Option<ComposerTodo> {
    for msg_id in msg_order.iter().rev() {
        let Some(entry) = messages.get(msg_id) else {
            continue;
        };
        if entry.role != "assistant" {
            continue;
        }
        for step in entry.steps.iter().rev() {
            match step {
                Step::Plan(entries) => {
                    if let Some(todo) = from_plan_entries(format!("{msg_id}#plan"), entries) {
                        return Some(todo);
                    }
                }
                Step::Steps { id, steps } if steps.kind == UiStepKind::Task => {
                    let done_n = steps.done();
                    let items: Vec<TodoItem> = steps
                        .items
                        .iter()
                        .enumerate()
                        .map(|(ix, label)| {
                            let status = if steps.current == Some(ix) {
                                TodoStatus::Running
                            } else if ix < done_n {
                                TodoStatus::Done
                            } else {
                                TodoStatus::Pending
                            };
                            TodoItem {
                                label: label.clone(),
                                status,
                            }
                        })
                        .collect();
                    if !items.is_empty() {
                        return Some(ComposerTodo {
                            source_id: format!("{msg_id}#ui-{id}"),
                            items,
                        });
                    }
                }
                _ => {}
            }
        }
    }
    None
}

fn tnum<E: Styled>(el: E) -> E {
    el.font_features(FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)])))
}

fn body_natural_height(len: usize) -> f32 {
    let visible = len.min(MAX_VISIBLE_ROWS).max(1) as f32;
    visible * ROW_H + BODY_PAD_Y * 2. + 1.
}

impl KeelWindow {
    fn is_todo_turn_running(&self) -> bool {
        self.active_status() == Some(ChatStatus::Running) || self.dev_live
    }

    /// Sync todo tray lifecycle state when sources appear, advance, or finish dismissing.
    pub(super) fn sync_composer_todo(
        &mut self,
        turn_running: bool,
        now: Instant,
    ) -> Option<ComposerTodo> {
        let todo = extract_latest(&self.msg_order, &self.messages)?;
        if self.todo_state.last_source.as_deref() != Some(todo.source_id.as_str()) {
            self.todo_state.last_source = Some(todo.source_id.clone());
            self.todo_state.appear_gen = self.todo_state.appear_gen.wrapping_add(1);
            self.todo_state.expanded = false;
            self.todo_state.toggle_at = None;
            self.todo_state.dismiss_at = None;
            self.todo_state.last_active = None;
            self.todo_state.swap = None;
        }
        if turn_running
            && todo.has_running()
            && self.todo_state.dismissed_source.as_deref() == Some(todo.source_id.as_str())
        {
            self.todo_state.dismissed_source = None;
            self.todo_state.dismiss_at = None;
        }
        let next_label = todo.collapsed_label();
        match &self.todo_state.last_active {
            Some((src, prev)) if src == &todo.source_id && prev != &next_label => {
                self.todo_state.swap = Some((prev.clone(), now));
                self.todo_state.last_active = Some((todo.source_id.clone(), next_label));
            }
            None => {
                self.todo_state.last_active = Some((todo.source_id.clone(), next_label));
            }
            _ => {}
        }
        if let Some((src, at)) = &self.todo_state.dismiss_at {
            let elapsed_ms = now.saturating_duration_since(*at).as_secs_f32() * 1000.;
            if src != &todo.source_id || elapsed_ms >= DISMISS_MS {
                self.todo_state.dismiss_at = None;
                return None;
            }
        } else if self.todo_state.dismissed_source.as_deref() == Some(todo.source_id.as_str()) {
            return None;
        }
        Some(todo)
    }

    pub(super) fn render_composer_todo(
        &mut self,
        t: Tokens,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<(AnyElement, f32)> {
        let now = Instant::now();
        let reduced = self.reduced();
        let turn_running = self.is_todo_turn_running();
        let todo = self.sync_composer_todo(turn_running, now)?;
        let has_running = turn_running && todo.has_running();
        let can_dismiss = !has_running && !turn_running;

        let dismiss_factor = if let Some((_, at)) = &self.todo_state.dismiss_at {
            let ms = now.saturating_duration_since(*at).as_secs_f32() * 1000.;
            let p = (ms / DISMISS_MS).clamp(0., 1.);
            window.request_animation_frame();
            1. - crate::motion::ease_out(p)
        } else {
            1.
        };

        let toggled_ms = self
            .todo_state
            .toggle_at
            .map(|at| now.saturating_duration_since(at).as_secs_f32() * 1000.);
        let fold = Fold::at(self.todo_state.expanded, toggled_ms, reduced);
        if !fold.settled() {
            window.request_animation_frame();
        }

        let swap_p = self.todo_state.swap.as_ref().and_then(|(prev, at)| {
            let ms = now.saturating_duration_since(*at).as_secs_f32() * 1000.;
            if reduced || ms >= SWAP_MS {
                None
            } else {
                window.request_animation_frame();
                Some((
                    prev.clone(),
                    crate::motion::ease_out_strong((ms / SWAP_MS).clamp(0., 1.)),
                ))
            }
        });
        if swap_p.is_none() {
            self.todo_state.swap = None;
        }

        let head_icon = show_header_icon(self.todo_state.expanded).then(|| {
            if has_running {
                crate::motion::dot_grid(
                    c(t.text_muted),
                    format!("todo-head-dots-{}", todo.source_id),
                    reduced,
                )
            } else if todo.all_done() {
                Icon::Check
                    .el(zpx(12.))
                    .text_color(c(t.success))
                    .into_any_element()
            } else if todo.first_error().is_some() {
                Icon::Alert
                    .el(zpx(12.))
                    .text_color(c(t.danger))
                    .into_any_element()
            } else {
                Icon::ListChecks
                    .el(zpx(12.))
                    .text_color(c(t.text_muted))
                    .into_any_element()
            }
        });

        let current_label = if self.todo_state.expanded {
            "Tasks".to_string()
        } else {
            todo.collapsed_label()
        };
        let label_color = if self.todo_state.expanded || has_running || todo.first_error().is_some()
        {
            c(t.text)
        } else {
            c(t.text_muted)
        };

        let title_el: AnyElement = match swap_p {
            Some((old_label, p)) if !self.todo_state.expanded => div()
                .relative()
                .flex_1()
                .min_w_0()
                .h(zpx(18.))
                .overflow_hidden()
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .truncate()
                        .text_size(zpx(12.5))
                        .line_height(zpx(18.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(label_color)
                        .opacity(1. - p)
                        .top(zpx(-4. * p))
                        .child(old_label),
                )
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .truncate()
                        .text_size(zpx(12.5))
                        .line_height(zpx(18.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(label_color)
                        .opacity(p)
                        .top(zpx(4. * (1. - p)))
                        .child(current_label),
                )
                .into_any_element(),
            _ => div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(zpx(12.5))
                .line_height(zpx(18.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(label_color)
                .child(current_label)
                .into_any_element(),
        };

        let dismiss_btn = can_dismiss.then(|| {
            let source_id = todo.source_id.clone();
            div()
                .id("composer-todo-dismiss")
                .flex_none()
                .size(zpx(20.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(RADIUS_SM)
                .text_color(c(t.text_faint))
                .cursor_pointer()
                .hover(|s| s.text_color(c(t.text)))
                .hover_fade(c(t.hover), None)
                .active(|s| s.opacity(0.75))
                .on_click(cx.listener(move |this, _: &gpui::ClickEvent, _, cx| {
                    cx.stop_propagation();
                    let reduced = this.reduced();
                    this.todo_state
                        .dismiss(source_id.clone(), Instant::now(), reduced);
                    this.kick_motion(cx);
                    cx.notify();
                }))
                .child(Icon::Close.el(zpx(10.5)))
        });

        let header = div()
            .id("composer-todo-header")
            .w_full()
            .h(zpx(HEAD_H))
            .px(zpx(14.))
            .flex()
            .items_center()
            .justify_between()
            .gap(zpx(8.))
            .cursor_pointer()
            .hover_fade(c(t.hover), None)
            .active(|s| s.opacity(0.85))
            .on_click(cx.listener(|this, _: &gpui::ClickEvent, _, cx| {
                this.todo_state.toggle(Instant::now());
                this.kick_motion(cx);
                cx.notify();
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(zpx(8.))
                    .when_some(head_icon, |d, icon| {
                        d.child(
                            div()
                                .flex_none()
                                .size(zpx(16.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(icon),
                        )
                    })
                    .child(title_el),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(zpx(8.))
                    .child(
                        tnum(
                            div()
                                .text_size(zpx(11.5))
                                .text_color(c(t.text_faint)),
                        )
                        .child(format!("{}/{}", todo.done_count(), todo.items.len())),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(c(t.text_faint))
                            .child(Icon::ChevronDown.el(zpx(11.)).with_transformation(
                                Transformation::rotate(gpui::radians(
                                    -std::f32::consts::FRAC_PI_2 * (1. - fold.chevron),
                                )),
                            )),
                    )
                    .children(dismiss_btn),
            );

        let natural_body_h = body_natural_height(todo.items.len());
        let scrollable = todo.items.len() > MAX_VISIBLE_ROWS;
        let body = fold.visible().then(|| {
            let rows = todo.items.iter().enumerate().map(|(ix, item)| {
                let status = effective_status(item.status, turn_running);
                let icon = match status {
                    TodoStatus::Running => crate::motion::dot_grid(
                        c(t.text_muted),
                        format!("todo-row-dots-{}-{ix}", todo.source_id),
                        reduced,
                    ),
                    TodoStatus::Done => Icon::Check
                        .el(zpx(12.))
                        .text_color(c(t.success))
                        .into_any_element(),
                    TodoStatus::Error => Icon::Alert
                        .el(zpx(12.))
                        .text_color(c(t.danger))
                        .into_any_element(),
                    TodoStatus::Pending => Icon::Circle
                        .el(zpx(10.))
                        .text_color(c(t.text_faint).opacity(0.65))
                        .into_any_element(),
                };
                let (text_color, weight) = match status {
                    TodoStatus::Running => (c(t.text), FontWeight::MEDIUM),
                    TodoStatus::Done => (c(t.text_faint), FontWeight::NORMAL),
                    TodoStatus::Error => (c(t.danger), FontWeight::NORMAL),
                    TodoStatus::Pending => (c(t.text_muted), FontWeight::NORMAL),
                };
                div()
                    .w_full()
                    .h(zpx(ROW_H))
                    .px(zpx(14.))
                    .flex()
                    .items_center()
                    .gap(zpx(8.))
                    .when(status == TodoStatus::Running, |d| {
                        d.bg(c(t.accent).opacity(0.04))
                    })
                    .child(
                        div()
                            .flex_none()
                            .size(zpx(16.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(zpx(12.5))
                            .font_weight(weight)
                            .text_color(text_color)
                            .when(item.status == TodoStatus::Done, |d| d.line_through())
                            .child(item.label.clone()),
                    )
            });

            let inner = div()
                .id("composer-todo-list")
                .w_full()
                .py(zpx(BODY_PAD_Y))
                .flex()
                .flex_col()
                .when(scrollable, |d| {
                    d.max_h(zpx(natural_body_h - 1.)).overflow_y_scroll()
                })
                .children(rows);

            div()
                .w_full()
                .border_t_1()
                .border_color(c(t.border).opacity(0.45))
                .overflow_hidden()
                .h(zpx(natural_body_h * fold.height))
                .opacity(fold.opacity)
                .child(inner)
        });

        let expand_h = if fold.visible() {
            natural_body_h * fold.height
        } else {
            0.
        };
        let total_natural_h = HEAD_H + expand_h;
        let tray = div()
            .w_full()
            .rounded_t(zpx(17.))
            .overflow_hidden()
            .bg(c(t.surface).opacity(0.42))
            .border_b_1()
            .border_color(c(t.border).opacity(0.6))
            .when(dismiss_factor < 1., |d| {
                d.h(zpx(total_natural_h * dismiss_factor))
                    .opacity(dismiss_factor)
            })
            .child(header)
            .children(body);

        Some((
            crate::motion::appear(
                tray,
                ("composer-todo-in", self.todo_state.appear_gen as usize),
                crate::motion::BASE,
                4.,
                reduced,
            )
            .into_any_element(),
            expand_h * dismiss_factor,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::MessageBlockState;
    use crate::steps::{StepKind as UiStepKind, Steps};
    use std::collections::HashMap;

    fn assistant_entry(steps: Vec<Step>) -> MsgEntry {
        MsgEntry {
            role: "assistant".into(),
            blocks: MessageBlockState::new("m".into()),
            images: Vec::new(),
            steps,
        }
    }

    #[test]
    fn extracts_latest_plan_or_task_steps_ignoring_guide_steps() {
        let mut messages = HashMap::new();
        messages.insert(
            "m1".into(),
            assistant_entry(vec![Step::Plan(vec![
                PlanItem {
                    content: "Read files".into(),
                    status: "completed".into(),
                    priority: None,
                },
                PlanItem {
                    content: "Implement tray".into(),
                    status: "in_progress".into(),
                    priority: None,
                },
                PlanItem {
                    content: "Run tests".into(),
                    status: "pending".into(),
                    priority: None,
                },
            ])]),
        );
        messages.insert(
            "m2".into(),
            assistant_entry(vec![Step::Steps {
                id: "g1".into(),
                steps: Steps {
                    items: vec!["Step A".into(), "Step B".into()],
                    current: Some(0),
                    kind: UiStepKind::Guide,
                },
            }]),
        );

        let order = vec!["m1".into(), "m2".into()];
        let todo = extract_latest(&order, &messages).expect("todo from m1 plan");
        assert_eq!(todo.source_id, "m1#plan");
        assert_eq!(todo.done_count(), 1);
        assert_eq!(todo.collapsed_label(), "Implement tray");
        assert_eq!(todo.items[0].status, TodoStatus::Done);
        assert_eq!(todo.items[1].status, TodoStatus::Running);
        assert_eq!(todo.items[2].status, TodoStatus::Pending);

        // Task steps in a newer turn supersede earlier plans.
        messages.insert(
            "m3".into(),
            assistant_entry(vec![Step::Steps {
                id: "t1".into(),
                steps: Steps {
                    items: vec!["Inspect".into(), "Edit".into(), "Verify".into()],
                    current: Some(1),
                    kind: UiStepKind::Task,
                },
            }]),
        );
        let order = vec!["m1".into(), "m2".into(), "m3".into()];
        let todo = extract_latest(&order, &messages).expect("todo from m3 task_steps");
        assert_eq!(todo.source_id, "m3#ui-t1");
        assert_eq!(todo.done_count(), 1);
        assert_eq!(todo.collapsed_label(), "Edit");
    }

    #[test]
    fn collapsed_label_handles_all_done_and_default_state_is_collapsed() {
        let state = TodoTrayState::default();
        assert!(!state.expanded);

        let todo = ComposerTodo {
            source_id: "m1#plan".into(),
            items: vec![
                TodoItem {
                    label: "One".into(),
                    status: TodoStatus::Done,
                },
                TodoItem {
                    label: "Two".into(),
                    status: TodoStatus::Done,
                },
            ],
        };
        assert!(todo.all_done());
        assert_eq!(todo.collapsed_label(), "All 2 tasks completed");
    }

    #[test]
    fn stopping_turn_halts_running_status_and_expanding_hides_header_icon() {
        assert_eq!(
            effective_status(TodoStatus::Running, true),
            TodoStatus::Running
        );
        assert_eq!(
            effective_status(TodoStatus::Running, false),
            TodoStatus::Pending
        );
        assert_eq!(effective_status(TodoStatus::Done, false), TodoStatus::Done);
        assert_eq!(
            effective_status(TodoStatus::Error, false),
            TodoStatus::Error
        );

        assert!(show_header_icon(false));
        assert!(!show_header_icon(true));
    }
}

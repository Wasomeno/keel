//! Chat session summary card: an origin-aware, high-density hover popover
//! that gives developers immediate context recovery on past or running sessions.
//!
//! Embodies Emil Kowalski's interaction craft (`cubic-bezier(0.23, 1, 0.32, 1)`,
//! warm-up delay, tactile active press) and anti-slop frontend taste (monochromatic
//! chrome, color strictly on semantic data, SF Mono for tabular git deltas).

use std::time::{Duration, Instant};

use gpui::{
    AnyElement, Context, ElementId, FontWeight, IntoElement, ParentElement, Styled, Window, div,
    point, prelude::*, px,
};
use keel_proto::DisplayStatus;
use keel_theme::Tokens;

use crate::icons::Icon;
use crate::style::{CAPTION, KeelStyled, MONO, RADIUS_LG, RADIUS_SM, SMALL, TITLE, c, zpx};

/// Width of the summary card popover.
pub const SUMMARY_CARD_WIDTH: f32 = 340.;

/// Synthesized summary model for a chat session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatSummary {
    pub chat_id: String,
    pub title: String,
    pub branch: String,
    pub objective: String,
    pub summary: String,
    pub last_handoff: Option<SessionHandoff>,
    pub git: Option<SessionGitDelta>,
    pub touched_files: Vec<String>,
    pub checks: Option<SessionChecks>,
    pub model: Option<String>,
    pub token_usage: Option<u64>,
    pub context_percent: Option<u8>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionHandoff {
    pub kind: HandoffKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffKind {
    Question,
    #[allow(dead_code)]
    Deliverable,
    Error,
    Step,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionGitDelta {
    pub branch: String,
    pub added: i64,
    pub removed: i64,
    pub file_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionChecks {
    pub passed: bool,
    pub summary_label: String,
}

impl ChatSummary {
    /// Create a fallback summary from known chat and diff state.
    pub fn from_chat_state(
        chat: &crate::state::ChatItem,
        diff: Option<&crate::state::DiffItem>,
        status: DisplayStatus,
        model_display: Option<String>,
        now_ms: i64,
    ) -> Self {
        let title = if chat.title.trim().is_empty() {
            "New chat".to_string()
        } else {
            chat.title.clone()
        };

        let git = diff
            .filter(|d| d.additions > 0 || d.deletions > 0 || d.files > 0)
            .map(|d| SessionGitDelta {
                branch: if chat.branch.is_empty() {
                    "HEAD".into()
                } else {
                    chat.branch.clone()
                },
                added: d.additions,
                removed: d.deletions,
                file_count: d.files,
            });

        let summary = match (&git, status) {
            (Some(g), _) => format!(
                "Modified {} files (+{} / −{}) on {}.",
                g.file_count, g.added, g.removed, g.branch
            ),
            (None, DisplayStatus::Running) => "Working on active turn in workspace.".into(),
            (None, DisplayStatus::AwaitingInput) => "Awaiting input from user to continue.".into(),
            (None, DisplayStatus::Aborted) => "Session was stopped.".into(),
            (None, _) => {
                if chat.branch.is_empty() {
                    "Clean working tree. Ready for prompt.".into()
                } else {
                    format!("Clean working tree on {}.", chat.branch)
                }
            }
        };

        let last_handoff = match status {
            DisplayStatus::AwaitingInput => Some(SessionHandoff {
                kind: HandoffKind::Question,
                message: "Awaiting your prompt or confirmation to proceed.".into(),
            }),
            DisplayStatus::Running => Some(SessionHandoff {
                kind: HandoffKind::Step,
                message: "Agent is actively processing tools and commands.".into(),
            }),
            DisplayStatus::Aborted => Some(SessionHandoff {
                kind: HandoffKind::Error,
                message: "Turn was aborted or cancelled.".into(),
            }),
            _ => None,
        };

        let total_tokens = chat.usage.total();
        let token_usage = if total_tokens > 0 {
            Some(total_tokens)
        } else {
            None
        };
        let context_percent = chat
            .usage
            .fill()
            .map(|f| (f * 100.0).round().clamp(0.0, 100.0) as u8);

        let branch = if !chat.branch.is_empty() {
            chat.branch.clone()
        } else {
            String::new()
        };

        Self {
            chat_id: chat.id.clone(),
            title: title.clone(),
            branch,
            objective: title,
            summary,
            last_handoff,
            git,
            touched_files: Vec::new(),
            checks: None,
            model: model_display.or_else(|| (!chat.model.is_empty()).then(|| chat.model.clone())),
            token_usage,
            context_percent,
            updated_at: if chat.last_message_at > 0 {
                chat.last_message_at
            } else {
                now_ms
            },
        }
    }
}

/// Timing constants for chat session summary card interactions and sleek motion.
/// Embodies Emil Kowalski's interaction craft:
/// - 350ms warm-up delay before initial entrance
/// - 500ms warm window for instant adjacent switches
/// - 160ms entrance on `cubic-bezier(0.23, 1, 0.32, 1)` with origin-aware horizontal drift
/// - 120ms exit on `ease_in` (slow start, fast acceleration out) with subtle retraction
pub const SUMMARY_HOVER_DELAY: Duration = Duration::from_millis(350);
pub const SUMMARY_WARM_WINDOW: Duration = Duration::from_millis(500);
pub const SUMMARY_ENTER_MS: f32 = 160.0;
pub const SUMMARY_EXIT_MS: f32 = 120.0;
pub const SUMMARY_SWITCH_MS: f32 = 140.0;

/// Tracks hover and entrance/exit motion for session summary cards.
#[derive(Debug, Default)]
pub struct SessionSummaryHover {
    /// Currently hovered chat row ID, and since when.
    hovered: Option<(String, Instant)>,
    /// Active popover: (chat ID, anchor y, shown at, animate entrance)
    shown: Option<(String, f32, Instant, bool)>,
    /// Transition between sessions: (from_chat_id, from_y, to_y, switched_at)
    transition: Option<(String, f32, f32, Instant)>,
    /// Leaving popover: (chat ID, anchor y, started leaving at)
    leaving: Option<(String, f32, Instant)>,
    /// When the last popover hid (for warm-up calculation).
    hidden_at: Option<Instant>,
    /// Clicked chat: suppressed until pointer leaves.
    suppressed: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SummaryMotionLook {
    pub chat_id: String,
    pub anchor_y: f32,
    pub opacity: f32,
    pub dx: f32,
    pub dy: f32,
    /// Content transition progress (0.0 just switched, 1.0 settled), and vertical parallax offset.
    pub content_opacity: f32,
    pub content_dy: f32,
    pub moving: bool,
}

impl SessionSummaryHover {
    /// Pointer entered (`on == true`) or left (`on == false`) chat `chat_id`.
    /// Returns `Some(delay)` if cold delay timer should be started.
    pub fn hover(
        &mut self,
        chat_id: String,
        anchor_y: f32,
        on: bool,
        now: Instant,
    ) -> Option<Duration> {
        if !on {
            if self.suppressed.as_deref() == Some(&chat_id) {
                self.suppressed = None;
            }
            if self.hovered.as_ref().is_some_and(|(h, _)| h == &chat_id) {
                self.hovered = None;
            }
            if let Some((s, cur_anchor_y, _, _)) = &self.shown {
                if s == &chat_id {
                    let exit_y = if let Some(tr) = &self.transition {
                        let elapsed = now.saturating_duration_since(tr.3).as_secs_f32() * 1000.0
                            / SUMMARY_SWITCH_MS;
                        let p = crate::motion::ease_out_strong(elapsed.clamp(0.0, 1.0));
                        tr.1 + (tr.2 - tr.1) * p
                    } else {
                        *cur_anchor_y
                    };
                    self.transition = None;
                    self.shown = None;
                    self.leaving = Some((chat_id, exit_y, now));
                    self.hidden_at = Some(now);
                }
            }
            return None;
        }

        self.hovered = Some((chat_id.clone(), now));
        if self.suppressed.as_deref() == Some(&chat_id)
            || self
                .shown
                .as_ref()
                .is_some_and(|(s, _, _, _)| s == &chat_id)
        {
            return None;
        }

        if self.warm(now) {
            // Warm switch: sleek transition across adjacent chat sessions
            let (from_id, current_y) = if let Some(tr) = &self.transition {
                let elapsed =
                    now.saturating_duration_since(tr.3).as_secs_f32() * 1000.0 / SUMMARY_SWITCH_MS;
                let p = crate::motion::ease_out_strong(elapsed.clamp(0.0, 1.0));
                let cur = tr.1 + (tr.2 - tr.1) * p;
                (tr.0.clone(), cur)
            } else if let Some((active_id, active_y, _, _)) = &self.shown {
                (active_id.clone(), *active_y)
            } else if let Some((leave_id, leave_y, _)) = &self.leaving {
                (leave_id.clone(), *leave_y)
            } else {
                (chat_id.clone(), anchor_y)
            };

            self.leaving = None;
            if from_id != chat_id {
                self.transition = Some((from_id, current_y, anchor_y, now));
            }
            self.shown = Some((chat_id, anchor_y, now, false));
            return None;
        }

        Some(SUMMARY_HOVER_DELAY)
    }

    fn warm(&self, now: Instant) -> bool {
        self.shown.is_some()
            || self
                .hidden_at
                .is_some_and(|at| now.saturating_duration_since(at) < SUMMARY_WARM_WINDOW)
    }

    /// The hover delay that began at `since` ran out: show if pointer is still on that chat.
    pub fn ripen(&mut self, chat_id: &str, since: Instant, anchor_y: f32, now: Instant) -> bool {
        if self.hovered != Some((chat_id.to_string(), since))
            || self.shown.is_some()
            || self.suppressed.as_deref() == Some(chat_id)
            || now.saturating_duration_since(since) < SUMMARY_HOVER_DELAY
        {
            return false;
        }
        if self.leaving.as_ref().is_some_and(|(l, _, _)| l == chat_id) {
            self.leaving = None;
        }
        self.shown = Some((chat_id.to_string(), anchor_y, now, true));
        true
    }

    /// Click on a chat row: fade out immediately and suppress until pointer leaves.
    pub fn press(&mut self, chat_id: &str, now: Instant) {
        self.suppressed = Some(chat_id.to_string());
        if let Some((s, target_anchor_y, _, _)) = self.shown.take() {
            let current_y = if let Some(tr) = self.transition.take() {
                let elapsed =
                    now.saturating_duration_since(tr.3).as_secs_f32() * 1000.0 / SUMMARY_SWITCH_MS;
                let p = crate::motion::ease_out_strong(elapsed.clamp(0.0, 1.0));
                tr.1 + (tr.2 - tr.1) * p
            } else {
                target_anchor_y
            };
            self.leaving = Some((s, current_y, now));
            self.hidden_at = None;
        }
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Which summary to draw now and its look: (chat_id, anchor_y, opacity, dx, dy, content_opacity, content_dy, moving).
    pub fn phase(&mut self, now: Instant, reduced: bool) -> Option<SummaryMotionLook> {
        if let Some((chat_id, anchor_y, at)) = &self.leaving {
            let x = now.saturating_duration_since(*at).as_secs_f32() * 1000.0 / SUMMARY_EXIT_MS;
            if reduced || x >= 1.0 {
                self.leaving = None;
                return None;
            }
            // Emil's ease-in: slow start, fast acceleration out
            let p = crate::motion::ease_in(x.clamp(0.0, 1.0));
            return Some(SummaryMotionLook {
                chat_id: chat_id.clone(),
                anchor_y: *anchor_y,
                opacity: (1.0 - p).max(0.0),
                dx: -6.0 * p,
                dy: 0.0,
                content_opacity: 1.0,
                content_dy: 0.0,
                moving: true,
            });
        }

        let (chat_id, target_anchor_y, at, animate_entrance) = self.shown.as_ref()?;

        let (shell_opacity, shell_dx, shell_dy, shell_moving) = if *animate_entrance {
            let x = now.saturating_duration_since(*at).as_secs_f32() * 1000.0 / SUMMARY_ENTER_MS;
            if reduced || x >= 1.0 {
                (1.0, 0.0, 0.0, false)
            } else {
                let p = crate::motion::ease_out_strong(x.clamp(0.0, 1.0));
                (p, -8.0 * (1.0 - p), -3.0 * (1.0 - p), true)
            }
        } else {
            (1.0, 0.0, 0.0, false)
        };

        let (current_anchor_y, content_opacity, content_dy, trans_moving) = if let Some(tr) =
            &self.transition
        {
            let x = now.saturating_duration_since(tr.3).as_secs_f32() * 1000.0 / SUMMARY_SWITCH_MS;
            if reduced || x >= 1.0 {
                self.transition = None;
                (*target_anchor_y, 1.0, 0.0, false)
            } else {
                let p = crate::motion::ease_out_strong(x.clamp(0.0, 1.0));
                let cur_y = tr.1 + (tr.2 - tr.1) * p;
                let direction = if tr.2 >= tr.1 { 1.0 } else { -1.0 };
                let dy = -4.0 * direction * (1.0 - p);
                let opacity = (p / 0.75).min(1.0);
                (cur_y, opacity, dy, true)
            }
        } else {
            (*target_anchor_y, 1.0, 0.0, false)
        };

        Some(SummaryMotionLook {
            chat_id: chat_id.clone(),
            anchor_y: current_anchor_y,
            opacity: shell_opacity,
            dx: shell_dx,
            dy: shell_dy,
            content_opacity,
            content_dy,
            moving: shell_moving || trans_moving,
        })
    }
}

fn time_ago_str(updated_at: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let diff_s = (now - updated_at).max(0) / 1000;
    match diff_s {
        s if s < 60 => "just now".into(),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86_400),
    }
}

fn render_header(summary: &ChatSummary, status: DisplayStatus, t: Tokens) -> impl IntoElement {
    let (status_text, dot_color, bg_tint) = match status {
        DisplayStatus::AwaitingInput => ("Awaiting Input", t.danger, t.danger),
        DisplayStatus::Running => ("Running", t.info, t.info),
        DisplayStatus::Idle => ("Completed", t.success, t.success),
        DisplayStatus::Aborted | DisplayStatus::Unknown => ("Stopped", t.text_faint, t.text_faint),
    };

    let branch = summary
        .git
        .as_ref()
        .map(|g| g.branch.clone())
        .or_else(|| (!summary.branch.is_empty()).then(|| summary.branch.clone()))
        .unwrap_or_else(|| "no repo".into());
    let time_ago = time_ago_str(summary.updated_at);

    div()
        .flex()
        .flex_row()
        .justify_between()
        .items_center()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(5.))
                .px(zpx(7.))
                .py(zpx(2.))
                .rounded_full()
                .bg(c(bg_tint).opacity(0.12))
                .child(div().size(zpx(6.)).rounded_full().bg(c(dot_color)))
                .child(
                    div()
                        .type_step(CAPTION)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(c(dot_color))
                        .child(status_text),
                ),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(6.))
                .font_family(MONO)
                .type_step(CAPTION)
                .text_color(c(t.text_faint))
                .child(
                    div()
                        .max_w(zpx(120.))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(branch),
                )
                .child(div().child("·"))
                .child(div().child(time_ago)),
        )
}

fn render_objective(summary: &ChatSummary, t: Tokens) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(zpx(3.))
        .child(
            div()
                .type_step(TITLE)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(c(t.text))
                .line_height(zpx(18.))
                .max_h(zpx(36.))
                .overflow_hidden()
                .text_ellipsis()
                .child(summary.objective.clone()),
        )
        .child(
            div()
                .type_step(SMALL)
                .text_color(c(t.text_muted))
                .line_height(zpx(16.))
                .max_h(zpx(48.))
                .overflow_hidden()
                .text_ellipsis()
                .child(summary.summary.clone()),
        )
}

fn render_handoff(handoff: &SessionHandoff, t: Tokens) -> impl IntoElement {
    let (icon, label, accent_color) = match handoff.kind {
        HandoffKind::Question => (Icon::Comment, "Needs your input", t.warning),
        HandoffKind::Deliverable => (Icon::Check, "Outcome", t.success),
        HandoffKind::Error => (Icon::Alert, "Turn stopped", t.danger),
        HandoffKind::Step => (Icon::Tool, "In progress", t.accent),
    };

    let message = handoff.message.trim().trim_matches('"').trim();

    div()
        .bg(c(t.window))
        .border_1()
        .border_color(c(t.border).opacity(0.6))
        .rounded(RADIUS_SM)
        .px(zpx(10.))
        .py(zpx(7.))
        .flex()
        .flex_col()
        .gap(zpx(2.))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(5.))
                .child(
                    div()
                        .flex_none()
                        .text_color(c(accent_color))
                        .child(icon.el(zpx(12.))),
                )
                .child(
                    div()
                        .type_step(CAPTION)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(c(t.text))
                        .child(label),
                ),
        )
        .child(
            div()
                .pl(zpx(17.))
                .type_step(SMALL)
                .text_color(c(t.text_muted))
                .line_height(zpx(15.))
                .max_h(zpx(45.))
                .overflow_hidden()
                .text_ellipsis()
                .child(message.to_string()),
        )
}

fn render_git_impact(
    git: &SessionGitDelta,
    touched_files: &[String],
    t: Tokens,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(zpx(4.))
        .child(
            div()
                .flex()
                .flex_row()
                .justify_between()
                .items_baseline()
                .child(
                    div()
                        .type_step(CAPTION)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(c(t.text_faint))
                        .child("CODE IMPACT"),
                )
                .child(
                    div()
                        .font_family(MONO)
                        .type_step(CAPTION)
                        .flex()
                        .flex_row()
                        .gap(zpx(5.))
                        .child(
                            div()
                                .text_color(c(t.success))
                                .child(format!("+{}", git.added)),
                        )
                        .child(
                            div()
                                .text_color(c(t.danger))
                                .child(format!("−{}", git.removed)),
                        )
                        .child(
                            div()
                                .text_color(c(t.text_faint))
                                .child(format!("in {} files", git.file_count)),
                        ),
                ),
        )
        .when(!touched_files.is_empty(), |el| {
            el.child(
                div()
                    .bg(c(t.window))
                    .border_1()
                    .border_color(c(t.border).opacity(0.6))
                    .rounded(RADIUS_SM)
                    .px(zpx(7.))
                    .py(zpx(4.))
                    .flex()
                    .flex_col()
                    .gap(zpx(2.))
                    .children(touched_files.iter().take(3).map(|f| {
                        div()
                            .font_family(MONO)
                            .type_step(CAPTION)
                            .text_color(c(t.text_muted))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(f.clone())
                    })),
            )
        })
}

fn render_checks(checks: &SessionChecks, t: Tokens) -> impl IntoElement {
    let (icon, color) = if checks.passed {
        ("✓", t.success)
    } else {
        ("✗", t.danger)
    };

    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(zpx(4.))
        .font_family(MONO)
        .type_step(CAPTION)
        .text_color(c(t.text_muted))
        .child(
            div()
                .font_weight(FontWeight::BOLD)
                .text_color(c(color))
                .child(icon),
        )
        .child(div().child(checks.summary_label.clone()))
}

fn render_footer(summary: &ChatSummary, t: Tokens) -> impl IntoElement {
    let model_str = summary
        .model
        .clone()
        .unwrap_or_else(|| "Default agent".into());
    let token_str = summary.token_usage.map(crate::usage::compact);
    let pct_str = summary.context_percent.map(|p| format!("({p}%)"));

    div()
        .pt(zpx(6.))
        .border_t_1()
        .border_color(c(t.border))
        .flex()
        .flex_row()
        .justify_between()
        .items_center()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(6.))
                .font_family(MONO)
                .type_step(CAPTION)
                .text_color(c(t.text_faint))
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(c(t.text))
                        .max_w(zpx(120.))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(model_str),
                )
                .children(token_str.map(|tok| {
                    div().child(match pct_str {
                        Some(pct) => format!("{tok} {pct}"),
                        None => tok,
                    })
                })),
        )
        .child(
            div()
                .type_step(CAPTION)
                .text_color(c(t.text_faint))
                .child("Click chat to open ↵"),
        )
}

impl super::KeelWindow {
    /// Update or synthesize a summary for a chat when its turn ends.
    pub(super) fn record_chat_summary_on_turn_end(&mut self, chat_id: &str) {
        let Some(chat) = self.state.chats.get(chat_id).cloned() else {
            return;
        };
        let diff = (!chat.checkout_id.is_empty())
            .then(|| self.state.diffs.get(&chat.checkout_id))
            .flatten()
            .cloned();

        let model_display = self.model_name(&chat.harness, &chat.model);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        // Find last user prompt and last assistant conclusion if this is the current chat
        let (objective, summary_text, touched_files, checks, last_handoff) =
            if self.current_chat.as_deref() == Some(chat_id) {
                let last_user_msg = self
                    .msg_order
                    .iter()
                    .rev()
                    .find(|id| self.messages.get(*id).is_some_and(|m| m.role == "user"))
                    .and_then(|id| self.messages.get(id))
                    .map(|m| m.blocks.full_text.trim().to_string())
                    .filter(|s| !s.is_empty());

                let last_assistant = self
                    .msg_order
                    .iter()
                    .rev()
                    .find(|id| {
                        self.messages
                            .get(*id)
                            .is_some_and(|m| m.role == "assistant")
                    })
                    .and_then(|id| self.messages.get(id));

                let mut files: Vec<String> = Vec::new();
                let mut checks_label: Option<String> = None;
                let mut conclusion: Option<String> = None;

                if let Some(asst) = last_assistant {
                    conclusion =
                        Some(asst.blocks.full_text.trim().to_string()).filter(|s| !s.is_empty());

                    for step in &asst.steps {
                        if let crate::turn::Step::Tools(tools) = step {
                            for tool in tools {
                                if let Some(diff) = &tool.diff {
                                    if !files.contains(&diff.path) {
                                        files.push(diff.path.clone());
                                    }
                                }
                                if tool.name.contains("test")
                                    || tool.name.contains("check")
                                    || tool.summary.contains("passed")
                                {
                                    if checks_label.is_none() {
                                        checks_label = Some(if tool.state == "error" {
                                            "checks failed".into()
                                        } else {
                                            "tests passed".into()
                                        });
                                    }
                                }
                            }
                        }
                    }
                }

                let obj = last_user_msg.unwrap_or_else(|| {
                    if chat.title.trim().is_empty() {
                        "New chat".into()
                    } else {
                        chat.title.clone()
                    }
                });

                // Extract first sentence or up to 100 chars for objective
                let clean_obj = if let Some(dot_idx) = obj.find(". ") {
                    obj[..dot_idx].to_string()
                } else if obj.chars().count() > 100 {
                    format!("{}…", obj.chars().take(99).collect::<String>())
                } else {
                    obj
                };

                let sum = conclusion.unwrap_or_else(|| {
                    if let Some(d) = &diff {
                        format!(
                            "Modified {} files (+{} / −{}) on {}.",
                            d.files, d.additions, d.deletions, chat.branch
                        )
                    } else {
                        format!("Turn completed cleanly on {}.", chat.branch)
                    }
                });

                let handoff = if let Some(panel) = self.question_panels.get(chat_id) {
                    panel.current_question().map(|q| SessionHandoff {
                        kind: HandoffKind::Question,
                        message: q.prompt.clone(),
                    })
                } else if chat.status == keel_proto::ChatStatus::AwaitingInput {
                    Some(SessionHandoff {
                        kind: HandoffKind::Question,
                        message: "Awaiting user confirmation or response.".into(),
                    })
                } else if chat.status == keel_proto::ChatStatus::Aborted {
                    Some(SessionHandoff {
                        kind: HandoffKind::Error,
                        message: "Turn was aborted or stopped.".into(),
                    })
                } else if chat.status == keel_proto::ChatStatus::Running {
                    Some(SessionHandoff {
                        kind: HandoffKind::Step,
                        message: "Agent is actively processing tools and commands.".into(),
                    })
                } else {
                    None
                };

                (
                    clean_obj,
                    sum,
                    files,
                    checks_label.map(|lbl| SessionChecks {
                        passed: !lbl.contains("failed"),
                        summary_label: lbl,
                    }),
                    handoff,
                )
            } else {
                let status = match chat.status {
                    keel_proto::ChatStatus::Idle => DisplayStatus::Idle,
                    keel_proto::ChatStatus::Running => DisplayStatus::Running,
                    keel_proto::ChatStatus::AwaitingInput => DisplayStatus::AwaitingInput,
                    keel_proto::ChatStatus::Aborted => DisplayStatus::Aborted,
                };
                let fallback = ChatSummary::from_chat_state(
                    &chat,
                    diff.as_ref(),
                    status,
                    model_display.clone(),
                    now_ms,
                );
                (
                    fallback.objective,
                    fallback.summary,
                    fallback.touched_files,
                    fallback.checks,
                    fallback.last_handoff,
                )
            };

        let git = diff
            .filter(|d| d.additions > 0 || d.deletions > 0 || d.files > 0)
            .map(|d| SessionGitDelta {
                branch: if chat.branch.is_empty() {
                    "HEAD".into()
                } else {
                    chat.branch.clone()
                },
                added: d.additions,
                removed: d.deletions,
                file_count: d.files,
            });

        let total_tokens = chat.usage.total();
        let token_usage = if total_tokens > 0 {
            Some(total_tokens)
        } else {
            None
        };
        let context_percent = chat
            .usage
            .fill()
            .map(|f| (f * 100.0).round().clamp(0.0, 100.0) as u8);

        let summary = ChatSummary {
            chat_id: chat_id.to_string(),
            title: chat.title.clone(),
            branch: chat.branch.clone(),
            objective,
            summary: summary_text,
            last_handoff,
            git,
            touched_files,
            checks,
            model: model_display.or_else(|| (!chat.model.is_empty()).then(|| chat.model.clone())),
            token_usage,
            context_percent,
            updated_at: if chat.last_message_at > 0 {
                chat.last_message_at
            } else {
                now_ms
            },
        };

        self.chat_summaries.insert(chat_id.to_string(), summary);
    }

    /// Construct a ChatSummary model for a chat row hover tooltip.
    pub(super) fn chat_summary(&self, chat_id: &str, status: DisplayStatus) -> ChatSummary {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        if let Some(summary) = self.chat_summaries.get(chat_id) {
            let mut s = summary.clone();
            if let Some(chat) = self.state.chats.get(chat_id) {
                if let Some(diff) = (!chat.checkout_id.is_empty())
                    .then(|| self.state.diffs.get(&chat.checkout_id))
                    .flatten()
                {
                    if diff.additions > 0 || diff.deletions > 0 || diff.files > 0 {
                        s.git = Some(SessionGitDelta {
                            branch: if chat.branch.is_empty() {
                                "HEAD".into()
                            } else {
                                chat.branch.clone()
                            },
                            added: diff.additions,
                            removed: diff.deletions,
                            file_count: diff.files,
                        });
                    }
                }
            }
            s
        } else if let Some(chat) = self.state.chats.get(chat_id) {
            let diff = (!chat.checkout_id.is_empty())
                .then(|| self.state.diffs.get(&chat.checkout_id))
                .flatten();
            let model = self.model_name(&chat.harness, &chat.model);
            ChatSummary::from_chat_state(chat, diff, status, model, now_ms)
        } else {
            ChatSummary {
                chat_id: chat_id.to_string(),
                title: "Chat".to_string(),
                branch: String::new(),
                objective: "Chat session".to_string(),
                summary: "No session history recorded yet.".to_string(),
                last_handoff: None,
                git: None,
                touched_files: Vec::new(),
                checks: None,
                model: None,
                token_usage: None,
                context_percent: None,
                updated_at: now_ms,
            }
        }
    }

    pub(super) fn chat_summary_hover(
        &mut self,
        chat_id: String,
        anchor_y: f32,
        hovered: bool,
        cx: &mut Context<Self>,
    ) {
        let now = Instant::now();
        if let Some(delay) = self
            .summary_hover
            .hover(chat_id.clone(), anchor_y, hovered, now)
        {
            let id = chat_id.clone();
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(delay).await;
                let _ = this.update(cx, |this, cx| {
                    if this.summary_hover.ripen(&id, now, anchor_y, Instant::now()) {
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        cx.notify();
    }

    pub(super) fn chat_summary_press(&mut self, chat_id: &str, cx: &mut Context<Self>) {
        self.summary_hover.press(chat_id, Instant::now());
        self.projects.folder_card = None;
        cx.notify();
    }

    pub(super) fn render_session_summary_card(
        &mut self,
        t: Tokens,
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let reduced = self.reduced();
        let now = Instant::now();
        let look = self.summary_hover.phase(now, reduced)?;
        if look.moving {
            window.request_animation_frame();
        }

        let chat = self.state.chats.get(&look.chat_id)?;
        let status = match chat.status {
            keel_proto::ChatStatus::Idle => DisplayStatus::Idle,
            keel_proto::ChatStatus::Running => DisplayStatus::Running,
            keel_proto::ChatStatus::AwaitingInput => DisplayStatus::AwaitingInput,
            keel_proto::ChatStatus::Aborted => DisplayStatus::Aborted,
        };
        let summary = self.chat_summary(&look.chat_id, status);
        let sidebar_width = self.state.sidebar_width();

        let target_x = sidebar_width + 8.0 + look.dx;
        let target_y = (look.anchor_y - 20.0).max(8.0) + look.dy;

        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(point(px(target_x), px(target_y)))
                    .snap_to_window_with_margin(zpx(8.))
                    .child(
                        div()
                            .id(ElementId::Name(
                                format!("session-summary-popover-{}", summary.chat_id).into(),
                            ))
                            .w(zpx(SUMMARY_CARD_WIDTH))
                            .bg(c(t.floating))
                            .border_1()
                            .border_color(c(t.border))
                            .rounded(RADIUS_LG)
                            .shadow(vec![
                                gpui::BoxShadow {
                                    color: c(t.shadow),
                                    offset: point(px(0.), zpx(8.)),
                                    blur_radius: zpx(24.),
                                    spread_radius: zpx(-4.),
                                    inset: false,
                                },
                                gpui::BoxShadow {
                                    color: c(t.border),
                                    offset: point(px(0.), px(0.)),
                                    blur_radius: px(1.),
                                    spread_radius: px(0.),
                                    inset: false,
                                },
                            ])
                            .p(zpx(12.))
                            .overflow_hidden()
                            .flex()
                            .flex_col()
                            .opacity(look.opacity)
                            .child(
                                div()
                                    .relative()
                                    .flex()
                                    .flex_col()
                                    .gap(zpx(10.))
                                    .opacity(look.content_opacity)
                                    .mt(zpx(look.content_dy))
                                    // 1. Header: Status badge, branch, recency
                                    .child(render_header(&summary, status, t))
                                    // 2. Objective & Synthesis
                                    .child(render_objective(&summary, t))
                                    // 3. Next Action / Handoff Box (if any)
                                    .children(
                                        summary.last_handoff.as_ref().map(|h| render_handoff(h, t)),
                                    )
                                    // 4. Code Impact / Git Delta (if any)
                                    .children(
                                        summary.git.as_ref().map(|g| {
                                            render_git_impact(g, &summary.touched_files, t)
                                        }),
                                    )
                                    // 5. Automated Checks (if any)
                                    .children(summary.checks.as_ref().map(|c| render_checks(c, t)))
                                    // 6. Footer: Model, token usage, hint
                                    .child(render_footer(&summary, t)),
                            ),
                    ),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ChatItem, DiffItem};
    use crate::usage::{ChatUsage, ModelTokens};
    use keel_proto::ChatStatus;

    #[test]
    fn chat_summary_fallback_generation() {
        let chat = ChatItem {
            id: "chat-1".into(),
            space_id: "space-1".into(),
            title: "Yooo".into(),
            status: ChatStatus::AwaitingInput,
            last_message_at: 1000,
            last_seen_at: 1000,
            archived: false,
            tombstoned: false,
            branch: "develop".into(),
            checkout_id: "chk-1".into(),
            harness: "grok".into(),
            model: "grok-4.5".into(),
            reasoning: "".into(),
            reasoning_known: false,
            permission: "".into(),
            usage: ChatUsage {
                models: vec![ModelTokens {
                    model: "grok-4.5".into(),
                    input: 2000,
                    output: 500,
                    cache_read: 0,
                    cache_write: 0,
                }],
                context_used: 2500,
                context_window: 10000,
            },
        };

        let diff = DiffItem {
            checkout_id: "chk-1".into(),
            additions: 84,
            deletions: 22,
            files: 2,
        };

        let summary = ChatSummary::from_chat_state(
            &chat,
            Some(&diff),
            DisplayStatus::AwaitingInput,
            Some("Grok 4.5".into()),
            2000,
        );

        assert_eq!(summary.chat_id, "chat-1");
        assert_eq!(summary.objective, "Yooo");
        assert_eq!(summary.model.as_deref(), Some("Grok 4.5"));
        assert_eq!(summary.context_percent, Some(25));
        let git = summary.git.expect("git delta present");
        assert_eq!(git.branch, "develop");
        assert_eq!(git.added, 84);
        assert_eq!(git.removed, 22);
        assert_eq!(git.file_count, 2);
        let handoff = summary.last_handoff.expect("handoff present");
        assert_eq!(handoff.kind, HandoffKind::Question);
    }

    #[test]
    fn chat_summary_clean_working_tree() {
        let chat = ChatItem {
            id: "chat-2".into(),
            space_id: "space-1".into(),
            title: "".into(),
            status: ChatStatus::Idle,
            last_message_at: 0,
            last_seen_at: 0,
            archived: false,
            tombstoned: false,
            branch: "main".into(),
            checkout_id: "".into(),
            harness: "".into(),
            model: "".into(),
            reasoning: "".into(),
            reasoning_known: false,
            permission: "".into(),
            usage: Default::default(),
        };

        let summary = ChatSummary::from_chat_state(&chat, None, DisplayStatus::Idle, None, 5000);

        assert_eq!(summary.title, "New chat");
        assert_eq!(summary.objective, "New chat");
        assert_eq!(summary.branch, "main");
        assert!(summary.summary.contains("Clean working tree on main"));
        assert!(summary.git.is_none());
        assert!(summary.last_handoff.is_none());
    }

    #[test]
    fn time_ago_formatting() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

        assert_eq!(time_ago_str(now), "just now");
        assert_eq!(time_ago_str(now - 120_000), "2m ago");
        assert_eq!(time_ago_str(now - 7_200_000), "2h ago");
        assert_eq!(time_ago_str(now - 172_800_000), "2d ago");
    }

    #[test]
    fn hover_delay_and_ripen_entrance() {
        let mut hover = SessionSummaryHover::default();
        let t0 = Instant::now();

        // 1. Cold hover asks for delay
        let delay = hover.hover("chat-1".into(), 100.0, true, t0);
        assert_eq!(delay, Some(SUMMARY_HOVER_DELAY));

        // 2. Before delay expires, not ripe
        assert!(!hover.ripen("chat-1", t0, 100.0, t0 + Duration::from_millis(100)));

        // 3. Delay expires: ripe!
        let t_ripe = t0 + SUMMARY_HOVER_DELAY;
        assert!(hover.ripen("chat-1", t0, 100.0, t_ripe));

        // 4. At start of entrance: opacity 0, dx -8px, dy -3px
        let look0 = hover.phase(t_ripe, false).expect("look present");
        assert_eq!(look0.chat_id, "chat-1");
        assert_eq!(look0.opacity, 0.0);
        assert_eq!(look0.dx, -8.0);
        assert_eq!(look0.dy, -3.0);
        assert!(look0.moving);

        // 5. Midway: moving, opacity increasing
        let look_mid = hover
            .phase(t_ripe + Duration::from_millis(80), false)
            .expect("look present");
        assert!(look_mid.opacity > 0.0 && look_mid.opacity < 1.0);
        assert!(look_mid.dx > -8.0 && look_mid.dx < 0.0);
        assert!(look_mid.moving);

        // 6. Finished entrance: opacity 1, dx 0, dy 0, settled
        let look_end = hover
            .phase(t_ripe + Duration::from_millis(170), false)
            .expect("look present");
        assert_eq!(look_end.opacity, 1.0);
        assert_eq!(look_end.dx, 0.0);
        assert_eq!(look_end.dy, 0.0);
        assert!(!look_end.moving);
    }

    #[test]
    fn warm_hover_content_transition_and_rail_glide() {
        let mut hover = SessionSummaryHover::default();
        let t0 = Instant::now();

        // 1. Show chat-1 at y = 100.0
        hover.hover("chat-1".into(), 100.0, true, t0);
        hover.ripen("chat-1", t0, 100.0, t0 + SUMMARY_HOVER_DELAY);

        // Settle entrance
        hover.phase(t0 + SUMMARY_HOVER_DELAY + Duration::from_millis(200), false);

        // 2. Switch to chat-2 at y = 150.0 within warm window
        let t_switch = t0 + SUMMARY_HOVER_DELAY + Duration::from_millis(250);
        let delay = hover.hover("chat-2".into(), 150.0, true, t_switch);
        assert_eq!(delay, None);

        // 3. At start of transition:
        let look0 = hover.phase(t_switch, false).expect("chat-2 active");
        assert_eq!(look0.chat_id, "chat-2");
        assert_eq!(look0.anchor_y, 100.0); // starts from previous y
        assert_eq!(look0.content_opacity, 0.0);
        assert_eq!(look0.content_dy, -4.0); // parallax nudge opposite to downward travel
        assert!(look0.moving);

        // 4. Midway (70ms into 140ms switch):
        let look_mid = hover
            .phase(t_switch + Duration::from_millis(70), false)
            .expect("chat-2 mid");
        assert!(look_mid.anchor_y > 100.0 && look_mid.anchor_y < 150.0);
        assert!(look_mid.content_opacity > 0.0 && look_mid.content_opacity <= 1.0);
        assert!(look_mid.content_dy > -4.0 && look_mid.content_dy <= 0.0);
        assert!(look_mid.moving);

        // 5. Settled at 150ms:
        let look_end = hover
            .phase(t_switch + Duration::from_millis(150), false)
            .expect("chat-2 settled");
        assert_eq!(look_end.anchor_y, 150.0);
        assert_eq!(look_end.content_opacity, 1.0);
        assert_eq!(look_end.content_dy, 0.0);
        assert!(!look_end.moving);
    }

    #[test]
    fn interruptible_rapid_session_switch() {
        let mut hover = SessionSummaryHover::default();
        let t0 = Instant::now();

        hover.hover("chat-1".into(), 100.0, true, t0);
        hover.ripen("chat-1", t0, 100.0, t0 + SUMMARY_HOVER_DELAY);
        let t_ripe = t0 + SUMMARY_HOVER_DELAY + Duration::from_millis(200);

        // Switch to chat-2 at y = 200.0
        hover.hover("chat-2".into(), 200.0, true, t_ripe);

        // 50ms in, rapidly switch to chat-3 at y = 50.0
        let t_rapid = t_ripe + Duration::from_millis(50);
        hover.hover("chat-3".into(), 50.0, true, t_rapid);

        // The new transition starts smoothly from intermediate y, not 100 or 200
        let look = hover.phase(t_rapid, false).expect("chat-3 active");
        assert_eq!(look.chat_id, "chat-3");
        assert!(look.anchor_y > 100.0 && look.anchor_y < 200.0);
        assert!(look.moving);
    }

    #[test]
    fn exit_motion_retraction_and_fade() {
        let mut hover = SessionSummaryHover::default();
        let t0 = Instant::now();

        // Show chat-1
        hover.hover("chat-1".into(), 100.0, true, t0);
        hover.ripen("chat-1", t0, 100.0, t0 + SUMMARY_HOVER_DELAY);

        // Pointer leaves
        let t_leave = t0 + SUMMARY_HOVER_DELAY + Duration::from_millis(200);
        hover.hover("chat-1".into(), 100.0, false, t_leave);

        // Exit plays: opacity 1 -> 0, dx 0 -> -6px
        let look_mid = hover
            .phase(t_leave + Duration::from_millis(60), false)
            .expect("leaving look");
        assert!(look_mid.opacity < 1.0 && look_mid.opacity > 0.0);
        assert!(look_mid.dx < 0.0 && look_mid.dx >= -6.0);
        assert!(look_mid.moving);

        // Exit finishes: None
        assert!(
            hover
                .phase(t_leave + Duration::from_millis(130), false)
                .is_none()
        );
    }

    #[test]
    fn press_immediate_exit_and_suppression() {
        let mut hover = SessionSummaryHover::default();
        let t0 = Instant::now();

        hover.hover("chat-1".into(), 100.0, true, t0);
        hover.ripen("chat-1", t0, 100.0, t0 + SUMMARY_HOVER_DELAY);

        // Click on row
        let t_press = t0 + SUMMARY_HOVER_DELAY + Duration::from_millis(50);
        hover.press("chat-1", t_press);

        // Plays exit
        let look = hover
            .phase(t_press + Duration::from_millis(40), false)
            .expect("leaving");
        assert_eq!(look.chat_id, "chat-1");

        // Hovering while suppressed returns None
        assert_eq!(
            hover.hover(
                "chat-1".into(),
                100.0,
                true,
                t_press + Duration::from_millis(50)
            ),
            None
        );
    }

    #[test]
    fn reduced_motion_instant_cut() {
        let mut hover = SessionSummaryHover::default();
        let t0 = Instant::now();

        hover.hover("chat-1".into(), 100.0, true, t0);
        hover.ripen("chat-1", t0, 100.0, t0 + SUMMARY_HOVER_DELAY);

        // Under reduced motion: instant cut, no movement
        let look = hover.phase(t0 + SUMMARY_HOVER_DELAY, true).expect("look");
        assert_eq!(look.opacity, 1.0);
        assert_eq!(look.dx, 0.0);
        assert!(!look.moving);

        // Pointer leaves: instant cut, is_none() at once
        hover.hover(
            "chat-1".into(),
            100.0,
            false,
            t0 + SUMMARY_HOVER_DELAY + Duration::from_millis(10),
        );
        assert!(
            hover
                .phase(t0 + SUMMARY_HOVER_DELAY + Duration::from_millis(10), true)
                .is_none()
        );
    }
}

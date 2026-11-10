//! A plan card (`component: "plan"`). The model, the approval message, and
//! the painter. Approval is for the whole plan. Keel does not check that the
//! later run follows the steps.

use gpui::{
    Animation, AnimationExt, AnyElement, Entity, FontFeatures, IntoElement, SharedString,
    div, prelude::*, px,
};
use gpui_base::input::InputState;
use keel_theme::Tokens;
use serde_json::Value;

use crate::hover_fade::HoverFadeExt;
use crate::icons::Icon;
use crate::motion::ease_out;
use crate::style::{MONO, RADIUS_MD, RADIUS_SM, c, zpx};
use crate::transcript::{OnRowAction, RowAction, TranscriptRowItem};

pub const APPROVE_RUN: &str = "Approve and run";
pub const EDIT_PLAN: &str = "Edit plan";
pub const KEEP_PLANNING: &str = "Keep planning";
pub const YES_AUTO: &str = "Auto-accept edits";
pub const YES_ASK: &str = "Approve each edit";
pub const KEEP: &str = "Keep planning";
pub const DONE: &str = "Done";
pub const ADD: &str = "Add step";
pub const REJECTED: &str = "Plan not approved";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Read,
    Edit,
    Run,
    Other,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Edit => "edit",
            Self::Run => "run",
            Self::Other => "other",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw.trim() {
            "read" => Self::Read,
            "edit" => Self::Edit,
            "run" => Self::Run,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub title: String,
    pub kind: Kind,
    pub targets: Vec<String>,
}

impl Step {
    /// Quiet second line, such as `edit · ui/view/mod.rs`.
    pub fn detail(&self) -> String {
        if self.targets.is_empty() {
            self.kind.as_str().to_string()
        } else {
            format!("{} · {}", self.kind.as_str(), self.targets.join(", "))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub title: String,
    pub summary: String,
    pub steps: Vec<Step>,
    pub footer: String,
}

impl Plan {
    /// A plan spec. `None` for another component, or one with no title or no steps.
    pub fn from_spec(spec: &Value) -> Option<Self> {
        if spec.get("component").and_then(Value::as_str) != Some("plan") {
            return None;
        }
        let title = spec
            .get("title")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())?;
        let steps: Vec<Step> = spec
            .get("steps")
            .and_then(Value::as_array)?
            .iter()
            .filter_map(|step| {
                let title = step
                    .get("title")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())?
                    .to_string();
                let kind = Kind::parse(step.get("kind").and_then(Value::as_str).unwrap_or("other"));
                let targets = step
                    .get("targets")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|v| v.as_str().map(str::trim).filter(|s| !s.is_empty()))
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                Some(Step { title, kind, targets })
            })
            .collect();
        if steps.is_empty() {
            return None;
        }
        let text = |key: &str| {
            spec.get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("")
                .to_string()
        };
        Some(Self {
            title: title.to_string(),
            summary: text("summary"),
            steps,
            footer: text("footer"),
        })
    }
}

/// One step as the user left it, including a note and a retitle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditStep {
    pub title: String,
    pub note: String,
    pub kind: Kind,
    pub targets: Vec<String>,
}

impl From<&Step> for EditStep {
    fn from(step: &Step) -> Self {
        Self {
            title: step.title.clone(),
            note: String::new(),
            kind: step.kind,
            targets: step.targets.clone(),
        }
    }
}

impl EditStep {
    pub fn detail(&self) -> String {
        Step {
            title: self.title.clone(),
            kind: self.kind,
            targets: self.targets.clone(),
        }
        .detail()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Streaming,
    Ready,
    Editing,
    Approved,
    Rejected,
}

impl Phase {
    pub fn buttons_enabled(self) -> bool {
        matches!(self, Self::Ready | Self::Editing)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Streaming => "streaming",
            Self::Ready => "ready",
            Self::Editing => "editing",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
        }
    }
}

/// A target item display representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetItem {
    pub display: String,
    pub full: String,
    pub is_command: bool,
}

/// Result of compacting targets for a step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactTargets {
    pub shared_dir: Option<String>,
    pub items: Vec<TargetItem>,
}

/// Compact a list of targets for display.
pub fn compact_targets(kind: Kind, targets: &[String]) -> CompactTargets {
    if targets.is_empty() {
        return CompactTargets {
            shared_dir: None,
            items: Vec::new(),
        };
    }
    if kind == Kind::Run {
        let items = targets
            .iter()
            .map(|t| TargetItem {
                display: t.clone(),
                full: t.clone(),
                is_command: true,
            })
            .collect();
        return CompactTargets {
            shared_dir: None,
            items,
        };
    }

    let (shared_dir, stripped_names) = strip_target_prefixes(targets);
    let items = targets
        .iter()
        .zip(stripped_names.into_iter())
        .map(|(full, display)| TargetItem {
            display,
            full: full.clone(),
            is_command: false,
        })
        .collect();

    CompactTargets { shared_dir, items }
}

/// Strip longest shared directory prefix (ending with '/') and common filename prefix.
pub fn strip_target_prefixes(targets: &[String]) -> (Option<String>, Vec<String>) {
    if targets.is_empty() {
        return (None, Vec::new());
    }

    // 1. Longest shared directory prefix ending with '/'
    let shared_dir = if targets.len() == 1 {
        let t = &targets[0];
        t.rfind('/').map(|pos| t[..=pos].to_string())
    } else {
        let first = &targets[0];
        let mut common_len = first.len();
        for t in &targets[1..] {
            common_len = first[..common_len]
                .char_indices()
                .zip(t.char_indices())
                .take_while(|((_, c1), (_, c2))| c1 == c2)
                .last()
                .map(|((ix, c), _)| ix + c.len_utf8())
                .unwrap_or(0);
        }
        let common = &first[..common_len];
        common.rfind('/').map(|pos| common[..=pos].to_string())
    };

    let dir_len = shared_dir.as_ref().map_or(0, |d| d.len());
    let mut filenames: Vec<String> = targets
        .iter()
        .map(|t| t[dir_len..].to_string())
        .collect();

    // 2. Shared filename prefix stripping
    let dir_segments: Vec<&str> = shared_dir
        .as_deref()
        .unwrap_or("")
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();

    let mut common_fn_len = if filenames.is_empty() { 0 } else { filenames[0].len() };
    for f in &filenames[1..] {
        common_fn_len = filenames[0][..common_fn_len]
            .char_indices()
            .zip(f.char_indices())
            .take_while(|((_, c1), (_, c2))| c1 == c2)
            .last()
            .map(|((ix, c), _)| ix + c.len_utf8())
            .unwrap_or(0);
    }
    let common_fn = if common_fn_len > 0 { &filenames[0][..common_fn_len] } else { "" };
    let shared_fn_prefix = if filenames.len() > 1 {
        common_fn.rfind(['_', '-']).map(|pos| &common_fn[..=pos])
    } else {
        None
    };

    let matching_dir_prefix = dir_segments.iter().rev().find_map(|seg| {
        let with_us = format!("{seg}_");
        let with_dash = format!("{seg}-");
        if filenames.iter().all(|f| f.starts_with(&with_us)) {
            Some(with_us)
        } else if filenames.iter().all(|f| f.starts_with(&with_dash)) {
            Some(with_dash)
        } else {
            None
        }
    });

    let strip_prefix = shared_fn_prefix.map(str::to_string).or(matching_dir_prefix);

    if let Some(prefix) = strip_prefix {
        let prefix_len = prefix.len();
        let safe = filenames.iter().all(|f| {
            if f.len() <= prefix_len {
                return false;
            }
            let rest = &f[prefix_len..];
            let stem = rest.split('.').next().unwrap_or("");
            !stem.is_empty()
        });
        if safe {
            for f in &mut filenames {
                *f = f[prefix_len..].to_string();
            }
        }
    }

    (shared_dir, filenames)
}

/// Permission mode chosen for plan execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApprovalMode {
    #[default]
    AutoEdit,
    Ask,
    Bypass,
}

impl ApprovalMode {
    pub const ALL: [Self; 3] = [Self::AutoEdit, Self::Ask, Self::Bypass];

    pub fn label(self) -> &'static str {
        match self {
            Self::AutoEdit => "Auto-accept edits",
            Self::Ask => "Approve each edit",
            Self::Bypass => "Bypass permissions",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Self::AutoEdit => "Edits run, commands ask",
            Self::Ask => "Asks before acting",
            Self::Bypass => "Nothing asks",
        }
    }

    pub fn to_permission(self) -> keel_proto::PermissionMode {
        match self {
            Self::AutoEdit => keel_proto::PermissionMode::AutoEdit,
            Self::Ask => keel_proto::PermissionMode::Ask,
            Self::Bypass => keel_proto::PermissionMode::FullAccess,
        }
    }

    pub fn from_permission(perm: keel_proto::PermissionMode) -> Self {
        match perm {
            keel_proto::PermissionMode::AutoEdit => Self::AutoEdit,
            keel_proto::PermissionMode::Ask => Self::Ask,
            keel_proto::PermissionMode::FullAccess => Self::Bypass,
        }
    }
}

/// Edits and phase for one card. Inputs exist only while editing.
#[derive(Clone)]
pub struct CardState {
    pub phase: Phase,
    pub steps: Vec<EditStep>,
    pub expanded: bool,
    pub drag_from: Option<usize>,
    pub titles: Vec<Entity<InputState>>,
    pub notes: Vec<Entity<InputState>>,
    pub fade: u64,
    pub manual: bool,
    pub mode: ApprovalMode,
    pub show_mode_menu: bool,
    pub expanded_targets: std::collections::HashSet<usize>,
}

impl CardState {
    pub fn from_plan(plan: &Plan, phase: Phase) -> Self {
        Self {
            phase,
            steps: plan.steps.iter().map(EditStep::from).collect(),
            expanded: false,
            drag_from: None,
            titles: Vec::new(),
            notes: Vec::new(),
            fade: 1,
            manual: false,
            mode: ApprovalMode::AutoEdit,
            show_mode_menu: false,
            expanded_targets: std::collections::HashSet::new(),
        }
    }

    pub fn set_mode(&mut self, mode: ApprovalMode) {
        self.mode = mode;
        self.manual = mode == ApprovalMode::Ask;
    }
}

/// `Approved plan:` then `N. title` and any note, then `Execute it.`
/// What the agent receives when the plan is approved. The transcript does
/// not draw this as a user message. The card is the record.
pub fn approval_text(steps: &[EditStep]) -> String {
    let mut out = String::from("Implement the approved plan.\n");
    for (ix, step) in steps.iter().enumerate() {
        out.push_str(&format!("{}. {}\n", ix + 1, step.title.trim()));
        let note = step.note.trim();
        if !note.is_empty() {
            out.push_str(note);
            out.push('\n');
        }
    }
    out
}

pub fn approved_line(count: usize) -> String {
    format!("Plan approved · {count} steps")
}

/// What a debug fixture reports. The same words the card paints.
pub fn fixture_reply(phase: Phase, plan: &Plan, steps: &[EditStep]) -> String {
    let titles: Vec<&str> = steps.iter().map(|s| s.title.as_str()).collect();
    let buttons = if phase == Phase::Streaming {
        "buttons: disabled"
    } else if phase.buttons_enabled() {
        "buttons: enabled"
    } else {
        "buttons: hidden"
    };
    match phase {
        Phase::Approved => format!(
            "state: approved\n{}\nsteps: {}\n{buttons}",
            approved_line(steps.len()),
            titles.join(" | ")
        ),
        Phase::Rejected => format!("state: rejected\n{REJECTED}\nsteps: {}", titles.join(" | ")),
        Phase::Editing => format!(
            "state: editing\nsteps: {}\n{DONE}\n{ADD}\n{buttons}\n{APPROVE_RUN}\n{EDIT_PLAN}\n{KEEP_PLANNING}",
            titles.join(" | ")
        ),
        other => format!(
            "state: {}\ntitle: {}\nsteps: {}\n{buttons}\n{APPROVE_RUN}\n{EDIT_PLAN}\n{KEEP_PLANNING}",
            other.name(),
            plan.title,
            titles.join(" | ")
        ),
    }
}

pub fn sample_plan() -> Plan {
    Plan::from_spec(&serde_json::json!({
        "component": "plan",
        "title": "Implement parameter management and audit service",
        "summary": "Refactor employee parameter usecases to domain-driven design, add validation pipelines, and verify database consistency.",
        "footer": "Database migrations, frontend bindings, documentation",
        "steps": [
            {
                "title": "Analyze parameter domain usecase interfaces",
                "kind": "read",
                "targets": [
                    "app/employee_parameter/usecase/employee_parameter_crud_usecase.go",
                    "app/employee_parameter/usecase/employee_parameter_query_usecase.go"
                ]
            },
            {
                "title": "Extract parameter validation rules and repository methods",
                "kind": "read",
                "targets": [
                    "app/employee_parameter/usecase/employee_parameter_validation.go",
                    "app/employee_parameter/usecase/employee_parameter_repository.go"
                ]
            },
            {
                "title": "Refactor CRUD and export parameter usecases",
                "kind": "edit",
                "targets": [
                    "app/employee_parameter/usecase/employee_parameter_crud_usecase.go",
                    "app/employee_parameter/usecase/employee_parameter_export_usecase.go",
                    "app/employee_parameter/usecase/employee_parameter_batch_usecase.go",
                    "app/employee_parameter/usecase/employee_parameter_helper.go"
                ]
            },
            {
                "title": "Implement audit log tracking for parameter updates",
                "kind": "edit",
                "targets": [
                    "app/employee_parameter/usecase/employee_parameter_audit_usecase.go",
                    "app/employee_parameter/usecase/employee_parameter_event_handler.go"
                ]
            },
            {
                "title": "Update engine router and handler registrations",
                "kind": "edit",
                "targets": [
                    "crates/engine/src/genui.rs",
                    "crates/engine/src/lib.rs"
                ]
            },
            {
                "title": "Update plan approval card and steps view",
                "kind": "edit",
                "targets": [
                    "crates/ui/src/plan.rs"
                ]
            },
            {
                "title": "Execute parameter test suites",
                "kind": "run",
                "targets": [
                    "go test -v ./app/employee_parameter/usecase/..."
                ]
            },
            {
                "title": "Verify UI integration test suite",
                "kind": "run",
                "targets": [
                    "cargo test -p keel-ui"
                ]
            }
        ]
    }))
    .expect("sample plan")
}

fn tnum<E: Styled>(el: E) -> E {
    el.font_features(FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)])))
}

fn once(ms: u64, reduced: bool, curve: impl Fn(f32) -> f32 + 'static) -> Animation {
    Animation::new(std::time::Duration::from_millis(if reduced { 1 } else { ms })).with_easing(curve)
}

/// The card. `state` is the user's edits when they have any.
pub fn render(
    item: &TranscriptRowItem,
    plan: &Plan,
    state: Option<&CardState>,
    running: bool,
    reduced: bool,
    on_action: OnRowAction,
    t: Tokens,
) -> gpui::Div {
    let row_id = item.row_id().to_string();
    let phase = state.map(|s| s.phase).unwrap_or(if running {
        Phase::Streaming
    } else {
        Phase::Ready
    });
    let steps: Vec<EditStep> = state
        .map(|s| s.steps.clone())
        .unwrap_or_else(|| plan.steps.iter().map(EditStep::from).collect());
    let expanded = state.is_some_and(|s| s.expanded);
    let fade = state.map(|s| s.fade).unwrap_or(0);
    let collapsed = matches!(phase, Phase::Approved | Phase::Rejected) && !expanded;

    let header: AnyElement = if collapsed {
        let line = if phase == Phase::Approved {
            approved_line(steps.len())
        } else {
            REJECTED.to_string()
        };
        let toggle = on_action.clone();
        let id = row_id.clone();
        div()
            .id(SharedString::from(format!("plan-fold-{row_id}")))
            .h(zpx(36.))
            .px(zpx(16.))
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(8.))
            .cursor_pointer()
            .text_color(c(t.text_muted))
            .text_size(zpx(13.))
            .on_click(move |_, _, cx| {
                toggle(RowAction::PlanToggle { row_id: id.clone() }, cx);
            })
            .child(Icon::ListChecks.el(zpx(14.)))
            .child(tnum(div()).child(line))
            .into_any_element()
    } else {
        div()
            .h(zpx(36.))
            .px(zpx(16.))
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(8.))
            .child(Icon::ListChecks.el(zpx(14.)).text_color(c(t.text_muted)))
            .child(
                div()
                    .text_size(zpx(13.))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(c(t.text))
                    .child("Plan"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_size(zpx(13.))
                    .text_color(c(t.text_muted))
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(plan.title.clone()),
            )
            .child(
                tnum(
                    div()
                        .flex_none()
                        .text_size(zpx(12.))
                        .text_color(c(t.text_faint)),
                )
                .child(format!("{} steps", steps.len())),
            )
            .into_any_element()
    };

    let mut body: Vec<AnyElement> = Vec::new();
    if !collapsed {
        if !plan.summary.is_empty() {
            body.push(
                div()
                    .px(zpx(16.))
                    .pb(zpx(8.))
                    .text_size(zpx(13.))
                    .text_color(c(t.text_muted))
                    .child(plan.summary.clone())
                    .into_any_element(),
            );
        }
        for (ix, step) in steps.iter().enumerate() {
            body.push(step_row(
                &row_id,
                ix,
                step,
                phase,
                state,
                reduced,
                on_action.clone(),
                t,
            ));
        }
        if !plan.footer.is_empty() && phase != Phase::Editing {
            body.push(
                div()
                    .px(zpx(16.))
                    .pt(zpx(12.))
                    .flex()
                    .flex_row()
                    .items_baseline()
                    .gap(zpx(6.))
                    .text_size(zpx(12.))
                    .child(
                        div()
                            .text_color(c(t.text_muted))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child("Not included"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_color(c(t.text_faint))
                            .child(plan.footer.clone()),
                    )
                    .into_any_element(),
            );
        }
        body.push(actions(&row_id, phase, state, reduced, on_action.clone(), t));
    }

    let inner = div()
        .w_full()
        .flex()
        .flex_col()
        .child(header)
        .children(body);

    let inner = if collapsed {
        inner
            .with_animation(
                SharedString::from(format!("plan-collapse-{row_id}-{fade}")),
                once(200, reduced, ease_out),
                |el, d| el.max_h(px(36. + (1. - d) * 240.)),
            )
            .into_any_element()
    } else if fade > 0 {
        inner
            .with_animation(
                SharedString::from(format!("plan-fade-{row_id}-{fade}")),
                once(150, reduced, ease_out),
                |el, d| el.opacity(d),
            )
            .into_any_element()
    } else {
        inner.into_any_element()
    };

    div()
        .w_full()
        .my_2()
        .rounded(RADIUS_MD)
        .bg(c(t.code))
        .flex()
        .flex_col()
        .child(inner)
}

fn step_row(
    row_id: &str,
    ix: usize,
    step: &EditStep,
    phase: Phase,
    state: Option<&CardState>,
    reduced: bool,
    on_action: OnRowAction,
    t: Tokens,
) -> AnyElement {
    let delay = (ix as u64).saturating_mul(30).min(150);
    let total = 160 + delay;
    let row = div()
        .px(zpx(16.))
        .py(zpx(4.))
        .flex()
        .flex_row()
        .items_start()
        .gap(zpx(10.))
        .child(
            tnum(
                div()
                    .mt(zpx(1.))
                    .size(zpx(18.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .border_1()
                    .border_color(c(t.border))
                    .text_size(zpx(11.))
                    .text_color(c(t.text_muted)),
            )
            .child(format!("{}", ix + 1)),
        )
        .child(step_body(row_id, ix, step, phase, state, reduced, on_action.clone(), t))
        .when(phase == Phase::Editing, |d| {
            let (id, action) = (row_id.to_string(), on_action.clone());
            d.child(
                div()
                    .id(SharedString::from(format!("plan-del-{row_id}-{ix}")))
                    .mt(zpx(2.))
                    .size(zpx(22.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(RADIUS_MD)
                    .cursor_pointer()
                    .text_color(c(t.text_faint))
                    .hover(|s| s.text_color(c(t.danger)).bg(c(t.hover)))
                    .child(Icon::Trash.el(zpx(13.)))
                    .on_click(move |_, _, cx| {
                        action(
                            RowAction::PlanDelete {
                                row_id: id.clone(),
                                index: ix,
                            },
                            cx,
                        );
                    }),
            )
        });
    row.with_animation(
        SharedString::from(format!("plan-step-{row_id}-{ix}")),
        once(total, reduced, move |t| {
            let hold = delay as f32 / total as f32;
            if t <= hold {
                0.
            } else {
                ease_out((t - hold) / (1. - hold))
            }
        }),
        |el, d| el.opacity(d).relative().top(zpx(4. * (1. - d))),
    )
    .into_any_element()
}

fn step_body(
    row_id: &str,
    ix: usize,
    step: &EditStep,
    phase: Phase,
    state: Option<&CardState>,
    reduced: bool,
    on_action: OnRowAction,
    t: Tokens,
) -> gpui::Div {
    if phase == Phase::Editing {
        let title = state.and_then(|s| s.titles.get(ix)).cloned();
        let note = state.and_then(|s| s.notes.get(ix)).cloned();
        let (drag_id, drop_id) = (row_id.to_string(), row_id.to_string());
        let (drag, drop) = (on_action.clone(), on_action);
        return div().flex_1().min_w_0().flex().flex_col().gap(zpx(4.)).child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(6.))
                .child(
                    div()
                        .id(SharedString::from(format!("plan-grip-{row_id}-{ix}")))
                        .cursor_pointer()
                        .text_color(c(t.text_faint))
                        .child(Icon::Ellipsis.el(zpx(14.)))
                        .on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| {
                            drag(
                                RowAction::PlanDrag {
                                    row_id: drag_id.clone(),
                                    index: ix,
                                },
                                cx,
                            );
                        })
                        .on_mouse_up(
                            gpui::MouseButton::Left,
                            move |_, _, cx| {
                                drop(
                                    RowAction::PlanDrop {
                                        row_id: drop_id.clone(),
                                        index: ix,
                                    },
                                    cx,
                                );
                            },
                        ),
                )
                .child(editor(title, t).flex_1()),
        )
        .child(editor(note, t));
    }

    let compacted = compact_targets(step.kind, &step.targets);
    let is_expanded = state.is_some_and(|s| s.expanded_targets.contains(&ix));
    let has_overflow = compacted.items.len() > 2;
    let visible_items = if has_overflow && !is_expanded {
        &compacted.items[..2]
    } else {
        &compacted.items[..]
    };

    let mut target_chips: Vec<AnyElement> = Vec::new();
    for (target_ix, item) in visible_items.iter().enumerate() {
        let full = item.full.clone();
        target_chips.push(
            div()
                .id(SharedString::from(format!("target-chip-{row_id}-{ix}-{target_ix}")))
                .h(zpx(18.))
                .px_1p5()
                .flex()
                .items_center()
                .rounded(RADIUS_SM)
                .font_family(MONO)
                .text_size(zpx(11.))
                .text_color(c(t.text_muted))
                .bg(c(t.surface))
                .border_1()
                .border_color(c(t.border))
                .cursor_pointer()
                .hover(|s| s.bg(c(t.hover)).text_color(c(t.text)))
                .tooltip(move |_, cx| cx.new(|_| crate::view::Tip::new(full.clone(), t)).into())
                .tooltip_show_delay(crate::view::tooltip::delay())
                .child(item.display.clone())
                .into_any_element(),
        );
    }

    if has_overflow {
        let (toggle_id, toggle_action) = (row_id.to_string(), on_action.clone());
        if !is_expanded {
            let more_count = compacted.items.len() - 2;
            target_chips.push(
                div()
                    .id(SharedString::from(format!("plan-more-{row_id}-{ix}")))
                    .h(zpx(18.))
                    .px_1p5()
                    .flex()
                    .items_center()
                    .rounded(RADIUS_SM)
                    .font_family(MONO)
                    .text_size(zpx(11.))
                    .text_color(c(t.accent))
                    .bg(c(t.surface))
                    .border_1()
                    .border_color(c(t.border))
                    .cursor_pointer()
                    .hover(|s| s.bg(c(t.hover)))
                    .active(|s| s.opacity(0.85))
                    .child(format!("+{} more", more_count))
                    .on_click(move |_, _, cx| {
                        toggle_action(
                            RowAction::PlanToggleTargets {
                                row_id: toggle_id.clone(),
                                step_ix: ix,
                            },
                            cx,
                        );
                    })
                    .into_any_element(),
            );
        } else {
            target_chips.push(
                div()
                    .id(SharedString::from(format!("plan-less-{row_id}-{ix}")))
                    .h(zpx(18.))
                    .px_1p5()
                    .flex()
                    .items_center()
                    .rounded(RADIUS_SM)
                    .font_family(MONO)
                    .text_size(zpx(11.))
                    .text_color(c(t.text_faint))
                    .bg(c(t.surface))
                    .border_1()
                    .border_color(c(t.border))
                    .cursor_pointer()
                    .hover(|s| s.bg(c(t.hover)).text_color(c(t.text)))
                    .active(|s| s.opacity(0.85))
                    .child("less")
                    .on_click(move |_, _, cx| {
                        toggle_action(
                            RowAction::PlanToggleTargets {
                                row_id: toggle_id.clone(),
                                step_ix: ix,
                            },
                            cx,
                        );
                    })
                    .into_any_element(),
            );
        }
    }

    let kind_label = format!("{} ·", step.kind.as_str());
    let detail_row = div()
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .gap(zpx(4.))
        .child(
            div()
                .text_size(zpx(12.))
                .text_color(c(t.text_faint))
                .child(kind_label),
        )
        .when_some(compacted.shared_dir, |d, dir| {
            d.child(
                div()
                    .text_size(zpx(11.5))
                    .font_family(MONO)
                    .text_color(c(t.text_faint))
                    .child(dir),
            )
        })
        .children(target_chips);

    let detail_animated = if has_overflow {
        detail_row
            .with_animation(
                SharedString::from(format!("plan-targets-{row_id}-{ix}-{is_expanded}")),
                once(if is_expanded { 160 } else { 120 }, reduced, ease_out),
                |el, d| el.opacity(d),
            )
            .into_any_element()
    } else {
        detail_row.into_any_element()
    };

    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(zpx(2.))
        .child(
            div()
                .text_size(zpx(13.))
                .text_color(c(t.text))
                .child(step.title.clone()),
        )
        .child(detail_animated)
        .when(!step.note.trim().is_empty(), |d| {
            d.child(
                div()
                    .text_size(zpx(12.))
                    .text_color(c(t.text_muted))
                    .child(step.note.clone()),
            )
        })
}

fn editor(input: Option<Entity<InputState>>, t: Tokens) -> gpui::Div {
    div()
        .w_full()
        .min_w_0()
        .h(zpx(26.))
        .px_2()
        .flex()
        .items_center()
        .rounded(RADIUS_MD)
        .bg(c(t.input))
        .text_size(zpx(13.))
        .text_color(c(t.text))
        .children(input)
}

fn actions(
    row_id: &str,
    phase: Phase,
    state: Option<&CardState>,
    _reduced: bool,
    on_action: OnRowAction,
    t: Tokens,
) -> AnyElement {
    let enabled = phase == Phase::Ready;
    let editing = phase == Phase::Editing;
    let row = div()
        .px(zpx(16.))
        .pt(zpx(12.))
        .pb(zpx(16.))
        .flex()
        .flex_row()
        .items_center()
        .gap(zpx(8.));

    if editing {
        let (done_id, add_id) = (row_id.to_string(), row_id.to_string());
        let (done, add) = (on_action.clone(), on_action);
        return row
            .child(
                primary_single(format!("plan-done-{row_id}"), DONE, true, t).on_click(move |_, _, cx| {
                    done(RowAction::PlanDone { row_id: done_id.clone() }, cx);
                }),
            )
            .child(
                secondary(format!("plan-add-{row_id}"), ADD, true, t).on_click(move |_, _, cx| {
                    add(RowAction::PlanAdd { row_id: add_id.clone() }, cx);
                }),
            )
            .into_any_element();
    }

    let mode = state.map(|s| s.mode).unwrap_or(if state.is_some_and(|s| s.manual) {
        ApprovalMode::Ask
    } else {
        ApprovalMode::AutoEdit
    });
    let manual = mode == ApprovalMode::Ask;
    let show_menu = state.is_some_and(|s| s.show_mode_menu);
    let mode_label = mode.label();

    let approve_id = row_id.to_string();
    let menu_id = row_id.to_string();
    let (do_approve, do_toggle_menu) = (on_action.clone(), on_action.clone());

    let split_dropdown_menu = if show_menu && enabled {
        let (auto_id, ask_id, bypass_id) = (row_id.to_string(), row_id.to_string(), row_id.to_string());
        let (set_auto, set_ask, set_bypass) = (on_action.clone(), on_action.clone(), on_action.clone());
        Some(
            div()
                .absolute()
                .bottom(zpx(34.))
                .left_0()
                .w(zpx(188.))
                .rounded(RADIUS_MD)
                .bg(c(t.floating))
                .border_1()
                .border_color(c(t.border))
                .text_color(c(t.text))
                .shadow(vec![
                    gpui::BoxShadow {
                        color: gpui::Hsla { a: 0.18, ..c(t.shadow) },
                        offset: gpui::point(zpx(0.), zpx(-4.)),
                        blur_radius: zpx(16.),
                        spread_radius: zpx(0.),
                        inset: false,
                    },
                    gpui::BoxShadow {
                        color: gpui::Hsla { a: 0.1, ..c(t.shadow) },
                        offset: gpui::point(zpx(0.), zpx(-1.)),
                        blur_radius: zpx(4.),
                        spread_radius: zpx(0.),
                        inset: false,
                    },
                ])
                .p_1()
                .flex()
                .flex_col()
                .gap(zpx(2.))
                .occlude()
                .child(
                    div()
                        .id(SharedString::from(format!("mode-auto-{row_id}")))
                        .px_2()
                        .py_1()
                        .rounded(RADIUS_SM)
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .text_size(zpx(12.5))
                        .cursor_pointer()
                        .text_color(if mode == ApprovalMode::AutoEdit { c(t.text) } else { c(t.text_muted) })
                        .when(mode == ApprovalMode::AutoEdit, |d| d.bg(c(t.hover)))
                        .hover(|s| s.bg(c(t.hover)).text_color(c(t.text)))
                        .child("Auto-accept edits")
                        .when(mode == ApprovalMode::AutoEdit, |d| d.child(Icon::Check.el(zpx(12.)).text_color(c(t.accent))))
                        .on_click(move |_, _, cx| {
                            set_auto(
                                RowAction::PlanSetMode {
                                    row_id: auto_id.clone(),
                                    mode: ApprovalMode::AutoEdit,
                                },
                                cx,
                            );
                        }),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("mode-ask-{row_id}")))
                        .px_2()
                        .py_1()
                        .rounded(RADIUS_SM)
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .text_size(zpx(12.5))
                        .cursor_pointer()
                        .text_color(if mode == ApprovalMode::Ask { c(t.text) } else { c(t.text_muted) })
                        .when(mode == ApprovalMode::Ask, |d| d.bg(c(t.hover)))
                        .hover(|s| s.bg(c(t.hover)).text_color(c(t.text)))
                        .child("Approve each edit")
                        .when(mode == ApprovalMode::Ask, |d| d.child(Icon::Check.el(zpx(12.)).text_color(c(t.accent))))
                        .on_click(move |_, _, cx| {
                            set_ask(
                                RowAction::PlanSetMode {
                                    row_id: ask_id.clone(),
                                    mode: ApprovalMode::Ask,
                                },
                                cx,
                            );
                        }),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("mode-bypass-{row_id}")))
                        .px_2()
                        .py_1()
                        .rounded(RADIUS_SM)
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .text_size(zpx(12.5))
                        .cursor_pointer()
                        .text_color(if mode == ApprovalMode::Bypass { c(t.text) } else { c(t.text_muted) })
                        .when(mode == ApprovalMode::Bypass, |d| d.bg(c(t.hover)))
                        .hover(|s| s.bg(c(t.hover)).text_color(c(t.text)))
                        .child("Bypass permissions")
                        .when(mode == ApprovalMode::Bypass, |d| d.child(Icon::Check.el(zpx(12.)).text_color(c(t.accent))))
                        .on_click(move |_, _, cx| {
                            set_bypass(
                                RowAction::PlanSetMode {
                                    row_id: bypass_id.clone(),
                                    mode: ApprovalMode::Bypass,
                                },
                                cx,
                            );
                        }),
                ),
        )
    } else {
        None
    };

    let split_primary = div()
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .h(zpx(28.))
        .rounded(RADIUS_MD)
        .bg(c(t.accent))
        .text_color(c(t.text_on_accent))
        .text_size(zpx(13.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .when(!enabled, |d| d.opacity(0.4))
        .when(enabled, |d| {
            d.child(
                div()
                    .id(SharedString::from(format!("plan-approve-{row_id}")))
                    .h_full()
                    .px_3()
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .hover(|s| s.opacity(0.9))
                    .active(|s| s.opacity(0.85))
                    .child(APPROVE_RUN)
                    .on_click(move |_, _, cx| {
                        do_approve(
                            RowAction::PlanApprove {
                                row_id: approve_id.clone(),
                                manual,
                            },
                            cx,
                        );
                    }),
            )
            .child(
                div()
                    .w(zpx(1.))
                    .h(zpx(14.))
                    .bg(c(t.text_on_accent))
                    .opacity(0.25),
            )
            .child(
                div()
                    .id(SharedString::from(format!("plan-mode-btn-{row_id}")))
                    .h_full()
                    .px_2()
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .hover(|s| s.opacity(0.9))
                    .active(|s| s.opacity(0.85))
                    .tooltip(move |_, cx| {
                        cx.new(|_| crate::view::Tip::new(format!("Mode: {mode_label}"), t))
                            .into()
                    })
                    .tooltip_show_delay(crate::view::tooltip::delay())
                    .child(Icon::ChevronDown.el(zpx(11.)))
                    .on_click(move |_, _, cx| {
                        do_toggle_menu(
                            RowAction::PlanToggleModeMenu {
                                row_id: menu_id.clone(),
                            },
                            cx,
                        );
                    }),
            )
            .children(split_dropdown_menu)
        })
        .when(!enabled, |d| {
            d.child(div().h_full().px_3().flex().items_center().child(APPROVE_RUN))
                .child(div().w(zpx(1.)).h(zpx(14.)).bg(c(t.text_on_accent)).opacity(0.25))
                .child(div().h_full().px_2().flex().items_center().child(Icon::ChevronDown.el(zpx(11.))))
        });

    let edit_id = row_id.to_string();
    let do_edit = on_action.clone();
    let edit_btn = secondary(format!("plan-edit-{row_id}"), EDIT_PLAN, enabled, t).when(enabled, |d| {
        d.on_click(move |_, _, cx| {
            do_edit(RowAction::PlanEdit { row_id: edit_id.clone() }, cx);
        })
    });

    let keep_id = row_id.to_string();
    let do_keep = on_action;
    let keep_btn = tertiary(format!("plan-keep-{row_id}"), KEEP_PLANNING, enabled, t).when(enabled, |d| {
        d.on_click(move |_, _, cx| {
            do_keep(RowAction::PlanAsk { row_id: keep_id.clone() }, cx);
        })
    });

    row.child(split_primary)
        .child(edit_btn)
        .child(keep_btn)
        .into_any_element()
}

fn primary_single(id: String, label: &'static str, enabled: bool, t: Tokens) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(id))
        .h(zpx(28.))
        .px_3()
        .flex()
        .items_center()
        .rounded(RADIUS_MD)
        .bg(c(t.accent))
        .text_color(c(t.text_on_accent))
        .text_size(zpx(13.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .when(enabled, |d| {
            d.cursor_pointer()
                .hover(|s| s.opacity(0.9))
                .active(|s| s.opacity(0.85))
        })
        .when(!enabled, |d| d.opacity(0.4))
        .child(label)
}

fn secondary(id: String, label: &'static str, enabled: bool, t: Tokens) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(id))
        .h(zpx(28.))
        .px_3()
        .flex()
        .items_center()
        .rounded(RADIUS_MD)
        .bg(c(t.surface))
        .text_color(c(t.text))
        .text_size(zpx(13.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .border_1()
        .border_color(c(t.border))
        .when(enabled, |d| {
            d.cursor_pointer()
                .hover_fade(c(t.hover), None)
                .hover(|s| s.bg(c(t.hover)))
                .active(|s| s.opacity(0.85))
        })
        .when(!enabled, |d| d.opacity(0.4))
        .child(label)
}

fn tertiary(id: String, label: &'static str, enabled: bool, t: Tokens) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(id))
        .h(zpx(28.))
        .px_2p5()
        .flex()
        .items_center()
        .rounded(RADIUS_MD)
        .text_size(zpx(13.))
        .text_color(c(t.text_muted))
        .when(enabled, |d| {
            d.cursor_pointer()
                .hover_fade(c(t.hover), None)
                .hover(|s| s.text_color(c(t.text)).bg(c(t.hover)))
                .active(|s| s.opacity(0.85))
        })
        .when(!enabled, |d| d.opacity(0.4))
        .child(label)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn step(title: &str, note: &str) -> EditStep {
        EditStep {
            title: title.into(),
            note: note.into(),
            kind: Kind::Edit,
            targets: vec!["ui/view/mod.rs".into()],
        }
    }

    #[test]
    fn prefix_stripping_shared_dir() {
        let targets = vec![
            "crates/ui/src/plan.rs".to_string(),
            "crates/ui/src/steps.rs".to_string(),
        ];
        let compacted = compact_targets(Kind::Edit, &targets);
        assert_eq!(compacted.shared_dir.as_deref(), Some("crates/ui/src/"));
        assert_eq!(compacted.items.len(), 2);
        assert_eq!(compacted.items[0].display, "plan.rs");
        assert_eq!(compacted.items[1].display, "steps.rs");
        assert_eq!(compacted.items[0].full, "crates/ui/src/plan.rs");
    }

    #[test]
    fn prefix_stripping_shared_filename_prefix() {
        let targets = vec![
            "app/employee_parameter/usecase/employee_parameter_crud_usecase.go".to_string(),
            "app/employee_parameter/usecase/employee_parameter_query_usecase.go".to_string(),
        ];
        let compacted = compact_targets(Kind::Edit, &targets);
        assert_eq!(
            compacted.shared_dir.as_deref(),
            Some("app/employee_parameter/usecase/")
        );
        assert_eq!(compacted.items.len(), 2);
        assert_eq!(compacted.items[0].display, "crud_usecase.go");
        assert_eq!(compacted.items[1].display, "query_usecase.go");
        assert_eq!(
            compacted.items[0].full,
            "app/employee_parameter/usecase/employee_parameter_crud_usecase.go"
        );
    }

    #[test]
    fn prefix_stripping_single_file() {
        let targets = vec!["crates/ui/src/plan.rs".to_string()];
        let compacted = compact_targets(Kind::Edit, &targets);
        assert_eq!(compacted.shared_dir.as_deref(), Some("crates/ui/src/"));
        assert_eq!(compacted.items.len(), 1);
        assert_eq!(compacted.items[0].display, "plan.rs");
        assert_eq!(compacted.items[0].full, "crates/ui/src/plan.rs");

        // Single file sharing step directory prefix
        let single_domain = vec![
            "app/employee_parameter/usecase/employee_parameter_crud_usecase.go".to_string(),
        ];
        let compacted_domain = compact_targets(Kind::Edit, &single_domain);
        assert_eq!(
            compacted_domain.shared_dir.as_deref(),
            Some("app/employee_parameter/usecase/")
        );
        assert_eq!(compacted_domain.items[0].display, "crud_usecase.go");
    }

    #[test]
    fn prefix_stripping_no_shared_prefix() {
        let targets = vec![
            "crates/ui/src/plan.rs".to_string(),
            "README.md".to_string(),
        ];
        let compacted = compact_targets(Kind::Edit, &targets);
        assert_eq!(compacted.shared_dir, None);
        assert_eq!(compacted.items.len(), 2);
        assert_eq!(compacted.items[0].display, "crates/ui/src/plan.rs");
        assert_eq!(compacted.items[1].display, "README.md");
    }

    #[test]
    fn prefix_stripping_mixed_with_commands() {
        let targets = vec!["cargo test -p keel-ui".to_string()];
        let compacted = compact_targets(Kind::Run, &targets);
        assert_eq!(compacted.shared_dir, None);
        assert_eq!(compacted.items.len(), 1);
        assert!(compacted.items[0].is_command);
        assert_eq!(compacted.items[0].display, "cargo test -p keel-ui");
        assert_eq!(compacted.items[0].full, "cargo test -p keel-ui");
    }

    #[test]
    fn overflow_count() {
        let targets = vec![
            "app/pkg/f1.go".to_string(),
            "app/pkg/f2.go".to_string(),
            "app/pkg/f3.go".to_string(),
            "app/pkg/f4.go".to_string(),
        ];
        let compacted = compact_targets(Kind::Edit, &targets);
        assert_eq!(compacted.items.len(), 4);
        let more_count = compacted.items.len().saturating_sub(2);
        assert_eq!(more_count, 2);
    }

    #[test]
    fn approval_text_keeps_edits_in_order() {
        let steps = vec![
            step("Add the plan card", "Keep the existing card shell."),
            step("Run the ui tests", ""),
        ];
        let text = approval_text(&steps);
        assert!(text.starts_with("Implement the approved plan.\n"));
        assert!(text.contains("1. Add the plan card\nKeep the existing card shell.\n"));
        assert!(text.contains("2. Run the ui tests\n"));
        assert!(!text.contains("Execute it."));
        assert!(!text.contains("Read the composer"));
        let detail = Step {
            title: "Add the plan card".into(),
            kind: Kind::Edit,
            targets: vec!["ui/view/mod.rs".into()],
        }
        .detail();
        assert_eq!(detail, "edit · ui/view/mod.rs");
    }

    #[test]
    fn approval_text_unchanged_by_display_compaction() {
        let plan = sample_plan();
        let edit_steps: Vec<EditStep> = plan.steps.iter().map(EditStep::from).collect();
        // Display compaction produces shortened chips for UI
        for s in &plan.steps {
            let compacted = compact_targets(s.kind, &s.targets);
            if s.targets.len() > 1 && compacted.shared_dir.is_some() {
                assert_ne!(compacted.items[0].display, s.targets[0]);
            }
        }
        // Underlying plan targets and approval text remain full and unaltered
        let text = approval_text(&edit_steps);
        assert!(text.starts_with("Implement the approved plan.\n"));
        assert!(text.contains("1. Analyze parameter domain usecase interfaces\n"));
        assert!(text.contains("7. Execute parameter test suites\n"));
        assert_eq!(
            edit_steps[0].targets[0],
            "app/employee_parameter/usecase/employee_parameter_crud_usecase.go"
        );
        assert_eq!(
            edit_steps[0].targets[1],
            "app/employee_parameter/usecase/employee_parameter_query_usecase.go"
        );
        assert_eq!(
            plan.steps[0].detail(),
            "read · app/employee_parameter/usecase/employee_parameter_crud_usecase.go, app/employee_parameter/usecase/employee_parameter_query_usecase.go"
        );
    }

    #[test]
    fn edit_mode_choice_carried_into_approval() {
        let plan = sample_plan();
        let mut card = CardState::from_plan(&plan, Phase::Ready);
        // Default edit mode is auto-accept edits (manual = false, ApprovalMode::AutoEdit)
        assert!(!card.manual);
        assert_eq!(card.mode, ApprovalMode::AutoEdit);
        assert_eq!(card.mode.to_permission(), keel_proto::PermissionMode::AutoEdit);

        // Switch to "Approve each edit"
        card.set_mode(ApprovalMode::Ask);
        assert!(card.manual);
        assert_eq!(card.mode, ApprovalMode::Ask);
        assert_eq!(card.mode.to_permission(), keel_proto::PermissionMode::Ask);

        // Switch to "Bypass permissions"
        card.set_mode(ApprovalMode::Bypass);
        assert!(!card.manual);
        assert_eq!(card.mode, ApprovalMode::Bypass);
        assert_eq!(card.mode.to_permission(), keel_proto::PermissionMode::FullAccess);

        // When approved, mode choice persists
        card.phase = Phase::Approved;
        assert_eq!(card.mode, ApprovalMode::Bypass);
        assert_eq!(card.mode.to_permission(), keel_proto::PermissionMode::FullAccess);

        // Verify per-session persistence mapping
        let mut session_modes = std::collections::HashMap::new();
        session_modes.insert("chat-1".to_string(), card.mode);
        assert_eq!(session_modes.get("chat-1"), Some(&ApprovalMode::Bypass));

        // Another card created for another session defaults to AutoEdit
        let card2 = CardState::from_plan(&plan, Phase::Ready);
        assert_eq!(card2.mode, ApprovalMode::AutoEdit);
    }

    #[test]
    fn suggestion_suppressed_while_ready() {
        let is_plan_pending = |phase: Phase| -> bool {
            matches!(phase, Phase::Ready | Phase::Editing | Phase::Streaming)
        };

        // Ready, Editing, and Streaming suppress suggestions
        assert!(is_plan_pending(Phase::Ready));
        assert!(is_plan_pending(Phase::Editing));
        assert!(is_plan_pending(Phase::Streaming));

        // Approved and Rejected allow normal suggestions to resume
        assert!(!is_plan_pending(Phase::Approved));
        assert!(!is_plan_pending(Phase::Rejected));
    }

    #[test]
    fn from_spec_rejects_a_chart_and_an_empty_plan() {
        assert!(Plan::from_spec(&json!({"component": "chart"})).is_none());
        assert!(Plan::from_spec(&json!({"component": "plan", "steps": []})).is_none());
        assert!(Plan::from_spec(&json!({"component": "plan", "title": "T"})).is_none());
        let plan = Plan::from_spec(&json!({
            "component": "plan",
            "title": "Ship it",
            "summary": "One sentence.",
            "footer": "none",
            "steps": [{"title": "Read", "kind": "read", "targets": ["a.rs"]}]
        }))
        .unwrap();
        assert_eq!(plan.steps[0].kind, Kind::Read);
        assert_eq!(
            fixture_reply(Phase::Ready, &plan, &[EditStep::from(&plan.steps[0])]),
            format!(
                "state: ready\ntitle: Ship it\nsteps: Read\nbuttons: enabled\n{APPROVE_RUN}\n{EDIT_PLAN}\n{KEEP_PLANNING}"
            )
        );
        assert!(
            fixture_reply(Phase::Streaming, &plan, &[EditStep::from(&plan.steps[0])])
                .contains("buttons: disabled")
        );
        assert!(
            fixture_reply(Phase::Approved, &plan, &[EditStep::from(&plan.steps[0])])
                .contains("Plan approved · 1 steps")
        );
        assert!(
            fixture_reply(Phase::Rejected, &plan, &[EditStep::from(&plan.steps[0])])
                .contains(REJECTED)
        );
    }
}

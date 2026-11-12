//! Composer, Questions, and Pickers component for Keel.
//!
//! Controls:
//! - Send / Steer / Stop morph button:
//!   - Idle, any text: Send. Enter sends, Shift+Enter inserts newline.
//!   - Running, field non-empty: Steer. Enter queues steer.
//!   - Running, field empty: Stop. Enter interrupts.
//! - AwaitingInput: composer replaced by Question panel. Digits 1-9 pick option.
//!   Paging if multiple questions. Required questions ignore Escape. Stop interrupts.
//! - Harness, Model, and Folder pickers.
//! - Draft text is device-local per chat in `ui-settings.json`.

#[cfg(test)]
use keel_proto::QuestionOption;
use keel_proto::{QuestionKind, UserInputAnswer, UserInputQuestion};

/// Composer morph action state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimaryButtonAction {
    Send,
    Steer,
    Stop,
}

impl PrimaryButtonAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Send => "Send",
            Self::Steer => "Steer",
            Self::Stop => "Stop",
        }
    }
}

/// Compute the primary button action from chat status and text input.
pub fn compute_button_action(is_running: bool, text: &str) -> PrimaryButtonAction {
    if !is_running {
        PrimaryButtonAction::Send
    } else if text.trim().is_empty() {
        PrimaryButtonAction::Stop
    } else {
        PrimaryButtonAction::Steer
    }
}

/// State for the questions panel when chat status is `awaiting_input`.
#[derive(Debug, Clone)]
pub struct QuestionPanelState {
    pub question_id: String,
    pub questions: Vec<UserInputQuestion>,
    pub current_page: usize,
    pub answers: Vec<UserInputAnswer>,
    /// Focused button (permission) or row (question).
    pub focus: usize,
    /// The focus ring shows only once the keyboard has moved or used it.
    pub keyboard: bool,
    /// Multi-select: checked option indices.
    pub checked: std::collections::BTreeSet<usize>,
    /// The "Other…" field is open.
    pub other_open: bool,
    /// The arguments block is expanded past its clamp.
    pub show_all: bool,
    /// When this page appeared, for its entry motion.
    pub shown_at: std::time::Instant,
    /// Unique per page shown, for element ids that must not carry state over.
    pub epoch: u64,
}

static PANEL_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl QuestionPanelState {
    pub fn new(question_id: String, questions: Vec<UserInputQuestion>) -> Self {
        let mut panel = Self {
            question_id,
            questions,
            current_page: 0,
            answers: Vec::new(),
            focus: 0,
            keyboard: false,
            checked: Default::default(),
            other_open: false,
            show_all: false,
            shown_at: std::time::Instant::now(),
            epoch: 0,
        };
        panel.reset_page();
        panel
    }

    /// Fresh per-page UI state: focus on Allow (or the first row), no ring.
    fn reset_page(&mut self) {
        self.focus = match self.current_question() {
            Some(q) if q.is_permission() => {
                crate::approval::default_focus(&crate::approval::permission_buttons(q))
            }
            _ => 0,
        };
        self.keyboard = false;
        self.checked.clear();
        self.other_open = false;
        self.show_all = false;
        self.shown_at = std::time::Instant::now();
        self.epoch = PANEL_EPOCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Answer with the option at `ix` (an index into the question's options).
    pub fn pick(&mut self, ix: usize) -> Option<Vec<UserInputAnswer>> {
        self.select_digit(ix + 1)
    }

    /// Multi-select: check or uncheck option `ix`.
    pub fn toggle(&mut self, ix: usize) {
        if self
            .current_question()
            .is_some_and(|q| ix < q.options.len())
            && !self.checked.remove(&ix)
        {
            self.checked.insert(ix);
        }
    }

    /// Multi-select: send the checked options' ids, one per line.
    pub fn submit_checked(&mut self) -> Option<Vec<UserInputAnswer>> {
        let q = self.current_question()?;
        if self.checked.is_empty() {
            return None;
        }
        let value = self
            .checked
            .iter()
            .filter_map(|&ix| q.options.get(ix))
            .map(|o| o.id.clone())
            .collect::<Vec<_>>()
            .join("\n");
        let kind = q.kind;
        self.record_answer(value, kind)
    }

    pub fn current_question(&self) -> Option<&UserInputQuestion> {
        self.questions.get(self.current_page)
    }

    pub fn is_last_page(&self) -> bool {
        self.current_page + 1 >= self.questions.len()
    }

    /// Select an option by 1-based digit index (1-9).
    pub fn select_digit(&mut self, digit: usize) -> Option<Vec<UserInputAnswer>> {
        let q = self.current_question()?;
        let option_ix = digit.checked_sub(1)?;
        let opt = q.options.get(option_ix)?;
        self.record_answer(opt.id.clone(), q.kind)
    }

    /// Whether the current question takes free text (Input / Editor).
    pub fn needs_text(&self) -> bool {
        self.other_open
            || matches!(
                self.current_question().map(|q| q.kind),
                Some(QuestionKind::Input | QuestionKind::Editor)
            )
    }

    /// Submit free text for the current question.
    pub fn submit_text(&mut self, text: &str) -> Option<Vec<UserInputAnswer>> {
        let kind = self.current_question()?.kind;
        self.record_answer(text.to_string(), kind)
    }

    /// Escape: a required question ignores it (returns `None` and stays put);
    /// an optional one is skipped with an empty answer.
    pub fn escape(&mut self) -> Option<Vec<UserInputAnswer>> {
        let q = self.current_question()?;
        if q.required {
            return None;
        }
        let kind = q.kind;
        self.record_answer(String::new(), kind)
    }

    /// Record an answer for the current question and advance or complete.
    pub fn record_answer(
        &mut self,
        value: String,
        kind: QuestionKind,
    ) -> Option<Vec<UserInputAnswer>> {
        let q_id = self.current_question()?.id.clone();
        self.answers.push(UserInputAnswer {
            question_id: q_id,
            kind,
            value,
        });

        if self.is_last_page() {
            Some(std::mem::take(&mut self.answers))
        } else {
            self.current_page += 1;
            self.reset_page();
            None
        }
    }
}

/// Harness option for the harness picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessOption {
    pub id: String,
    pub display_name: String,
    pub installed: bool,
    /// Modes this harness honours; one entry hides the permission chip.
    pub permission_modes: Vec<keel_proto::PermissionMode>,
}

impl HarnessOption {
    /// Parse the `list_harnesses` result rows.
    pub fn parse_list(rows: &[serde_json::Value]) -> Vec<Self> {
        rows.iter()
            .filter_map(|v| {
                let id = v.get("id")?.as_str()?.to_string();
                Some(Self {
                    display_name: v
                        .get("display_name")
                        .and_then(|d| d.as_str())
                        .unwrap_or(&id)
                        .to_string(),
                    installed: v
                        .get("installed")
                        .and_then(|i| i.as_bool())
                        .unwrap_or(false),
                    permission_modes: v
                        .get("permission_modes")
                        .and_then(|m| serde_json::from_value(m.clone()).ok())
                        .unwrap_or_default(),
                    id,
                })
            })
            .collect()
    }

    /// Default harness: the first installed one (Keel; the engine lists no other).
    pub fn default_id(list: &[Self]) -> Option<String> {
        list.iter().find(|h| h.installed).map(|h| h.id.clone())
    }
}

/// One model from `list_models`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelOption {
    pub id: String,
    pub name: String,
    /// Empty: this model has no effort knob.
    pub reasoning_levels: Vec<String>,
    /// Context size in tokens, when the harness reports it.
    pub context_window: Option<u64>,
}

/// Parsed `list_models` result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelList {
    pub models: Vec<ModelOption>,
    pub default_model: Option<String>,
}

impl ModelList {
    pub fn parse(res: &serde_json::Value) -> Self {
        let models = res
            .get("models")
            .and_then(|m| m.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| {
                        let id = m.get("id")?.as_str()?.to_string();
                        let raw_name = m
                            .get("name")
                            .and_then(|n| n.as_str())
                            .map(str::trim)
                            .filter(|n| !n.is_empty())
                            .unwrap_or(&id);
                        Some(ModelOption {
                            name: crate::model_menu::format_name(raw_name),
                            reasoning_levels: m
                                .get("reasoning_levels")
                                .and_then(|r| r.as_array())
                                .map(|r| {
                                    r.iter()
                                        .filter_map(|l| l.as_str().map(String::from))
                                        .collect()
                                })
                                .unwrap_or_default(),
                            context_window: ["context_window", "contextWindow"]
                                .iter()
                                .find_map(|k| m.get(*k)?.as_u64())
                                .filter(|w| *w > 0),
                            id,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let default_model = res
            .get("default_model")
            .and_then(|d| d.as_str())
            .filter(|d| !d.is_empty())
            .map(String::from);
        Self {
            models,
            default_model,
        }
    }

    pub fn get(&self, id: &str) -> Option<&ModelOption> {
        self.models.iter().find(|m| m.id == id)
    }

    /// The model new sessions use when nothing was picked yet.
    pub fn initial_model(&self) -> String {
        self.default_model
            .clone()
            .or_else(|| self.models.first().map(|m| m.id.clone()))
            .unwrap_or_default()
    }

    /// Effort levels for `model` (empty: no knob, hide the picker).
    pub fn levels(&self, model: &str) -> &[String] {
        self.get(model)
            .map(|m| m.reasoning_levels.as_slice())
            .unwrap_or(&[])
    }

    /// Keep `effort` if `model` supports it, otherwise fall back to the
    /// harness default (empty string). A saved retired poteto lands on
    /// `high` (else the model's top level).
    pub fn coerce_effort(&self, model: &str, effort: &str) -> String {
        let effort = &keel_proto::migrate_effort(effort, self.levels(model));
        if self.levels(model).iter().any(|l| l == effort) {
            effort.to_string()
        } else {
            String::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_poteto_effort_coerces_to_a_real_level() {
        let list = ModelList::parse(&serde_json::json!({"models": [
            {"id": "a", "name": "A", "reasoning_levels": ["low", "medium", "high", "xhigh"]},
            {"id": "b", "name": "B", "reasoning_levels": ["off"]}
        ]}));
        assert_eq!(list.coerce_effort("a", "poteto"), "high");
        assert_eq!(list.coerce_effort("b", "poteto"), "");
        assert_eq!(list.coerce_effort("a", "medium"), "medium");
    }

    #[test]
    fn button_action_morphs_correctly() {
        // Idle with any text -> Send
        assert_eq!(compute_button_action(false, ""), PrimaryButtonAction::Send);
        assert_eq!(
            compute_button_action(false, "hello"),
            PrimaryButtonAction::Send
        );

        // Running with text -> Steer
        assert_eq!(
            compute_button_action(true, "faster"),
            PrimaryButtonAction::Steer
        );

        // Running with empty text -> Stop
        assert_eq!(compute_button_action(true, ""), PrimaryButtonAction::Stop);
        assert_eq!(
            compute_button_action(true, "   "),
            PrimaryButtonAction::Stop
        );
    }

    #[test]
    fn question_panel_digit_paging_and_completion() {
        let q1 = UserInputQuestion {
            id: "q1".to_string(),
            kind: QuestionKind::Select,
            prompt: "Choose option:".to_string(),
            options: vec![
                QuestionOption {
                    id: "opt_a".to_string(),
                    label: "A".to_string(),
                    role: Default::default(),
                    detail: None,
                },
                QuestionOption {
                    id: "opt_b".to_string(),
                    label: "B".to_string(),
                    role: Default::default(),
                    detail: None,
                },
            ],
            required: true,
            tool: None,
            multi: false,
            other: false,
        };
        let q2 = UserInputQuestion {
            id: "q2".to_string(),
            kind: QuestionKind::Confirm,
            prompt: "Confirm?".to_string(),
            options: vec![
                QuestionOption {
                    id: "yes".to_string(),
                    label: "Yes".to_string(),
                    role: Default::default(),
                    detail: None,
                },
                QuestionOption {
                    id: "no".to_string(),
                    label: "No".to_string(),
                    role: Default::default(),
                    detail: None,
                },
            ],
            required: false,
            tool: None,
            multi: false,
            other: false,
        };

        let mut panel = QuestionPanelState::new("batch_1".to_string(), vec![q1, q2]);
        assert_eq!(panel.current_page, 0);

        // Digit 1 selects "opt_a" on page 0
        let res = panel.select_digit(1);
        assert!(res.is_none());
        assert_eq!(panel.current_page, 1);

        // Digit 2 selects "no" on page 1, finishing the question set
        let res = panel.select_digit(2);
        assert!(res.is_some());
        let answers = res.unwrap();
        assert_eq!(answers.len(), 2);
        assert_eq!(answers[0].value, "opt_a");
        assert_eq!(answers[1].value, "no");
    }

    fn q(id: &str, kind: QuestionKind, required: bool) -> UserInputQuestion {
        UserInputQuestion {
            id: id.into(),
            kind,
            prompt: "?".into(),
            options: vec![],
            required,
            tool: None,
            multi: false,
            other: false,
        }
    }

    #[test]
    fn text_questions_and_escape_rules() {
        let mut panel = QuestionPanelState::new(
            "b".into(),
            vec![
                q("q1", QuestionKind::Input, true),
                q("q2", QuestionKind::Editor, false),
            ],
        );
        assert!(panel.needs_text());
        // Required: Escape does nothing.
        assert!(panel.escape().is_none());
        assert_eq!(panel.current_page, 0);
        assert!(panel.submit_text("name").is_none());
        assert_eq!(panel.current_page, 1);
        // Optional: Escape skips with an empty answer and completes.
        let answers = panel.escape().unwrap();
        assert_eq!(answers.len(), 2);
        assert_eq!(answers[0].value, "name");
        assert_eq!(answers[1].value, "");
    }

    #[test]
    fn model_list_parsing_and_effort_coercion() {
        let res = serde_json::json!({
            "models": [
                {"id": "a", "name": "Model A", "reasoning_levels": ["low", "high"]},
                {"id": "b", "name": "Model B", "reasoning_levels": []},
                {"id": "antigravity/gemini-3-8-flash", "name": "gemini-3.8-flash"},
                {"id": "minimax/MiniMax-M3.1-Flash", "name": ""}
            ],
            "default_model": "b"
        });
        let list = ModelList::parse(&res);
        assert_eq!(list.models.len(), 4);
        assert_eq!(list.models[2].name, "Gemini 3.8 Flash");
        assert_eq!(list.models[3].name, "MiniMax M3.1 Flash");
        assert_eq!(list.initial_model(), "b");
        assert_eq!(list.levels("a"), ["low".to_string(), "high".to_string()]);
        assert!(list.levels("b").is_empty());
        assert_eq!(list.coerce_effort("a", "high"), "high");
        assert_eq!(list.coerce_effort("b", "high"), "");

        let hs = HarnessOption::parse_list(&[
            serde_json::json!({"id": "mock", "display_name": "Mock", "installed": false}),
            serde_json::json!({"id": "keel", "display_name": "Keel", "installed": true}),
        ]);
        assert_eq!(HarnessOption::default_id(&hs).as_deref(), Some("keel"));
    }
}

use gpui::{Context, Window};
use keel_proto::ChatStatus;

use super::KeelWindow;

impl KeelWindow {
    pub(super) fn invalidate_suggestion(&mut self) {
        self.suggestion_revision += 1;
        self.suggestion_key = None;
        self.prompt_suggestion = None;
        self.suggestion_task = None;
    }

    pub(super) fn sync_prompt_suggestion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ready = self.current_chat.is_some()
            && self.chat_stream.is_some()
            && !self.messages.is_empty()
            && !self.awaiting_first_output
            && matches!(
                self.active_status(),
                Some(ChatStatus::Idle | ChatStatus::Aborted)
            );
        if !ready {
            if self.suggestion_key.is_some() {
                self.invalidate_suggestion();
            }
        } else if self.suggestion_key.is_none()
            && self.composer.read(cx).value().is_empty()
            && !self.settings_open
            && !self.gitlab_open
            && self.subagent_open.is_none()
            && let (Some(client), Some(chat)) = (self.client.clone(), self.current_chat.clone())
        {
            let (generation, revision) = (self.chat_gen, self.suggestion_revision);
            self.suggestion_key = Some(revision);
            self.suggestion_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(400))
                    .await;
                let result = client
                    .call("suggest_prompt", serde_json::json!({ "chat_id": chat }))
                    .await;
                let _ = this.update(cx, |this, cx| {
                    if this.chat_gen != generation
                        || this.suggestion_key != Some(revision)
                        || this.awaiting_first_output
                        || !matches!(
                            this.active_status(),
                            Some(ChatStatus::Idle | ChatStatus::Aborted)
                        )
                    {
                        return;
                    }
                    match result {
                        Ok(body) => {
                            this.prompt_suggestion = body["suggestion"].as_str().map(str::to_string);
                            this.prompt_suggestion_at = Some(std::time::Instant::now());
                        }
                        Err(err) => tracing::info!("prompt suggestion unavailable: {err}"),
                    }
                    cx.notify();
                });
            }));
        }
        // The suggestion shows in the composer's own ghost-text layer (one
        // line, under the shortcut pill), so the native placeholder steps aside.
        let placeholder = if self.prompt_suggestion.is_some() {
            ""
        } else {
            "Do anything…"
        };
        if self.composer_placeholder != placeholder {
            self.composer_placeholder = placeholder.to_string();
            self.composer.update(cx, |input, cx| {
                input.set_placeholder(placeholder, window, cx)
            });
        }
    }

    pub(super) fn can_accept_suggestion(&self, cx: &Context<Self>) -> bool {
        self.prompt_suggestion.is_some()
            && self.current_chat == self.state.active_tab
            && matches!(
                self.active_status(),
                Some(ChatStatus::Idle | ChatStatus::Aborted)
            )
            && !self.awaiting_first_output
            && !self.settings_open
            && !self.gitlab_open
            && self.subagent_open.is_none()
            && self.composer.read(cx).value().is_empty()
            && self.pending_images.is_empty()
            && self.pending_refs.is_empty()
            && self.attaching == 0
    }

    pub(super) fn accept_suggestion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_accept_suggestion(cx) {
            return;
        }
        let Some(text) = self.prompt_suggestion.clone() else {
            return;
        };
        let end = text.len();
        self.composer.update(cx, |input, cx| {
            input.set_value(text, window, cx);
            input.set_selected_range(end..end, cx);
            input.focus(window, cx);
        });
        self.on_composer_changed(cx);
    }
}

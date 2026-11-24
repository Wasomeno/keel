//! Settings → Accounts: model providers, OAuth / device sign-in flows,
//! inline masked API key editor, and two-click disconnect confirmation.

use gpui::{AnyElement, ClickEvent, Context, ElementId, IntoElement, div, prelude::*};
use keel_theme::Tokens;

use super::super::KeelWindow;
use super::controls::{
    ButtonKind, button, card_group_header, icon_button, settings_card, status_pill,
};
use super::{Account, ApiKeyEditState, HarnessStatus, SignInUi};
use crate::icons::Icon;
use crate::style::{INPUT_H, KeelStyled, MONO, c, r_md, zpx};

impl KeelWindow {
    /// Keel's Pi and which providers it is signed in to.
    pub(in crate::view) fn load_agent_status(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let res = client
                .call(
                    "harness_status",
                    serde_json::json!({ "check_sign_in": true }),
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                match res {
                    Ok(v) => {
                        let rows = v["harnesses"]
                            .as_array()
                            .map(|r| r.iter().map(HarnessStatus::parse).collect());
                        this.harness_status = Some(rows.unwrap_or_default());
                    }
                    Err(err) => this.report("couldn't check the agents", err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn start_sign_in(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.sign_in = Some((
            id.clone(),
            SignInUi {
                state: "starting".into(),
                ..Default::default()
            },
        ));
        cx.notify();
        cx.spawn(async move |this, cx| {
            if let Err(err) = client
                .call("sign_in", serde_json::json!({ "provider": id }))
                .await
            {
                let _ = this.update(cx, |this, cx| {
                    this.sign_in = Some((
                        id.clone(),
                        SignInUi {
                            state: "failed".into(),
                            message: err,
                            ..Default::default()
                        },
                    ));
                    cx.notify();
                });
                return;
            }
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(500))
                    .await;
                let Ok(v) = client
                    .call("sign_in_status", serde_json::json!({ "provider": id }))
                    .await
                else {
                    continue;
                };
                let next = SignInUi::parse(&v);
                let over = this
                    .update(cx, |this, cx| {
                        let Some(last) = this
                            .sign_in
                            .as_ref()
                            .filter(|(p, _)| *p == id)
                            .map(|(_, s)| s.clone())
                        else {
                            return true;
                        };
                        let fresh = last.state != next.state || last.url != next.url;
                        if fresh && next.state == "device" {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                next.code.clone(),
                            ));
                        }
                        if fresh
                            && matches!(next.state.as_str(), "browser" | "device")
                            && !next.url.is_empty()
                        {
                            cx.open_url(&next.url);
                        }
                        let over = match next.state.as_str() {
                            "done" => {
                                this.switch_gen += 1;
                                this.auth_landed = Some((id.clone(), this.switch_gen));
                                this.sign_in = None;
                                this.load_agent_status(cx);
                                this.refresh_models(cx);
                                true
                            }
                            "idle" => {
                                this.sign_in = None;
                                true
                            }
                            state => {
                                let failed = state == "failed";
                                this.sign_in = Some((id.clone(), next));
                                failed
                            }
                        };
                        cx.notify();
                        over
                    })
                    .unwrap_or(true);
                if over {
                    break;
                }
            }
        })
        .detach();
    }

    fn refresh_models(&mut self, cx: &mut Context<Self>) {
        self.models.clear();
        self.models_ready_at.clear();
        self.models_error.clear();
        let harness = self.selected_harness.clone();
        if !harness.is_empty() {
            self.ensure_models(harness, cx);
        }
    }

    fn cancel_sign_in(&mut self, cx: &mut Context<Self>) {
        let Some((id, _)) = self.sign_in.take() else {
            return;
        };
        if let Some(client) = self.client.clone() {
            cx.spawn(async move |_, _| {
                let _ = client
                    .call("sign_in_cancel", serde_json::json!({ "provider": id }))
                    .await;
            })
            .detach();
        }
        cx.notify();
    }

    fn bump_account_state(&mut self, provider: &str) {
        self.account_state_seq
            .entry(provider.to_string())
            .and_modify(|n| *n += 1)
            .or_insert(1);
    }

    pub(in crate::view) fn open_api_key_edit(
        &mut self,
        provider: String,
        is_update: bool,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((prev, _)) = self.account_confirm.take() {
            self.bump_account_state(&prev);
        }
        self.api_key_closing = None;
        self.switch_gen += 1;
        let seq = self.switch_gen;
        self.api_key_edit = Some(ApiKeyEditState {
            provider,
            is_update,
            saving: false,
            revealed: false,
            error: None,
            seq,
        });
        let placeholder = if is_update {
            "Enter new API key"
        } else {
            "Enter API key"
        };
        self.api_key_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.set_masked(true, window, cx);
            input.set_placeholder(placeholder, window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    pub(in crate::view) fn cancel_api_key_edit(
        &mut self,
        window: Option<&mut gpui::Window>,
        cx: &mut Context<Self>,
    ) {
        let taken = self.api_key_edit.take();
        if let Some(window) = window {
            self.api_key_input.update(cx, |input, cx| {
                input.set_value("", window, cx);
                input.set_masked(true, window, cx);
            });
        }
        if let Some(edit) = taken
            && !self.reduced()
        {
            let provider = edit.provider;
            let seq = edit.seq;
            self.api_key_closing = Some((provider.clone(), seq));
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(160))
                    .await;
                let _ = this.update(cx, |this, cx| {
                    if this.api_key_closing.as_ref() == Some(&(provider.clone(), seq)) {
                        this.api_key_closing = None;
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        cx.notify();
    }

    fn toggle_api_key_revealed(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let Some(edit) = &mut self.api_key_edit else {
            return;
        };
        edit.revealed = !edit.revealed;
        let masked = !edit.revealed;
        self.api_key_input
            .update(cx, |input, cx| input.set_masked(masked, window, cx));
        cx.notify();
    }

    pub(in crate::view) fn save_api_key(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let Some(edit) = &mut self.api_key_edit else {
            return;
        };
        if edit.saving {
            return;
        }
        let provider = edit.provider.clone();
        let api_key = self.api_key_input.read(cx).value().trim().to_string();
        if api_key.is_empty() {
            edit.error = Some("Paste an API key first".into());
            cx.notify();
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        edit.saving = true;
        edit.error = None;
        self.api_key_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.set_masked(true, window, cx);
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = client
                .call(
                    "set_provider_api_key",
                    serde_json::json!({ "provider": provider, "api_key": api_key }),
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                match res {
                    Ok(_) => {
                        this.api_key_edit = None;
                        this.api_key_closing = None;
                        this.switch_gen += 1;
                        this.auth_landed = Some((provider.clone(), this.switch_gen));
                        this.bump_account_state(&provider);
                        this.load_agent_status(cx);
                        this.refresh_models(cx);
                    }
                    Err(err) => {
                        if let Some(edit) = &mut this.api_key_edit
                            && edit.provider == provider
                        {
                            edit.saving = false;
                            edit.error = Some(err);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn arm_account_confirm(&mut self, provider: String, cx: &mut Context<Self>) {
        if self.account_busy.as_deref() == Some(provider.as_str()) {
            return;
        }
        if let Some((prev, _)) = self.account_confirm.take()
            && prev != provider
        {
            self.bump_account_state(&prev);
        }
        if let Some(edit) = self.api_key_edit.take() {
            self.api_key_closing = None;
            let _ = edit;
        }
        self.account_confirm_gen += 1;
        let seq = self.account_confirm_gen;
        self.account_confirm = Some((provider.clone(), seq));
        self.bump_account_state(&provider);
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(5))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this
                    .account_confirm
                    .as_ref()
                    .is_some_and(|(_, g)| *g == seq)
                {
                    this.cancel_account_confirm(cx);
                }
            });
        })
        .detach();
    }

    pub(in crate::view) fn cancel_account_confirm(&mut self, cx: &mut Context<Self>) {
        let Some((provider, _)) = self.account_confirm.take() else {
            return;
        };
        self.bump_account_state(&provider);
        cx.notify();
    }

    fn confirm_account_disconnect(&mut self, provider: String, cx: &mut Context<Self>) {
        self.account_confirm = None;
        self.bump_account_state(&provider);
        self.disconnect_provider(provider, cx);
    }

    fn disconnect_provider(&mut self, provider: String, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.account_busy = Some(provider.clone());
        if self
            .auth_landed
            .as_ref()
            .is_some_and(|(p, _)| *p == provider)
        {
            self.auth_landed = None;
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = client
                .call(
                    "disconnect_provider",
                    serde_json::json!({ "provider": provider }),
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.account_busy.as_deref() == Some(provider.as_str()) {
                    this.account_busy = None;
                    this.bump_account_state(&provider);
                }
                match res {
                    Ok(_) => {
                        this.load_agent_status(cx);
                        this.refresh_models(cx);
                    }
                    Err(err) => this.report("disconnect failed", err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn pi_missing(&self) -> bool {
        self.harness_status
            .as_ref()
            .is_some_and(|rows| rows.first().is_some_and(|row| !row.installed))
    }

    pub(super) fn render_accounts_tab(&mut self, t: Tokens, cx: &mut Context<Self>) -> gpui::Div {
        let Some(h) = self
            .harness_status
            .clone()
            .unwrap_or_default()
            .into_iter()
            .next()
        else {
            return div().flex().flex_col().child(card_group_header(
                "Model providers",
                Some("Sign-in opens in your browser and is kept for Keel only, separate from your own Pi."),
                None,
                t,
            ));
        };

        if !h.installed {
            return div()
                .flex()
                .flex_col()
                .child(card_group_header("Model providers", None, None, t))
                .child(settings_card(
                    vec![
                        div()
                            .py(zpx(12.))
                            .flex()
                            .flex_col()
                            .gap(zpx(4.))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(zpx(8.))
                                    .t_body()
                                    .text_color(c(t.danger))
                                    .child(Icon::Alert.el(zpx(14.)))
                                    .child("Keel's bundled Pi is missing"),
                            )
                            .child(div().t_small().text_color(c(t.text_muted)).child(
                                "Run scripts/fetch-pi.sh or set PI_EXECUTABLE, then restart Keel.",
                            )),
                    ],
                    t,
                ));
        }

        let version_pill = (!h.version.is_empty())
            .then(|| status_pill(format!("Pi {}", h.version), t.text_faint, t).into_any_element());

        let rows: Vec<gpui::Div> = h
            .accounts
            .clone()
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(ix, account)| self.render_account_row(ix, account, t, cx))
            .collect();

        div()
            .flex()
            .flex_col()
            .child(card_group_header(
                "Model providers",
                Some("Sign-in opens in your browser and is stored for Keel only, separate from your own Pi."),
                version_pill,
                t,
            ))
            .child(settings_card(rows, t))
    }

    fn render_account_row(
        &self,
        _ix: usize,
        account: Account,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        use gpui::AnimationExt as _;

        let reduced = self.reduced();
        let active = self
            .sign_in
            .as_ref()
            .filter(|(id, _)| *id == account.id)
            .map(|(_, s)| s.clone());
        let editing = self
            .api_key_edit
            .as_ref()
            .filter(|e| e.provider == account.id)
            .cloned();
        let closing_seq = self
            .api_key_closing
            .as_ref()
            .filter(|(id, _)| *id == account.id && editing.is_none())
            .map(|(_, seq)| *seq);
        let confirming = self
            .account_confirm
            .as_ref()
            .is_some_and(|(id, _)| *id == account.id);
        let state_seq = self
            .account_state_seq
            .get(&account.id)
            .copied()
            .unwrap_or(0);
        let busy = self.account_busy.as_deref() == Some(account.id.as_str());

        // Quiet actions are ghost, the outlined sign-in is secondary, and the
        // second click that actually disconnects is the danger confirm.
        let small_button = |id: String, label: &'static str, primary: bool| {
            button(
                id,
                label,
                if primary {
                    ButtonKind::Secondary
                } else {
                    ButtonKind::Ghost
                },
                true,
                t,
            )
        };
        let outline_button = |id: String, label: &'static str, _emphasis: bool| {
            button(id, label, ButtonKind::Secondary, true, t)
        };
        let danger_outline_button =
            |id: String, label: &'static str| button(id, label, ButtonKind::Ghost, true, t);
        let confirm_pill =
            |id: String, label: &'static str| button(id, label, ButtonKind::Danger, true, t);

        // Keep subline only for active sign-in / error states or concise hint
        let mut subline: Option<(String, u32)> = None;

        let state: AnyElement = if editing.is_some() {
            if account.signed_in {
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(6.))
                    .t_small()
                    .text_color(c(t.text_muted))
                    .child(Icon::Check.el(zpx(13.)).text_color(c(t.success)))
                    .child(account.credential())
                    .into_any_element()
            } else {
                div().into_any_element()
            }
        } else if account.signed_in && active.is_none() {
            let rhs = if busy {
                let spinner = crate::motion::spin(
                    Icon::Spinner.el(zpx(12.)).text_color(c(t.text_faint)),
                    ElementId::Name(format!("acct-busy-{}", account.id).into()),
                    reduced,
                );
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(6.))
                    .px(zpx(8.))
                    .py(zpx(4.))
                    .t_small()
                    .text_color(c(t.text_muted))
                    .child(spinner)
                    .child(if account.detail == "oauth" {
                        "Disconnecting…"
                    } else {
                        "Removing…"
                    })
            } else if confirming {
                let id_confirm = account.id.clone();
                let is_oauth = account.detail == "oauth";
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(6.))
                    .child(
                        div()
                            .mr(zpx(4.))
                            .t_small()
                            .text_color(c(t.text_muted))
                            .child(if is_oauth {
                                "Disconnect?"
                            } else {
                                "Remove key?"
                            }),
                    )
                    .child(
                        outline_button(format!("acct-cancel-{}", account.id), "Cancel", false)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.cancel_account_confirm(cx)
                            })),
                    )
                    .child(
                        confirm_pill(
                            format!("acct-confirm-{}", account.id),
                            if is_oauth { "Disconnect" } else { "Remove" },
                        )
                        .on_click(cx.listener(
                            move |this, _: &ClickEvent, _, cx| {
                                this.confirm_account_disconnect(id_confirm.clone(), cx)
                            },
                        )),
                    )
            } else {
                let status = div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(6.))
                    .t_small()
                    .text_color(c(t.text_muted))
                    .child(Icon::Check.el(zpx(13.)).text_color(c(t.success)))
                    .child(account.credential());
                let status_el = match &self.auth_landed {
                    Some((id, n)) if *id == account.id => crate::motion::appear(
                        status,
                        ElementId::Name(format!("auth-landed-{id}-{n}").into()),
                        crate::motion::SLOW,
                        4.,
                        reduced,
                    )
                    .into_any_element(),
                    _ => status.into_any_element(),
                };
                let separator = || div().w(gpui::px(1.)).h(zpx(12.)).bg(c(t.border));
                if account.detail == "oauth" {
                    let id = account.id.clone();
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(zpx(8.))
                        .child(status_el)
                        .child(separator())
                        .child(
                            danger_outline_button(
                                format!("acct-disconnect-{}", account.id),
                                "Disconnect",
                            )
                            .on_click(cx.listener(
                                move |this, _: &ClickEvent, _, cx| {
                                    this.arm_account_confirm(id.clone(), cx)
                                },
                            )),
                        )
                } else if account.detail == "api_key" {
                    let id_edit = account.id.clone();
                    let id_rm = account.id.clone();
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(zpx(8.))
                        .child(status_el)
                        .child(separator())
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(zpx(6.))
                                .child(
                                    outline_button(
                                        format!("apikey-edit-{}", account.id),
                                        "Edit",
                                        false,
                                    )
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, window, cx| {
                                            this.open_api_key_edit(
                                                id_edit.clone(),
                                                true,
                                                window,
                                                cx,
                                            )
                                        },
                                    )),
                                )
                                .child(
                                    danger_outline_button(
                                        format!("apikey-remove-{}", account.id),
                                        "Remove",
                                    )
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, _, cx| {
                                            this.arm_account_confirm(id_rm.clone(), cx)
                                        },
                                    )),
                                ),
                        )
                } else {
                    div().flex().flex_row().items_center().child(status_el)
                }
            };

            if state_seq > 0 {
                let dur = if reduced {
                    std::time::Duration::from_millis(90)
                } else if confirming {
                    crate::motion::BASE
                } else {
                    crate::motion::FAST
                };
                let ease = if confirming {
                    crate::motion::ease_out_strong
                } else {
                    crate::motion::ease_out
                };
                let shift = if reduced {
                    0.
                } else if confirming {
                    4.
                } else {
                    -3.
                };
                rhs.with_animation(
                    ElementId::Name(format!("acct-rhs-{}-{state_seq}", account.id).into()),
                    gpui::Animation::new(dur).with_easing(ease),
                    move |el, delta| el.opacity(delta).relative().left(zpx(shift * (1. - delta))),
                )
                .into_any_element()
            } else {
                rhs.into_any_element()
            }
        } else if let Some(sign_in) = active.filter(|s| s.state != "failed") {
            let spinner = crate::motion::spin(
                Icon::Spinner.el(zpx(12.)).text_color(c(t.text_faint)),
                ElementId::Name(format!("signin-spin-{}", account.id).into()),
                reduced,
            );
            subline = Some((
                match sign_in.state.as_str() {
                    "browser" => "Finish signing in in your browser".to_string(),
                    "device" => "Code copied — confirm on the page that opened".to_string(),
                    "working" if !sign_in.message.is_empty() => {
                        sign_in.message.trim_end_matches('.').to_string()
                    }
                    _ => "Starting sign-in".to_string(),
                },
                t.text_muted,
            ));
            let reopen = (!sign_in.url.is_empty()).then(|| {
                let url = sign_in.url.clone();
                small_button(format!("signin-open-{}", account.id), "Open again", false)
                    .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| cx.open_url(&url)))
            });
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(4.))
                .when(sign_in.state == "device", |d| {
                    d.child(
                        div()
                            .mr(zpx(4.))
                            .px(zpx(8.))
                            .py(zpx(2.))
                            .rounded(r_md())
                            .bg(c(t.hover))
                            .font_family(MONO)
                            .t_small()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(c(t.text))
                            .child(sign_in.code.clone()),
                    )
                })
                .when(sign_in.state != "device", |d| {
                    d.child(div().mr(zpx(4.)).child(spinner))
                })
                .children(reopen)
                .child(
                    small_button(format!("signin-cancel-{}", account.id), "Cancel", false)
                        .on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.cancel_sign_in(cx)),
                        ),
                )
                .into_any_element()
        } else {
            if let Some((_, failed)) = self
                .sign_in
                .as_ref()
                .filter(|(id, s)| *id == account.id && s.state == "failed")
            {
                let why = failed.message.trim().trim_end_matches('.');
                subline = Some((
                    if why.is_empty() {
                        "Sign-in failed".to_string()
                    } else {
                        format!("Sign-in failed: {why}")
                    },
                    t.danger,
                ));
            }
            let id = account.id.clone();
            if account.supports_oauth {
                let id_key = account.id.clone();
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(4.))
                    .child(
                        small_button(format!("apikey-add-{}", account.id), "API key", false)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.open_api_key_edit(id_key.clone(), false, window, cx)
                            })),
                    )
                    .child(
                        small_button(format!("signin-{}", account.id), "Sign in", true).on_click(
                            cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.start_sign_in(id.clone(), cx)
                            }),
                        ),
                    )
                    .into_any_element()
            } else {
                small_button(format!("apikey-add-{}", account.id), "Add API key", true)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.open_api_key_edit(id.clone(), false, window, cx)
                    }))
                    .into_any_element()
            }
        };

        let hint_tip = (!account.hint.is_empty()).then(|| account.hint.clone());
        let provider_id = account.id.clone();

        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(12.))
            .child(
                div()
                    .size(zpx(28.))
                    .flex_none()
                    .rounded(r_md())
                    .bg(c(t.input))
                    .border_1()
                    .border_color(c(t.border))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(c(if account.signed_in {
                        t.text
                    } else {
                        t.text_muted
                    }))
                    .child(Icon::provider(&account.id).el(zpx(15.))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(zpx(6.))
                            .child(
                                div()
                                    .t_body()
                                    .text_color(c(t.text))
                                    .child(account.name.clone()),
                            )
                            .when_some(hint_tip, |d, tip| {
                                d.child(
                                    div()
                                        .id(ElementId::Name(
                                            format!("acct-hint-{provider_id}").into(),
                                        ))
                                        .flex_none()
                                        .size(zpx(14.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .t_caption()
                                        .text_color(c(t.text_faint))
                                        .hover(|s| s.text_color(c(t.text)))
                                        .cursor_default()
                                        .child("?")
                                        .tooltip(move |_, cx| {
                                            cx.new(|_| {
                                                super::super::tooltip::InfoTip::new(tip.clone(), t)
                                            })
                                            .into()
                                        }),
                                )
                            }),
                    )
                    .when_some(subline, |d, (text, color)| {
                        d.child(
                            div()
                                .t_small()
                                .text_color(c(color))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(text),
                        )
                    }),
            )
            .child(state);

        let editor = editing.map(|edit| {
            let has_text = !self.api_key_input.read(cx).value().trim().is_empty();
            let has_error = edit.error.is_some();
            let eye = icon_button(
                format!("apikey-eye-{}", account.id),
                if edit.revealed {
                    Icon::EyeOff
                } else {
                    Icon::Eye
                },
                t,
            )
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.toggle_api_key_revealed(window, cx)
            }));
            let input_box = div()
                .flex_1()
                .min_w_0()
                .h(zpx(INPUT_H))
                .pl(zpx(8.))
                .pr(zpx(4.))
                .rounded(r_md())
                .bg(c(t.input))
                .border_1()
                .border_color(c(if has_error { t.danger } else { t.border }))
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(4.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_family(MONO)
                        .t_small()
                        .child(self.api_key_input.clone()),
                )
                .child(eye);
            let save_btn = button(
                format!("apikey-save-{}", account.id),
                if edit.saving { "Saving…" } else { "Save" },
                ButtonKind::Primary,
                !edit.saving && has_text,
                t,
            )
            .on_click(
                cx.listener(|this, _: &ClickEvent, window, cx| this.save_api_key(window, cx)),
            );
            let cancel_btn =
                outline_button(format!("apikey-cancel-{}", account.id), "Cancel", false).on_click(
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.cancel_api_key_edit(Some(window), cx)
                    }),
                );
            let panel = div()
                .pl(zpx(40.))
                .pt(zpx(10.))
                .flex()
                .flex_col()
                .gap(zpx(6.))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(zpx(8.))
                        .child(input_box)
                        .child(cancel_btn)
                        .child(save_btn),
                )
                .when_some(edit.error, |d, err| {
                    d.child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(zpx(6.))
                            .t_small()
                            .text_color(c(t.danger))
                            .child(Icon::Alert.el(zpx(12.)))
                            .child(err),
                    )
                });
            let open_dur = if reduced {
                std::time::Duration::from_millis(90)
            } else {
                std::time::Duration::from_millis(200)
            };
            let animated_panel = panel.with_animation(
                ElementId::Name(format!("apikey-panel-in-{}-{}", account.id, edit.seq).into()),
                gpui::Animation::new(open_dur).with_easing(crate::motion::ease_out_strong),
                move |el, delta| {
                    let vis = crate::motion::ease_out((delta / 0.7).min(1.));
                    let dy = if reduced { 0. } else { 4. * (1. - delta) };
                    el.opacity(vis).relative().top(zpx(dy))
                },
            );
            div()
                .child(animated_panel)
                .with_animation(
                    ElementId::Name(format!("apikey-panel-{}-{}", account.id, edit.seq).into()),
                    gpui::Animation::new(open_dur).with_easing(crate::motion::ease_out_strong),
                    move |el, delta| {
                        el.when(!reduced && delta < 1.0, |d| {
                            d.h(zpx(42. * delta)).overflow_hidden()
                        })
                    },
                )
                .into_any_element()
        });

        let closing_editor = closing_seq.map(|seq| {
            div()
                .with_animation(
                    ElementId::Name(format!("apikey-close-{}-{seq}", account.id).into()),
                    gpui::Animation::new(std::time::Duration::from_millis(150))
                        .with_easing(crate::motion::ease_out),
                    move |el, delta| {
                        let inv = 1. - delta;
                        let vis = crate::motion::ease_out((inv / 0.75).min(1.));
                        el.h(zpx(44. * inv)).overflow_hidden().opacity(vis)
                    },
                )
                .into_any_element()
        });

        div()
            .flex()
            .flex_col()
            .py(zpx(10.))
            .child(header)
            .children(editor)
            .children(closing_editor)
    }
}

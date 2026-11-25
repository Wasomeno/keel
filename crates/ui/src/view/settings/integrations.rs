//! Settings → Integrations: Browser allowed sites and connected external
//! services (GitLab, Dotify), using compact cards.

use gpui::{ClickEvent, Context, IntoElement, div, prelude::*};
use keel_theme::Tokens;

use super::super::KeelWindow;
use super::ConnectionRow;
use super::controls::{ButtonKind, button, card_group_header, field, settings_card};
use crate::icons::Icon;
use crate::style::{INPUT_H, KeelStyled, ROW_MIN_H, c, r_md, zpx};

impl KeelWindow {
    pub(in crate::view) fn load_connections_pub(&mut self, cx: &mut Context<Self>) {
        self.load_connections(cx);
    }

    pub(super) fn load_connections(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            if let Ok(v) = client.call("list_connections", serde_json::json!({})).await {
                let _ = this.update(cx, |this, cx| {
                    this.connections = v["connections"]
                        .as_array()
                        .map(|r| r.iter().map(ConnectionRow::parse).collect())
                        .unwrap_or_default();
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn connect_gitlab(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let host = self.gitlab_host.read(cx).value().trim().to_string();
        let host = if host.is_empty() {
            "https://gitlab.com".to_string()
        } else {
            host
        };
        let token = self.gitlab_token.read(cx).value().trim().to_string();
        let Some(client) = self.client.clone() else {
            return;
        };
        self.connect_busy = true;
        self.connect_error = None;
        self.gitlab_token
            .update(cx, |input, cx| input.set_value("", window, cx));
        cx.spawn(async move |this, cx| {
            let res = client
                .call(
                    "connect_gitlab",
                    serde_json::json!({ "host": host, "token": token }),
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                this.connect_busy = false;
                match res {
                    Ok(_) => {
                        this.settings_gitlab_form_open = false;
                        this.load_connections(cx);
                    }
                    Err(err) => this.connect_error = Some(err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn disconnect(&mut self, kind: String, host: String, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let res = client
                .call(
                    "disconnect",
                    serde_json::json!({ "kind": kind, "host": host }),
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                match res {
                    Ok(_) => this.load_connections(cx),
                    Err(err) => this.report("disconnect failed", err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn render_integrations_tab(
        &mut self,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap(zpx(24.))
            .child(self.render_browser_card(t, cx))
            .child(self.render_connections_card(t, cx))
    }

    fn render_browser_card(&mut self, t: Tokens, cx: &mut Context<Self>) -> gpui::Div {
        let saved = self.state.ui_settings.browser_allowed_sites.clone();
        let typed =
            crate::browser::agent::parse_sites(&self.browser_sites.read(cx).value().to_string());
        let dirty = typed != saved;

        let status = div()
            .flex_none()
            .t_small()
            .text_color(c(t.text_muted))
            .child(if saved.is_empty() {
                "Any site".to_string()
            } else if saved.len() == 1 {
                format!("Only {}", saved[0])
            } else {
                format!("{} hosts", saved.len())
            });

        let row = div()
            .flex()
            .flex_col()
            .gap(zpx(8.))
            .py(zpx(12.))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap(zpx(8.))
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(zpx(2.))
                            .child(
                                div()
                                    .t_body()
                                    .text_color(c(t.text))
                                    .child("Allowed sites"),
                            )
                            .child(
                                div()
                                    .t_small()
                                    .text_color(c(t.text_muted))
                                    .child("Hosts agents may open in the browser pane."),
                            ),
                    )
                    .child(status),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h(zpx(INPUT_H))
                            .px(zpx(8.))
                            .flex()
                            .items_center()
                            .rounded(r_md())
                            .border_1()
                            .border_color(c(t.border))
                            .t_body()
                            .child(self.browser_sites.clone()),
                    )
                    .child(
                        button(
                            "save-browser-sites-btn",
                            "Save",
                            ButtonKind::Primary,
                            dirty,
                            t,
                        )
                        .when(dirty, |b| {
                            b.on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.save_browser_sites(cx)
                            }))
                        }),
                    ),
            );

        div()
            .flex()
            .flex_col()
            .child(card_group_header("Browser", None, None, t))
            .child(settings_card(vec![row], t))
    }

    fn render_connections_card(&mut self, t: Tokens, cx: &mut Context<Self>) -> gpui::Div {
        let busy = self.connect_busy;
        let error = self.connect_error.clone();
        let non_dotify: Vec<_> = self
            .connections
            .clone()
            .into_iter()
            .filter(|c| c.kind != "dotify")
            .collect();
        let has_conns = !non_dotify.is_empty();
        let show_form = !has_conns || self.settings_gitlab_form_open;

        let mut rows: Vec<gpui::Div> = non_dotify
            .into_iter()
            .map(|conn| {
                let (kind, host) = (conn.kind.clone(), conn.host.clone());
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .min_h(zpx(ROW_MIN_H))
                    .py(zpx(10.))
                    .gap(zpx(16.))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(zpx(8.))
                            .min_w_0()
                            .child(
                                div()
                                    .size(zpx(7.))
                                    .flex_none()
                                    .rounded_full()
                                    .bg(c(t.success)),
                            )
                            .child(div().t_body().text_color(c(t.text)).child(
                                if conn.kind == "gitlab" {
                                    "GitLab".to_string()
                                } else {
                                    conn.kind.clone()
                                },
                            ))
                            .child(div().t_small().text_color(c(t.text_muted)).child(format!(
                                "{} · @{}",
                                conn.host.trim_start_matches("https://"),
                                conn.username
                            )))
                            .when(conn.name != conn.username, |d| {
                                d.child(
                                    div()
                                        .t_small()
                                        .text_color(c(t.text_faint))
                                        .child(conn.name.clone()),
                                )
                            }),
                    )
                    .child(
                        button(
                            format!("disconnect-{}", conn.host),
                            "Disconnect",
                            ButtonKind::Ghost,
                            true,
                            t,
                        )
                        .on_click(cx.listener(
                            move |this, _: &ClickEvent, _, cx| {
                                this.disconnect(kind.clone(), host.clone(), cx)
                            },
                        )),
                    )
            })
            .collect();

        if show_form {
            let gitlab_form = div()
                .flex()
                .flex_col()
                .gap(zpx(12.))
                .py(zpx(12.))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(zpx(2.))
                                .child(
                                    div()
                                        .t_body()
                                        .text_color(c(t.text))
                                        .child("Connect GitLab"),
                                )
                                .child(div().t_small().text_color(c(t.text_muted)).child(
                                    "gitlab.com or self-hosted. Use a token with the read_api scope.",
                                )),
                        )
                        .when(has_conns, |d| {
                            d.child(
                                button(
                                    "gitlab-form-cancel",
                                    "Cancel",
                                    ButtonKind::Ghost,
                                    true,
                                    t,
                                )
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.settings_gitlab_form_open = false;
                                        this.connect_error = None;
                                        cx.notify();
                                    })),
                            )
                        }),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(zpx(8.))
                        .child(field(
                            t,
                            "Address",
                            self.gitlab_host.clone().into_any_element(),
                        ))
                        .child(field(
                            t,
                            "Token",
                            self.gitlab_token.clone().into_any_element(),
                        )),
                )
                .when_some(error, |d, err| {
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
                })
                .child(
                    div().flex().flex_row().justify_end().child(
                        button(
                            "connect-gitlab-btn",
                            if busy { "Checking…" } else { "Connect" },
                            ButtonKind::Primary,
                            !busy,
                            t,
                        )
                        .on_click(cx.listener(
                            move |this, _: &ClickEvent, window, cx| {
                                if !this.connect_busy {
                                    this.connect_gitlab(window, cx);
                                }
                            },
                        )),
                    ),
                );
            rows.push(gitlab_form);
        } else {
            rows.push(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .py(zpx(10.))
                    .child(
                        div()
                            .t_small()
                            .text_color(c(t.text_muted))
                            .child("Add another GitLab instance"),
                    )
                    .child(
                        button(
                            "gitlab-form-open",
                            "+ Connect GitLab",
                            ButtonKind::Ghost,
                            true,
                            t,
                        )
                        .on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| {
                                this.settings_gitlab_form_open = true;
                                cx.notify();
                            },
                        )),
                    ),
            );
        }

        div()
            .flex()
            .flex_col()
            .child(card_group_header(
                "Connections",
                Some("Tokens stay on this Mac, readable only by you."),
                None,
                t,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(zpx(12.))
                    .when(self.dotify_visible(), |s| {
                        s.child(self.render_dotify_settings_card(t, cx))
                    })
                    .child(settings_card(rows, t)),
            )
    }
}

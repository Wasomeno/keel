//! Settings → Integrations: Browser allowed sites and connected external
//! services (GitLab, Dotify), using compact cards and hover info badges.

use gpui::{ClickEvent, Context, IntoElement, div, prelude::*};
use keel_theme::Tokens;

use super::super::KeelWindow;
use super::super::sidebar::filled_button;
use super::ConnectionRow;
use super::controls::{
    card_group_header, field, info_badge, settings_card, status_pill, text_button_owned,
};
use crate::icons::Icon;
use crate::style::{KeelStyled, RADIUS_MD, c, zpx};

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
            .gap_5()
            .child(self.render_browser_card(t, cx))
            .child(self.render_connections_card(t, cx))
    }

    fn render_browser_card(&mut self, t: Tokens, cx: &mut Context<Self>) -> gpui::Div {
        let saved = self.state.ui_settings.browser_allowed_sites.clone();
        let typed =
            crate::browser::agent::parse_sites(&self.browser_sites.read(cx).value().to_string());
        let dirty = typed != saved;

        let pill = if saved.is_empty() {
            status_pill("Any site", t.text_muted, t)
        } else if saved.len() == 1 {
            status_pill(format!("1 host: {}", saved[0]), t.accent, t)
        } else {
            status_pill(format!("{} hosts allowed", saved.len()), t.accent, t)
        };

        let row = div()
            .flex()
            .flex_col()
            .gap_2p5()
            .py_3()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1p5()
                            .child(
                                div()
                                    .t_body()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(c(t.text))
                                    .child("Allowed sites"),
                            )
                            .child(info_badge(
                                "info-browser-sites",
                                "Comma-separated hosts agents may open in Keel's browser pane. A host includes its subdomains; *.example.com matches subdomains only. Leave empty to allow any site. Your own browsing is never restricted.",
                                t,
                            )),
                    )
                    .child(pill),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h(zpx(32.))
                            .px_2p5()
                            .flex()
                            .items_center()
                            .rounded(RADIUS_MD)
                            .bg(c(t.input))
                            .border_1()
                            .border_color(c(t.border))
                            .t_body()
                            .child(self.browser_sites.clone()),
                    )
                    .child(
                        filled_button("save-browser-sites-btn", "Save", t.accent, t)
                            .when(!dirty, |b| b.opacity(0.45))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.save_browser_sites(cx)
                            })),
                    ),
            );

        div()
            .flex()
            .flex_col()
            .child(card_group_header(
                "BROWSER",
                Some("Controls which sites agents can load in the built-in browser pane."),
                None,
                t,
            ))
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
                    .py_2p5()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2p5()
                            .min_w_0()
                            .child(
                                div()
                                    .size(zpx(7.))
                                    .flex_none()
                                    .rounded_full()
                                    .bg(c(t.success)),
                            )
                            .child(
                                div()
                                    .t_body()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(c(t.text))
                                    .child(if conn.kind == "gitlab" {
                                        "GitLab".to_string()
                                    } else {
                                        conn.kind.clone()
                                    }),
                            )
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
                        text_button_owned(format!("disconnect-{}", conn.host), "Disconnect", t)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.disconnect(kind.clone(), host.clone(), cx)
                            })),
                    )
            })
            .collect();

        if show_form {
            let gitlab_form = div()
                .flex()
                .flex_col()
                .gap_3()
                .py_3()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_1p5()
                                .child(
                                    div()
                                        .t_body()
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(c(t.text))
                                        .child("Connect GitLab"),
                                )
                                .child(info_badge(
                                    "info-gitlab-token",
                                    "Supports gitlab.com or self-hosted instances. Create a personal access token with the read_api scope under User settings → Access tokens.",
                                    t,
                                )),
                        )
                        .when(has_conns, |d| {
                            d.child(
                                text_button_owned("gitlab-form-cancel".into(), "Cancel", t)
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
                        .gap_2()
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
                            .gap_1p5()
                            .t_small()
                            .text_color(c(t.danger))
                            .child(Icon::Alert.el(zpx(12.)))
                            .child(err),
                    )
                })
                .child(
                    div().flex().flex_row().justify_end().child(
                        filled_button(
                            "connect-gitlab-btn",
                            if busy { "Checking…" } else { "Connect" },
                            t.accent,
                            t,
                        )
                        .when(busy, |b| b.opacity(0.6))
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
                    .py_2p5()
                    .child(
                        div()
                            .t_small()
                            .text_color(c(t.text_muted))
                            .child("Add another GitLab instance"),
                    )
                    .child(
                        text_button_owned("gitlab-form-open".into(), "+ Connect GitLab", t)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.settings_gitlab_form_open = true;
                                cx.notify();
                            })),
                    ),
            );
        }

        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(card_group_header(
                "CONNECTIONS",
                Some("External services Keel can talk to. Tokens stay local on this Mac, readable only by you."),
                None,
                t,
            ))
            .when(self.dotify_visible(), |s| {
                s.child(self.render_dotify_settings_card(t, cx))
            })
            .child(settings_card(rows, t))
    }
}

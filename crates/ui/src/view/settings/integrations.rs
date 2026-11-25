//! Settings → Integrations: Browser allowed sites and connected external
//! services, using compact cards.

use gpui::{ClickEvent, Context, div, prelude::*};
use keel_theme::Tokens;

use super::super::KeelWindow;
use super::controls::{ButtonKind, button, card_group_header, settings_card};
use crate::style::{INPUT_H, KeelStyled, c, r_md, zpx};

impl KeelWindow {
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
            .when(self.dotify_visible(), |d| {
                d.child(self.render_connections_card(t, cx))
            })
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
        div()
            .flex()
            .flex_col()
            .child(card_group_header(
                "Connections",
                Some("Tokens stay on this Mac, readable only by you."),
                None,
                t,
            ))
            .child(self.render_dotify_settings_card(t, cx))
    }
}

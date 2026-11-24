//! Settings → General: Appearance, Background, and Updates in three compact,
//! scannable cards that fit in a single viewport without scrolling.

use gpui::{ClickEvent, Context, IntoElement, div, prelude::*};
use keel_proto::ThemeChoice;
use keel_theme::Tokens;

use super::super::KeelWindow;
use super::controls::{
    ButtonKind, button, card_group_header, info_badge, setting_row_info, settings_card, toggle,
};
use crate::icons::Icon;
use crate::style::{KeelStyled, c, r_md, zpx};

impl KeelWindow {
    pub(super) fn choose_background(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Use picture".into()),
        });
        let data_dir = self.state.data_dir.clone();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let result = cx
                .background_spawn(
                    async move { crate::background::import_background(&data_dir, &path) },
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(name) => {
                        this.import_error = None;
                        this.state.ui_settings.background = Some(name);
                        this.state.save_settings();
                        this.forget_hero_path();
                    }
                    Err(err) => {
                        this.import_error = Some(format!("Couldn't use that picture: {err}"))
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn reset_background(&mut self, cx: &mut Context<Self>) {
        crate::background::reset_background(&self.state.data_dir);
        self.state.ui_settings.background = None;
        self.state.save_settings();
        self.import_error = None;
        self.forget_hero_path();
        cx.notify();
    }

    fn forget_hero_path(&mut self) {
        *self.hero_path.borrow_mut() = None;
    }

    pub(super) fn render_general_tab(&mut self, t: Tokens, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap(zpx(24.))
            .child(self.render_appearance_card(t, cx))
            .child(self.render_background_card(t, cx))
            .child(self.render_updates_section(t, cx))
    }

    fn render_appearance_card(&mut self, t: Tokens, cx: &mut Context<Self>) -> gpui::Div {
        let current_theme = self.state.ui_settings.theme;
        let theme_options = [
            ("System", None),
            ("Light", Some(ThemeChoice::Light)),
            ("Dark", Some(ThemeChoice::Dark)),
        ];

        let is_reduced = self.state.effective_reduced_motion(self.os_reduced_motion);

        let rows = vec![
            setting_row_info(
                t,
                "Theme",
                None,
                self.render_segmented_control(
                    "theme",
                    &theme_options,
                    current_theme,
                    58.0,
                    t,
                    cx,
                    |this, choice, _| {
                        this.state.set_theme(choice);
                    },
                )
                .into_any_element(),
            ),
            setting_row_info(
                t,
                "Reduced motion",
                Some(
                    "Replaces sliding and spring animations with instant transitions or gentle fades.",
                ),
                toggle("settings-motion".into(), is_reduced, t)
                    .on_click(cx.listener(|this, _, _, cx| {
                        let cur = this.state.effective_reduced_motion(this.os_reduced_motion);
                        let next = !cur;
                        this.state.set_reduced_motion(Some(next));
                        this.transcript.reduced_motion = next;
                        cx.set_reduce_motion(next);
                        cx.notify();
                    }))
                    .into_any_element(),
            ),
        ];

        div()
            .flex()
            .flex_col()
            .child(card_group_header("Appearance", None, None, t))
            .child(settings_card(rows, t))
    }

    fn render_background_card(&mut self, t: Tokens, cx: &mut Context<Self>) -> gpui::Div {
        let custom = crate::background::background_path(
            &self.state.data_dir,
            self.state.ui_settings.background.as_deref(),
        );
        let dim = self.state.ui_settings.background_dim.unwrap_or(0.);
        let steps = [
            (0.0_f32, "Off"),
            (0.2, "20%"),
            (0.4, "40%"),
            (0.6, "60%"),
            (0.8, "80%"),
        ];
        let nearest = steps
            .iter()
            .min_by(|a, b| (a.0 - dim).abs().total_cmp(&(b.0 - dim).abs()))
            .map(|s| s.0)
            .unwrap_or(0.);

        let thumb = div()
            .w(zpx(96.))
            .h(zpx(54.))
            .flex_none()
            .relative()
            .overflow_hidden()
            .rounded(r_md())
            .border_1()
            .border_color(c(t.border))
            .child(
                custom
                    .clone()
                    .map_or_else(|| gpui::img(crate::icons::HERO_IMAGE), gpui::img)
                    .size_full()
                    .object_fit(gpui::ObjectFit::Cover),
            )
            .when(dim > 0., |d| {
                d.child(div().absolute().inset_0().bg(gpui::hsla(0., 0., 0., dim)))
            });

        let error = self.import_error.clone();

        let picture_row = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(zpx(16.))
            .py(zpx(10.))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(12.))
                    .min_w_0()
                    .child(thumb)
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
                                    .child(if custom.is_some() {
                                        "Custom image"
                                    } else {
                                        "Default landscape"
                                    }),
                            )
                            .child(info_badge(
                                "info-bg-formats",
                                "Shown behind the new-chat screen. Accepts PNG, JPEG, HEIC, or WebP (stored locally at up to 2560 px).",
                                t,
                            )),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(6.))
                    .flex_none()
                    .when(custom.is_some(), |d| {
                        d.child(
                            button("bg-reset-btn", "Reset", ButtonKind::Ghost, true, t).on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.reset_background(cx)
                                }),
                            ),
                        )
                    })
                    .child(
                        button("bg-choose-btn", "Choose…", ButtonKind::Primary, true, t).on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.choose_background(cx)),
                        ),
                    ),
            );

        let dim_row = setting_row_info(
            t,
            "Dim",
            Some("Darkens the background image to keep text legible."),
            self.render_segmented_control(
                "dim",
                &[
                    ("Off", 0.0_f32),
                    ("20%", 0.2),
                    ("40%", 0.4),
                    ("60%", 0.6),
                    ("80%", 0.8),
                ],
                nearest,
                42.0,
                t,
                cx,
                |this, step, _| {
                    this.state.ui_settings.background_dim = (step > 0.).then_some(step);
                    this.state.save_settings();
                },
            )
            .into_any_element(),
        );

        let glow_on = !self.state.ui_settings.chat_glow_off;
        let glow_row = setting_row_info(
            t,
            "Chat glow",
            Some("Adds a soft ambient glow of the background image beneath the composer."),
            toggle("settings-chat-glow".into(), glow_on, t)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.state.ui_settings.chat_glow_off = !this.state.ui_settings.chat_glow_off;
                    this.state.save_settings();
                    cx.notify();
                }))
                .into_any_element(),
        );

        div()
            .flex()
            .flex_col()
            .child(card_group_header("Background", None, None, t))
            .child(settings_card(vec![picture_row, dim_row, glow_row], t))
            .when_some(error, |d, err| {
                d.child(
                    div()
                        .pt(zpx(8.))
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
    }
}

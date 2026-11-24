//! Settings → Notifications: organized into three scannable cards
//! (Triggers, Alerts, Sound) with hover tooltips instead of inline prose.

use gpui::{AnyElement, ClickEvent, Context, ElementId, IntoElement, div, prelude::*};
use keel_theme::Tokens;

use super::super::KeelWindow;
use super::controls::{
    ButtonKind, button, card_group_header, render_sound_preview_btn, setting_row_info,
    settings_card, toggle,
};
use crate::hover_fade::HoverFadeExt;
use crate::icons::Icon;
use crate::style::{CONTROL_H, KeelStyled, c, r_md, r_sm, zpx};

impl KeelWindow {
    fn set_notify(&mut self, change: impl FnOnce(&mut keel_proto::NotifySettings)) {
        change(&mut self.state.ui_settings.notifications);
        self.state.save_settings();
        self.refresh_dock_badge();
    }

    fn notify_toggle(&mut self, change: impl FnOnce(&mut keel_proto::NotifySettings)) {
        self.set_notify(change);
    }

    pub(in crate::view) fn preview_sound(&mut self, cx: &mut Context<Self>) {
        let n = &self.state.ui_settings.notifications;
        if let Some(path) = crate::background::sound_path(&self.state.data_dir, &n.sound) {
            crate::platform_notify::play_sound(&path, n.volume.max(0.05));
            self.sound_preview_epoch = self.sound_preview_epoch.wrapping_add(1);
            let epoch = self.sound_preview_epoch;
            cx.notify();
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(850))
                    .await;
                let _ = this.update(cx, |this, cx| {
                    if this.sound_preview_epoch == epoch {
                        this.sound_preview_epoch = 0;
                        cx.notify();
                    }
                });
            })
            .detach();
        }
    }

    fn choose_sound(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Use sound".into()),
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
                .background_spawn(async move { crate::background::import_sound(&data_dir, &path) })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(name) => {
                        this.import_error = None;
                        this.set_notify(|n| n.sound = keel_proto::SoundChoice::Custom(name));
                        this.preview_sound(cx);
                    }
                    Err(err) => this.import_error = Some(format!("Couldn't use that sound: {err}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn render_notifications_tab(
        &mut self,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        use crate::platform_notify::{BannerPermission, banner_permission};
        use keel_proto::{BadgeStyle, SoundChoice};

        let n = self.state.ui_settings.notifications.clone();
        let switch = |id: &'static str, on: bool| toggle(id.to_string(), on, t);

        // 1. Triggers card
        let trigger_rows = vec![
            setting_row_info(
                t,
                "Agent finished",
                None,
                switch("notify-finished", n.on_finished)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.notify_toggle(|n| n.on_finished = !n.on_finished);
                        cx.notify();
                    }))
                    .into_any_element(),
            ),
            setting_row_info(
                t,
                "Needs your input",
                Some("Alerts when an agent asks a question or requests permission."),
                switch("notify-input", n.on_needs_input)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.notify_toggle(|n| n.on_needs_input = !n.on_needs_input);
                        cx.notify();
                    }))
                    .into_any_element(),
            ),
            setting_row_info(
                t,
                "Turn error",
                Some("Alerts if a turn fails or is cut off unexpectedly."),
                switch("notify-error", n.on_error)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.notify_toggle(|n| n.on_error = !n.on_error);
                        cx.notify();
                    }))
                    .into_any_element(),
            ),
        ];

        // 2. Alerts card (Badge, Dock bounce, macOS Banner + permission notice)
        crate::platform_notify::refresh_permission();
        let permission = banner_permission();
        let banner_notice = (n.banner && permission != BannerPermission::Allowed).then(|| {
            let (msg, action): (&'static str, Option<AnyElement>) = match permission {
                BannerPermission::Denied => (
                    "Banners are blocked in macOS System Settings.",
                    Some(
                        button(
                            "notify-open-settings",
                            "Open Settings",
                            ButtonKind::Ghost,
                            true,
                            t,
                        )
                        .on_click(|_, _, _| crate::platform_notify::open_notification_settings())
                        .into_any_element(),
                    ),
                ),
                BannerPermission::NotAsked => (
                    "Permission required to show macOS banners.",
                    Some(
                        button("notify-ask", "Allow", ButtonKind::Ghost, true, t)
                            .on_click(|_, _, _| crate::platform_notify::request_permission())
                            .into_any_element(),
                    ),
                ),
                _ => ("Banners require the installed Keel.app bundle.", None),
            };
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(zpx(12.))
                .py(zpx(8.))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(zpx(8.))
                        .t_small()
                        .text_color(c(t.warning))
                        .child(Icon::Alert.el(zpx(13.)))
                        .child(msg),
                )
                .children(action)
        });

        let mut alert_rows = vec![
            setting_row_info(
                t,
                "Badge",
                Some("Shown on the Dock icon and sidebar chats."),
                self.render_segmented_control(
                    "badge",
                    &[
                        ("Count", BadgeStyle::Count),
                        ("Dot", BadgeStyle::Dot),
                        ("None", BadgeStyle::None),
                    ],
                    n.badge,
                    52.0,
                    t,
                    cx,
                    |this, style, _| {
                        this.set_notify(|n| n.badge = style);
                    },
                )
                .into_any_element(),
            ),
            setting_row_info(
                t,
                "Bounce Dock icon",
                Some("Bounces once when an agent needs your input."),
                switch("notify-bounce", n.bounce)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.notify_toggle(|n| n.bounce = !n.bounce);
                        cx.notify();
                    }))
                    .into_any_element(),
            ),
            setting_row_info(
                t,
                "macOS banner",
                Some("Shows a native macOS notification banner. Click it to jump to the chat."),
                switch("notify-banner", n.banner)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.notify_toggle(|n| n.banner = !n.banner);
                        if this.state.ui_settings.notifications.banner
                            && banner_permission() == BannerPermission::NotAsked
                        {
                            crate::platform_notify::request_permission();
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
            ),
        ];
        if let Some(notice) = banner_notice {
            alert_rows.push(notice);
        }

        // 3. Sound card
        if self.show_sound_menu && self.system_sounds.is_empty() {
            self.system_sounds = crate::background::system_sounds();
        }
        let sound_label = crate::background::sound_label(&n.sound);
        let volumes = [
            (0.25_f32, "25%"),
            (0.5, "50%"),
            (0.75, "75%"),
            (1.0, "100%"),
        ];
        let nearest = volumes
            .iter()
            .min_by(|a, b| (a.0 - n.volume).abs().total_cmp(&(b.0 - n.volume).abs()))
            .map(|v| v.0)
            .unwrap_or(0.75);

        let sound_phase = self.menu_phase("sound-list", self.show_sound_menu, cx);
        let sound_menu: Option<AnyElement> = if let Some(leaving) = sound_phase {
            let mut options: Vec<(SoundChoice, String)> = vec![
                (SoundChoice::None, "None".into()),
                (
                    SoundChoice::Default,
                    crate::background::sound_label(&SoundChoice::Default),
                ),
            ];
            options.extend(
                self.system_sounds
                    .iter()
                    .filter(|name| name.as_str() != keel_proto::DEFAULT_SOUND)
                    .map(|name| (SoundChoice::System(name.clone()), name.clone())),
            );
            if let SoundChoice::Custom(_) = &n.sound {
                options.push((n.sound.clone(), crate::background::sound_label(&n.sound)));
            }

            let weak_dismiss = cx.entity().downgrade();
            let dismiss =
                move |_: &gpui::MouseDownEvent, _: &mut gpui::Window, cx: &mut gpui::App| {
                    let _ = weak_dismiss.update(cx, |this: &mut KeelWindow, cx| {
                        this.note_menu_dismissed("sound-list");
                        this.show_sound_menu = false;
                        cx.notify();
                    });
                };

            let mut items: Vec<AnyElement> = options
                .into_iter()
                .enumerate()
                .map(|(ix, (choice, label))| {
                    let selected = choice == n.sound;
                    div()
                        .id(ElementId::Name(format!("sound-opt-{ix}").into()))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(zpx(8.))
                        .px(zpx(8.))
                        .py(zpx(4.))
                        .rounded(r_sm())
                        .t_small()
                        .cursor_pointer()
                        .when(selected, |d| d.bg(c(t.selected)))
                        .when(!selected, |d| d.hover_fade(c(t.hover), None))
                        .child(
                            div()
                                .w(zpx(12.))
                                .text_color(c(t.accent))
                                .child(if selected { "✓" } else { "" }),
                        )
                        .child(label)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            let choice = choice.clone();
                            this.show_sound_menu = false;
                            this.set_notify(|n| n.sound = choice);
                            this.preview_sound(cx);
                            cx.notify();
                        }))
                        .into_any_element()
                })
                .collect();

            items.push(
                div()
                    .id("sound-opt-custom")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(8.))
                    .px(zpx(8.))
                    .py(zpx(4.))
                    .rounded(r_sm())
                    .t_small()
                    .text_color(c(t.accent))
                    .cursor_pointer()
                    .hover_fade(c(t.hover), None)
                    .child(div().w(zpx(12.)))
                    .child("Custom…")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.show_sound_menu = false;
                        this.choose_sound(cx);
                    }))
                    .into_any_element(),
            );

            Some(super::super::floating_menu_positioned_with_max_h(
                "sound-list",
                t,
                180.,
                200.,
                gpui::Anchor::TopLeft,
                None,
                false,
                leaving,
                self.reduced(),
                dismiss,
                items,
            ))
        } else {
            None
        };

        let is_playing = self.sound_preview_epoch > 0;
        let is_none = matches!(n.sound, SoundChoice::None);

        let sound_anchor = div()
            .relative()
            .child(
                div()
                    .id("sound-picker")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(8.))
                    .px(zpx(10.))
                    .h(zpx(CONTROL_H))
                    .rounded(r_md())
                    .bg(if self.show_sound_menu {
                        c(t.selected)
                    } else {
                        c(t.input)
                    })
                    .border_1()
                    .border_color(if self.show_sound_menu {
                        c(t.accent)
                    } else {
                        c(t.border)
                    })
                    .t_small()
                    .text_color(c(t.text))
                    .cursor_pointer()
                    .hover_fade(c(t.hover), Some(c(t.selected)))
                    .active(|s| s.opacity(0.8))
                    .child(Icon::Volume2.el(zpx(12.)).text_color(c(if is_none {
                        t.text_faint
                    } else {
                        t.text_muted
                    })))
                    .child(sound_label)
                    .child(
                        Icon::ChevronDown
                            .el(zpx(11.))
                            .text_color(c(t.text_muted))
                            .with_transformation(gpui::Transformation::rotate(gpui::radians(
                                if self.show_sound_menu {
                                    std::f32::consts::PI
                                } else {
                                    0.0
                                },
                            ))),
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        if this.menu_just_dismissed("sound-list") {
                            return;
                        }
                        this.show_sound_menu = !this.show_sound_menu;
                        cx.notify();
                    })),
            )
            .children(sound_menu);

        let sound_control =
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(8.))
                .child(sound_anchor)
                .when(!is_none, |d| {
                    d.child(render_sound_preview_btn(is_playing, t).on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| this.preview_sound(cx)),
                    ))
                });

        let sound_rows = vec![
            setting_row_info(
                t,
                "Sound",
                Some("Played when an agent finishes or needs attention."),
                sound_control.into_any_element(),
            ),
            setting_row_info(
                t,
                "Volume",
                None,
                self.render_segmented_control(
                    "volume",
                    &[
                        ("25%", 0.25_f32),
                        ("50%", 0.5),
                        ("75%", 0.75),
                        ("100%", 1.0),
                    ],
                    nearest,
                    46.0,
                    t,
                    cx,
                    |this, volume, cx| {
                        this.set_notify(|n| n.volume = volume);
                        this.preview_sound(cx);
                    },
                )
                .into_any_element(),
            ),
            setting_row_info(
                t,
                "Background only",
                Some("Only play sounds when Keel is not the active window."),
                switch("notify-sound-bg", n.sound_in_background_only)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.notify_toggle(|n| {
                            n.sound_in_background_only = !n.sound_in_background_only
                        });
                        cx.notify();
                    }))
                    .into_any_element(),
            ),
        ];

        div()
            .flex()
            .flex_col()
            .gap(zpx(24.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(card_group_header(
                        "Notify when",
                        Some("Triggers for chats you aren't currently viewing."),
                        None,
                        t,
                    ))
                    .child(settings_card(trigger_rows, t)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(card_group_header("Alerts", None, None, t))
                    .child(settings_card(alert_rows, t)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(card_group_header("Sound", None, None, t))
                    .child(settings_card(sound_rows, t)),
            )
    }
}

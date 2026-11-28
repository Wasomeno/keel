//! Settings → Shortcuts: searchable, categorized list of all keyboard shortcuts
//! with live inline recording, conflict/duplicate validation, and per-row or
//! global reset to defaults.

use gpui::{
    AnimationExt, AnyElement, ClickEvent, Context, ElementId, IntoElement, Keystroke, Window,
    div, prelude::*,
};
use keel_theme::Tokens;

use super::super::KeelWindow;
use super::controls::{
    ButtonKind, button, card_group_header, icon_button, settings_card,
};
use super::{SettingsTab, ShortcutRecordingState};
use crate::hover_fade::HoverFadeExt;
use crate::icons::Icon;
use crate::keys::{
    self, CandidateOutcome, ShortcutCategory, ShortcutSpec,
};
use crate::style::{CONTROL_H, INPUT_H, KeelStyled, MONO, ROW_MIN_H, c, r_md, r_sm, zpx};

fn keycap_shadow(t: Tokens) -> Vec<gpui::BoxShadow> {
    vec![gpui::BoxShadow {
        color: c(t.shadow),
        offset: gpui::point(zpx(0.), zpx(1.)),
        blur_radius: zpx(0.),
        spread_radius: zpx(0.),
        inset: false,
    }]
}

fn recording_ring(color: u32) -> Vec<gpui::BoxShadow> {
    vec![gpui::BoxShadow {
        color: c(color).opacity(0.28),
        offset: gpui::point(zpx(0.), zpx(0.)),
        blur_radius: zpx(0.),
        spread_radius: zpx(2.),
        inset: false,
    }]
}

fn keycap(label: String, t: Tokens) -> gpui::Div {
    div()
        .min_w(zpx(18.))
        .h(zpx(18.))
        .px(zpx(5.))
        .rounded(r_sm())
        .bg(c(t.window))
        .border_1()
        .border_color(c(t.border))
        .shadow(keycap_shadow(t))
        .flex()
        .items_center()
        .justify_center()
        .font_family(MONO)
        .t_caption()
        .text_color(c(t.text))
        .child(label)
}

fn matches_query(
    spec: &ShortcutSpec,
    query: &str,
    effective: &str,
    compact: &str,
) -> bool {
    if query.is_empty() {
        return true;
    }
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return true;
    }
    spec.name.to_ascii_lowercase().contains(&q)
        || spec.description.to_ascii_lowercase().contains(&q)
        || spec.category.title().to_ascii_lowercase().contains(&q)
        || spec
            .scope
            .badge()
            .is_some_and(|b| b.to_ascii_lowercase().contains(&q))
        || effective.to_ascii_lowercase().contains(&q)
        || compact.to_ascii_lowercase().contains(&q)
}

impl KeelWindow {
    pub(in crate::view) fn is_recording_shortcut(&self) -> bool {
        self.settings_open
            && self.settings_tab == SettingsTab::Shortcuts
            && self.shortcut_recording.is_some()
    }

    pub(super) fn start_shortcut_recording(
        &mut self,
        id: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.shortcut_reset_confirm = None;
        self.shortcut_recording = Some(ShortcutRecordingState {
            id,
            error: None,
            error_seq: 0,
            held_modifiers: gpui::Modifiers::none(),
        });
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    pub(super) fn cancel_shortcut_recording(&mut self, cx: &mut Context<Self>) {
        if self.shortcut_recording.take().is_some() {
            cx.notify();
        }
    }

    pub(in crate::view) fn update_shortcut_held_modifiers(
        &mut self,
        modifiers: gpui::Modifiers,
        cx: &mut Context<Self>,
    ) {
        if let Some(rec) = &mut self.shortcut_recording {
            if rec.held_modifiers != modifiers {
                rec.held_modifiers = modifiers;
                cx.notify();
            }
        }
    }

    pub(in crate::view) fn handle_shortcut_record_keystroke(
        &mut self,
        ks: &Keystroke,
        cx: &mut Context<Self>,
    ) {
        let Some(rec) = self.shortcut_recording.as_mut() else {
            return;
        };
        let target_id = rec.id;
        let os = keys::current_os();
        match keys::validate_candidate(&self.state.ui_settings.shortcuts, target_id, ks, os) {
            CandidateOutcome::IgnoreModifierOnly => {
                rec.held_modifiers = ks.modifiers;
                cx.notify();
            }
            CandidateOutcome::Cancel => {
                self.shortcut_recording = None;
                cx.notify();
            }
            CandidateOutcome::Clear => {
                self.clear_shortcut(target_id, cx);
            }
            CandidateOutcome::Accept(canonical) => {
                self.save_shortcut(target_id, canonical, cx);
            }
            CandidateOutcome::Reject(msg) => {
                rec.error = Some(msg);
                rec.error_seq = rec.error_seq.wrapping_add(1);
                rec.held_modifiers = ks.modifiers;
                cx.notify();
            }
        }
    }

    fn save_shortcut(
        &mut self,
        id: &'static str,
        keystroke: String,
        cx: &mut Context<Self>,
    ) {
        let os = keys::current_os();
        let default = keys::shortcut_by_id(id)
            .map(|s| s.default_for_os(os))
            .unwrap_or_default();
        if keystroke == default {
            self.state.ui_settings.shortcuts.remove(id);
        } else {
            self.state
                .ui_settings
                .shortcuts
                .insert(id.to_string(), keystroke);
        }
        self.state.save_settings();
        keys::apply(cx, &self.state.ui_settings.shortcuts);
        self.shortcut_recording = None;
        cx.notify();
    }

    fn clear_shortcut(&mut self, id: &'static str, cx: &mut Context<Self>) {
        let os = keys::current_os();
        let default = keys::shortcut_by_id(id)
            .map(|s| s.default_for_os(os))
            .unwrap_or_default();
        if default.is_empty() {
            self.state.ui_settings.shortcuts.remove(id);
        } else {
            self.state
                .ui_settings
                .shortcuts
                .insert(id.to_string(), String::new());
        }
        self.state.save_settings();
        keys::apply(cx, &self.state.ui_settings.shortcuts);
        self.shortcut_recording = None;
        cx.notify();
    }

    fn reset_shortcut(&mut self, id: &'static str, cx: &mut Context<Self>) {
        self.state.ui_settings.shortcuts.remove(id);
        self.state.save_settings();
        keys::apply(cx, &self.state.ui_settings.shortcuts);
        if self
            .shortcut_recording
            .as_ref()
            .is_some_and(|r| r.id == id)
        {
            self.shortcut_recording = None;
        }
        cx.notify();
    }

    fn reset_all_shortcuts(&mut self, cx: &mut Context<Self>) {
        self.state.ui_settings.shortcuts.clear();
        self.state.save_settings();
        keys::apply(cx, &self.state.ui_settings.shortcuts);
        self.shortcut_recording = None;
        self.shortcut_reset_confirm = None;
        cx.notify();
    }

    pub(super) fn render_shortcuts_tab(
        &mut self,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let os = keys::current_os();
        let query = self.shortcuts_filter.read(cx).value().to_string();
        let has_query = !query.trim().is_empty();
        let customized = keys::customized_count(&self.state.ui_settings.shortcuts, os);
        let confirming_reset = self.shortcut_reset_confirm.is_some();

        let search_box = div()
            .id("shortcuts-search-box")
            .flex_1()
            .min_w_0()
            .h(zpx(INPUT_H))
            .px(zpx(8.))
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(8.))
            .rounded(r_md())
            .bg(c(t.input))
            .border_1()
            .border_color(c(t.border))
            .t_body()
            .text_color(c(t.text_muted))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.cancel_shortcut_recording(cx);
                this.shortcut_reset_confirm = None;
            }))
            .child(Icon::Search.el(zpx(13.)).text_color(c(t.text_faint)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(self.shortcuts_filter.clone().into_any_element()),
            )
            .when(has_query, |d| {
                d.child(
                    icon_button("shortcuts-search-clear", Icon::Close, t)
                        .tooltip(move |_, cx| {
                            cx.new(|_| crate::view::dotify_ui::Tip::new("Clear search", t))
                                .into()
                        })
                        .tooltip_show_delay(crate::view::tooltip::delay())
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.shortcuts_filter
                                .update(cx, |s, cx| s.set_value("", window, cx));
                            cx.notify();
                        })),
                )
            });

        let summary_label = if customized == 0 {
            format!("{} shortcuts", keys::all_shortcuts().len())
        } else {
            format!(
                "{} shortcuts · {} custom",
                keys::all_shortcuts().len(),
                customized
            )
        };

        let toolbar = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(zpx(12.))
            .child(search_box)
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(8.))
                    .child(
                        div()
                            .t_small()
                            .text_color(c(t.text_muted))
                            .child(summary_label),
                    )
                    .when(customized > 0, |d| {
                        if confirming_reset {
                            d.child(
                                button(
                                    "shortcuts-reset-cancel",
                                    "Cancel",
                                    ButtonKind::Ghost,
                                    true,
                                    t,
                                )
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.shortcut_reset_confirm = None;
                                    cx.notify();
                                })),
                            )
                            .child(
                                button(
                                    "shortcuts-reset-confirm",
                                    format!("Reset {customized}?"),
                                    ButtonKind::Danger,
                                    true,
                                    t,
                                )
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.reset_all_shortcuts(cx);
                                })),
                            )
                        } else {
                            d.child(
                                button(
                                    "shortcuts-reset-all",
                                    "Reset all",
                                    ButtonKind::Secondary,
                                    true,
                                    t,
                                )
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.cancel_shortcut_recording(cx);
                                    this.shortcut_reset_confirm = Some(1);
                                    cx.notify();
                                })),
                            )
                        }
                    }),
            );

        let mut sections: Vec<AnyElement> = Vec::new();
        for &cat in ShortcutCategory::ALL {
            let matching: Vec<&ShortcutSpec> = keys::all_shortcuts()
                .iter()
                .filter(|spec| {
                    if spec.category != cat {
                        return false;
                    }
                    let eff = keys::effective_binding(
                        &self.state.ui_settings.shortcuts,
                        spec.id,
                        os,
                    );
                    let compact = keys::display_compact(&eff, os);
                    matches_query(spec, &query, &eff, &compact)
                })
                .collect();

            if matching.is_empty() {
                continue;
            }

            let rows: Vec<gpui::Div> = matching
                .into_iter()
                .map(|spec| self.render_shortcut_row(spec, os, t, cx))
                .collect();

            sections.push(
                div()
                    .flex()
                    .flex_col()
                    .child(card_group_header(cat.title(), cat.subtitle(), None, t))
                    .child(settings_card(rows, t))
                    .into_any_element(),
            );
        }

        if sections.is_empty() {
            sections.push(
                div()
                    .w_full()
                    .py(zpx(32.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(zpx(10.))
                    .child(
                        div()
                            .t_body()
                            .text_color(c(t.text_muted))
                            .child(format!("No shortcuts match \"{}\"", query.trim())),
                    )
                    .child(
                        button(
                            "shortcuts-empty-clear",
                            "Clear search",
                            ButtonKind::Secondary,
                            true,
                            t,
                        )
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.shortcuts_filter
                                .update(cx, |s, cx| s.set_value("", window, cx));
                            cx.notify();
                        })),
                    )
                    .into_any_element(),
            );
        }

        div().child(
            div()
                .id("shortcuts-tab-root")
                .flex()
                .flex_col()
                .gap(zpx(20.))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.cancel_shortcut_recording(cx);
                    if this.shortcut_reset_confirm.take().is_some() {
                        cx.notify();
                    }
                }))
                .child(toolbar)
                .children(sections),
        )
    }

    fn render_shortcut_row(
        &self,
        spec: &'static ShortcutSpec,
        os: gpui::OperatingSystem,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let id = spec.id;
        let effective = keys::effective_binding(&self.state.ui_settings.shortcuts, id, os);
        let tokens = keys::display_tokens(&effective, os);
        let is_custom = keys::is_customized(&self.state.ui_settings.shortcuts, id, os);
        let default_display = keys::display_compact(spec.default_for_os(os), os);
        let recording = self
            .shortcut_recording
            .as_ref()
            .filter(|r| r.id == id);

        let title_line = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(6.))
            .child(div().t_body().text_color(c(t.text)).child(spec.name))
            .when_some(spec.scope.badge(), |d, badge| {
                d.child(
                    div()
                        .flex_none()
                        .h(zpx(18.))
                        .px(zpx(6.))
                        .rounded(r_sm())
                        .bg(c(t.hover).opacity(0.75))
                        .flex()
                        .items_center()
                        .justify_center()
                        .t_caption()
                        .text_color(c(t.text_muted))
                        .child(badge),
                )
            })
            .when(is_custom, |d| {
                d.child(
                    div()
                        .flex_none()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(zpx(4.))
                        .child(div().size(zpx(5.)).rounded_full().bg(c(t.accent)))
                        .child(
                            div()
                                .t_caption()
                                .text_color(c(t.accent))
                                .child("Custom"),
                        ),
                )
            });

        let left = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .justify_center()
            .gap(zpx(2.))
            .child(title_line)
            .child(
                div()
                    .t_small()
                    .text_color(c(t.text_muted))
                    .child(spec.description),
            );

        let right: AnyElement = if let Some(rec) = recording {
            let has_err = rec.error.is_some();
            let ring_col = if has_err { t.danger } else { t.accent };
            let held = keys::modifier_tokens(rec.held_modifiers, os);

            let capture_pill = div()
                .id(ElementId::Name(format!("shortcut-rec-{id}").into()))
                .h(zpx(CONTROL_H))
                .min_w(zpx(108.))
                .px(zpx(8.))
                .rounded(r_md())
                .bg(c(t.selected))
                .border_1()
                .border_color(c(ring_col))
                .shadow(recording_ring(ring_col))
                .flex()
                .flex_row()
                .items_center()
                .justify_center()
                .gap(zpx(4.))
                .on_click(|_: &ClickEvent, _, cx| cx.stop_propagation())
                .map(|d| {
                    if !held.is_empty() {
                        d.children(held.into_iter().map(|tok| keycap(tok, t)))
                            .child(
                                div()
                                    .t_caption()
                                    .text_color(c(t.text_muted))
                                    .child("…"),
                            )
                    } else {
                        d.child(
                            div()
                                .t_small()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(c(if has_err { t.danger } else { t.accent }))
                                .child("Press keys…"),
                        )
                    }
                });

            div()
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(4.))
                .when(!tokens.is_empty(), |d| {
                    d.child(
                        button(
                            format!("shortcut-clear-{id}"),
                            "Clear",
                            ButtonKind::Ghost,
                            true,
                            t,
                        )
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            this.clear_shortcut(id, cx);
                        })),
                    )
                })
                .child(capture_pill)
                .child(
                    icon_button(format!("shortcut-cancel-{id}"), Icon::Close, t)
                        .tooltip(move |_, cx| {
                            cx.new(|_| crate::view::dotify_ui::Tip::new("Cancel (Esc)", t))
                                .into()
                        })
                        .tooltip_show_delay(crate::view::tooltip::delay())
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            this.cancel_shortcut_recording(cx);
                        })),
                )
                .into_any_element()
        } else {
            let edit_pill = div()
                .id(ElementId::Name(format!("shortcut-edit-{id}").into()))
                .h(zpx(CONTROL_H))
                .px(zpx(6.))
                .rounded(r_md())
                .bg(c(t.input))
                .border_1()
                .border_color(c(t.border))
                .flex()
                .flex_row()
                .items_center()
                .justify_center()
                .gap(zpx(3.))
                .cursor_pointer()
                .hover(|s| s.border_color(c(t.text_faint)))
                .hover_fade(c(t.hover), None)
                .active(|s| s.opacity(0.8))
                .tooltip(move |_, cx| {
                    cx.new(|_| crate::view::dotify_ui::Tip::new("Click to edit shortcut", t))
                        .into()
                })
                .tooltip_show_delay(crate::view::tooltip::delay())
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    this.start_shortcut_recording(id, window, cx);
                }))
                .map(|d| {
                    if tokens.is_empty() {
                        d.child(
                            div()
                                .px(zpx(4.))
                                .t_small()
                                .text_color(c(t.text_faint))
                                .child("Not set"),
                        )
                    } else {
                        d.children(tokens.into_iter().map(|tok| keycap(tok, t)))
                    }
                });

            let reset_tip = if default_display.is_empty() {
                "Reset to unbound".to_string()
            } else {
                format!("Reset to {default_display}")
            };

            div()
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(4.))
                .when(is_custom, |d| {
                    d.child(
                        icon_button(format!("shortcut-reset-{id}"), Icon::Reset, t)
                            .tooltip(move |_, cx| {
                                cx.new(|_| crate::view::dotify_ui::Tip::new(reset_tip.clone(), t))
                                    .into()
                            })
                            .tooltip_show_delay(crate::view::tooltip::delay())
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                this.reset_shortcut(id, cx);
                            })),
                    )
                })
                .child(edit_pill)
                .into_any_element()
        };

        let error_banner = recording.and_then(|rec| {
            let err = rec.error.clone()?;
            let seq = rec.error_seq as usize;
            let reduced = self.reduced();
            let banner = div()
                .mb(zpx(10.))
                .px(zpx(10.))
                .py(zpx(6.))
                .rounded(r_md())
                .bg(c(t.danger).opacity(0.08))
                .border_1()
                .border_color(c(t.danger).opacity(0.28))
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(6.))
                .child(Icon::Alert.el(zpx(12.)).text_color(c(t.danger)))
                .child(div().t_small().text_color(c(t.danger)).child(err));
            Some(
                banner
                    .with_animation(
                        ElementId::Name(format!("shortcut-err-{id}-{seq}").into()),
                        gpui::Animation::new(std::time::Duration::from_millis(120))
                            .with_easing(crate::motion::ease_out),
                        move |el, delta| {
                            if reduced {
                                el
                            } else {
                                el.opacity(delta).relative().top(zpx(3. * (1. - delta)))
                            }
                        },
                    )
                    .into_any_element(),
            )
        });

        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .min_h(zpx(ROW_MIN_H))
                    .py(zpx(10.))
                    .gap(zpx(16.))
                    .child(left)
                    .child(right),
            )
            .children(error_banner)
    }
}

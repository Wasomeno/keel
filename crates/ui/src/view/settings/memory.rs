//! Settings → Memory: inline first-class tab for inspecting, editing,
//! archiving, and deleting persistent agent notes.

use gpui::{AnyElement, ClickEvent, Context, ElementId, IntoElement, div, prelude::*};
use keel_theme::Tokens;

use super::super::KeelWindow;
use super::controls::{ButtonKind, button, icon_button, input_shell, settings_card};
use super::{MemoryConfirm, MemoryNote, MemoryOpen, MemoryScopeView, MemoryTab};
use crate::hover_fade::HoverFadeExt;
use crate::icons::Icon;
use crate::style::{KeelStyled, MONO, ROW_MIN_H, c, r_sm, r_xl, zpx};

impl KeelWindow {
    pub(in crate::view) fn load_memory(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.memory.load_gen += 1;
        let ticket = self.memory.load_gen;
        let cwd = self.mention_root();
        cx.spawn(async move |this, cx| {
            let res = client
                .call("memory_list", serde_json::json!({ "cwd": cwd }))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.memory.load_gen != ticket {
                    return;
                }
                this.memory.cwd = cwd;
                match res {
                    Ok(v) => {
                        this.memory.user =
                            Some(Ok(MemoryScopeView::parse(&v["user"]).unwrap_or_default()));
                        this.memory.project = Some(Ok(MemoryScopeView::parse(&v["project"])));
                    }
                    Err(err) => {
                        this.memory.user = Some(Err(err.clone()));
                        this.memory.project = Some(Err(err));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(in crate::view) fn open_memory(&mut self, cx: &mut Context<Self>) {
        self.memory.open = true;
        self.memory.modal = None;
        self.memory.modal_leaving = None;
        self.memory.confirm = None;
        self.memory.error = None;
        self.memory
            .scroll
            .set_offset(gpui::point(gpui::px(0.), gpui::px(0.)));
        self.select_settings_tab(super::SettingsTab::Memory, cx);
        self.load_memory(cx);
    }

    pub(super) fn render_memory_tab(&mut self, t: Tokens, cx: &mut Context<Self>) -> gpui::Div {
        self.render_memory_body(t, cx)
    }

    fn render_memory_body(&mut self, t: Tokens, cx: &mut Context<Self>) -> gpui::Div {
        let reduced = self.reduced();
        let tab = self.memory.tab;
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(zpx(16.))
            .pb(zpx(12.))
            .child(self.render_segmented_control(
                "memory-scope",
                &[
                    ("You", MemoryTab::You),
                    ("This project", MemoryTab::Project),
                ],
                tab,
                96.0,
                t,
                cx,
                |this, tab, _| {
                    if this.memory.tab != tab {
                        this.memory.tab = tab;
                        this.memory.list_epoch = this.memory.list_epoch.wrapping_add(1);
                    }
                },
            ));

        let muted_line = |text: &'static str| {
            div()
                .py(zpx(12.))
                .t_body()
                .text_color(c(t.text_muted))
                .child(text)
        };
        let scope: Result<Option<MemoryScopeView>, String> = match tab {
            MemoryTab::You => match &self.memory.user {
                None => return div().flex().flex_col().child(header),
                Some(Ok(s)) => Ok(Some(s.clone())),
                Some(Err(e)) => Err(e.clone()),
            },
            MemoryTab::Project => match &self.memory.project {
                None => return div().flex().flex_col().child(header),
                Some(Ok(s)) => Ok(s.clone()),
                Some(Err(e)) => Err(e.clone()),
            },
        };
        let content: gpui::AnyElement = match scope {
            Err(err) => div()
                .flex()
                .flex_col()
                .gap(zpx(8.))
                .py(zpx(8.))
                .child(
                    div()
                        .t_body()
                        .text_color(c(t.text))
                        .child("Couldn't read memory."),
                )
                .child(div().t_small().text_color(c(t.danger)).child(err))
                .child(
                    button("memory-retry", "Retry", ButtonKind::Ghost, true, t)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.load_memory(cx))),
                )
                .into_any_element(),
            Ok(None) => muted_line("Open a chat that has a folder to see its project notes.")
                .into_any_element(),
            Ok(Some(scope)) if scope.notes.is_empty() => muted_line("None yet.").into_any_element(),
            Ok(Some(scope)) => {
                let warning = scope.truncated.then(|| {
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(zpx(6.))
                        .pb(zpx(8.))
                        .t_small()
                        .text_color(c(t.warning))
                        .child(Icon::Alert.el(zpx(12.)))
                        .child("The index is over the cap. Later chats will not see every line.")
                });
                let rows: Vec<_> = scope
                    .notes
                    .into_iter()
                    .map(|note| self.render_memory_row(tab, note, t, cx))
                    .collect();
                let list = div()
                    .flex()
                    .flex_col()
                    .children(warning)
                    .child(settings_card(rows, t));
                crate::motion::appear(
                    list,
                    ("memory-list-in", self.memory.list_epoch as usize),
                    crate::motion::BASE,
                    4.,
                    reduced,
                )
                .into_any_element()
            }
        };
        div().flex().flex_col().child(header).child(content)
    }

    fn render_memory_row(
        &mut self,
        tab: MemoryTab,
        note: MemoryNote,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let tag = |text: String, mono: bool| {
            div()
                .flex_none()
                .px(zpx(6.))
                .h(zpx(18.))
                .flex()
                .items_center()
                .rounded(r_sm())
                .bg(c(t.input))
                .t_caption()
                .text_color(c(t.text_muted))
                .when(mono, |d| d.font_family(MONO))
                .child(text)
        };
        let description = if note.description.is_empty() {
            note.slug.clone()
        } else {
            note.description.clone()
        };
        let open = note.clone();
        div()
            .id(ElementId::Name(
                format!("memory-row-{}-{}", tab.scope(), note.slug).into(),
            ))
            .w_full()
            .min_h(zpx(ROW_MIN_H))
            .py(zpx(10.))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(zpx(16.))
            .cursor_pointer()
            .hover_fade(c(t.hover), None)
            .active(|s| s.opacity(0.8))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.open_memory_note(tab, open.clone(), window, cx)
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .t_body()
                    .text_color(c(if note.unindexed { t.text_muted } else { t.text }))
                    .child(description),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(6.))
                    .when(note.unindexed, |d| d.child(tag("Unindexed".into(), false)))
                    .when(!note.kind.is_empty(), |d| {
                        d.child(tag(note.kind.clone(), false))
                    })
                    .child(tag(note.slug.clone(), true)),
            )
    }

    fn open_memory_note(
        &mut self,
        tab: MemoryTab,
        note: MemoryNote,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.memory.modal_gen = self.memory.modal_gen.wrapping_add(1);
        let generation = self.memory.modal_gen;
        let cwd = self.memory.cwd.clone();
        self.memory.modal = Some(MemoryOpen {
            tab,
            cwd: cwd.clone(),
            note: note.clone(),
            loaded: None,
        });
        self.memory.modal_leaving = None;
        self.memory.confirm = None;
        self.memory.error = None;
        self.memory.busy = false;
        self.memory
            .modal_scroll
            .set_offset(gpui::point(gpui::px(0.), gpui::px(0.)));
        let description = note.description.clone();
        self.memory_desc
            .update(cx, |i, cx| i.set_value(description, window, cx));
        self.memory_body
            .update(cx, |i, cx| i.set_value("", window, cx));
        let params = serde_json::json!({ "scope": tab.scope(), "slug": note.slug, "cwd": cwd });
        cx.spawn_in(window, async move |this, cx| {
            let res = client.call("memory_read", params).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(open) = this.memory.modal.as_mut() else {
                    return;
                };
                if this.memory.modal_gen != generation {
                    return;
                }
                match res {
                    Ok(v) => {
                        open.loaded = Some(Ok(()));
                        let body = v["body"].as_str().unwrap_or_default().to_string();
                        this.memory_body
                            .update(cx, |i, cx| i.set_value(body, window, cx));
                    }
                    Err(err) => open.loaded = Some(Err(err)),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(in crate::view) fn dismiss_memory_modal(&mut self, cx: &mut Context<Self>) {
        let Some(open) = self.memory.modal.take() else {
            return;
        };
        self.memory.confirm = None;
        self.memory.error = None;
        if self.reduced() {
            self.memory.modal_leaving = None;
            cx.notify();
            return;
        }
        let generation = self.memory.modal_gen;
        self.memory.modal_leaving = Some((generation, open));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(crate::motion::FAST).await;
            let _ = this.update(cx, |this, cx| {
                if this.memory.modal_leaving.as_ref().map(|(n, _)| *n) == Some(generation) {
                    this.memory.modal_leaving = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Save, archive or delete the open note; reloads the list on success.
    fn memory_modal_action(&mut self, action: Option<MemoryConfirm>, cx: &mut Context<Self>) {
        let (Some(client), Some(open)) = (self.client.clone(), self.memory.modal.clone()) else {
            return;
        };
        if self.memory.busy {
            return;
        }
        let note = &open.note;
        let scope = open.tab.scope();
        let (method, params) = match action {
            None => (
                "memory_write",
                serde_json::json!({
                    "scope": scope,
                    "action": "upsert",
                    "slug": note.slug,
                    "type": note.kind,
                    "description": self.memory_desc.read(cx).value().trim().to_string(),
                    "body": self.memory_body.read(cx).value().to_string(),
                    "cwd": open.cwd,
                }),
            ),
            Some(MemoryConfirm::Archive) => (
                "memory_write",
                serde_json::json!({ "scope": scope, "action": "archive", "slug": note.slug, "cwd": open.cwd }),
            ),
            Some(MemoryConfirm::Delete) => (
                "memory_delete",
                serde_json::json!({ "scope": scope, "slug": note.slug, "cwd": open.cwd }),
            ),
        };
        self.memory.busy = true;
        self.memory.error = None;
        let generation = self.memory.modal_gen;
        cx.spawn(async move |this, cx| {
            let res = client.call(method, params).await;
            let _ = this.update(cx, |this, cx| {
                this.memory.busy = false;
                if this.memory.modal_gen != generation || this.memory.modal.is_none() {
                    return;
                }
                match res {
                    Ok(_) => {
                        this.dismiss_memory_modal(cx);
                        this.load_memory(cx);
                    }
                    Err(err) => {
                        this.memory.confirm = None;
                        this.memory.error = Some(err);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn render_memory_modal(
        &mut self,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (open, leaving, modal_gen) = match (&self.memory.modal, &self.memory.modal_leaving) {
            (Some(o), _) => (o.clone(), false, self.memory.modal_gen),
            (None, Some((n, o))) => (o.clone(), true, *n),
            _ => return None,
        };
        let reduced = self.reduced();
        let note = open.note.clone();
        let busy = self.memory.busy;
        let confirm = self.memory.confirm;
        let field = |label: &'static str| {
            div()
                .t_small()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(c(t.text_muted))
                .child(label)
        };
        let input_box = || input_shell(t).w_full().flex_none();
        let tag = |text: String, mono: bool| {
            div()
                .flex_none()
                .px(zpx(6.))
                .h(zpx(18.))
                .flex()
                .items_center()
                .rounded(r_sm())
                .bg(c(t.input))
                .t_caption()
                .text_color(c(t.text_muted))
                .when(mono, |d| d.font_family(MONO))
                .child(text)
        };

        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(zpx(12.))
            .px(zpx(20.))
            .py(zpx(12.))
            .border_b_1()
            .border_color(c(t.border))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(8.))
                    .child(div().t_title().text_color(c(t.text)).child("Edit note"))
                    .when(note.unindexed, |d| d.child(tag("Unindexed".into(), false)))
                    .when(!note.kind.is_empty(), |d| {
                        d.child(tag(note.kind.clone(), false))
                    })
                    .child(tag(note.slug.clone(), true)),
            )
            .child(
                icon_button("memory-modal-close-x", Icon::Close, t).on_click(
                    cx.listener(|this, _: &ClickEvent, _, cx| this.dismiss_memory_modal(cx)),
                ),
            );

        let loading = open.loaded.is_none();
        let read_error = open.loaded.clone().and_then(|r| r.err());
        let body = div()
            .id("memory-modal-scroll")
            .track_scroll(&self.memory.modal_scroll)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(zpx(20.))
            .py(zpx(16.))
            .flex()
            .flex_col()
            .gap(zpx(6.))
            .child(field("Description"))
            .child(input_box().child(self.memory_desc.clone()))
            .child(div().h(zpx(8.)))
            .child(field("Body"))
            .child(
                input_box()
                    .h(zpx(160.))
                    .items_start()
                    .child(self.memory_body.clone()),
            )
            .when_some(read_error, |d, err| {
                d.child(
                    div()
                        .t_small()
                        .text_color(c(t.danger))
                        .child(format!("Couldn't read this note. {err}")),
                )
            })
            .when_some(self.memory.error.clone(), |d, err| {
                d.child(div().t_small().text_color(c(t.danger)).child(err))
            });

        let left = match confirm {
            Some(kind) => {
                let (question, action) = match kind {
                    MemoryConfirm::Archive => {
                        ("Archive this note? Later chats will not see it.", "Archive")
                    }
                    MemoryConfirm::Delete => ("Delete this note? The file is removed.", "Delete"),
                };
                crate::motion::appear(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(zpx(8.))
                        .child(div().t_small().text_color(c(t.text)).child(question))
                        .child(
                            button("memory-confirm-go", action, ButtonKind::Danger, true, t)
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.memory_modal_action(Some(kind), cx)
                                })),
                        )
                        .child(
                            button(
                                "memory-confirm-cancel",
                                "Cancel",
                                ButtonKind::Ghost,
                                true,
                                t,
                            )
                            .on_click(cx.listener(
                                |this, _: &ClickEvent, _, cx| {
                                    this.memory.confirm = None;
                                    cx.notify();
                                },
                            )),
                        ),
                    ("memory-confirm-in", self.memory.confirm_gen as usize),
                    crate::motion::FAST,
                    4.,
                    reduced,
                )
                .into_any_element()
            }
            None => div()
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(4.))
                .when(!note.unindexed, |d| {
                    d.child(
                        button("memory-archive", "Archive", ButtonKind::Ghost, true, t).on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.memory.confirm = Some(MemoryConfirm::Archive);
                                this.memory.confirm_gen = this.memory.confirm_gen.wrapping_add(1);
                                cx.notify();
                            }),
                        ),
                    )
                })
                .child(
                    button("memory-delete", "Delete", ButtonKind::Danger, true, t).on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.memory.confirm = Some(MemoryConfirm::Delete);
                            this.memory.confirm_gen = this.memory.confirm_gen.wrapping_add(1);
                            cx.notify();
                        }),
                    ),
                )
                .into_any_element(),
        };
        let footer = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(zpx(12.))
            .px(zpx(20.))
            .py(zpx(12.))
            .border_t_1()
            .border_color(c(t.border))
            .child(left)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(8.))
                    .child(
                        button("memory-cancel", "Cancel", ButtonKind::Ghost, true, t).on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.dismiss_memory_modal(cx)
                            }),
                        ),
                    )
                    .child(
                        button(
                            "memory-save",
                            "Save",
                            ButtonKind::Primary,
                            !busy && !loading,
                            t,
                        )
                        .on_click(cx.listener(
                            move |this, _: &ClickEvent, _, cx| {
                                if !loading {
                                    this.memory_modal_action(None, cx)
                                }
                            },
                        )),
                    ),
            );

        let card = div()
            .id(("memory-modal-card", modal_gen as usize))
            .w(zpx(640.))
            .max_w(gpui::relative(0.92))
            .max_h(gpui::relative(0.85))
            .floating_surface(t)
            .rounded(r_xl())
            .flex()
            .flex_col()
            .overflow_hidden()
            .on_click(|_: &ClickEvent, _, cx| cx.stop_propagation())
            .child(header)
            .child(body)
            .child(footer);
        let card = if leaving {
            crate::motion::dismiss(card, ("memory-modal-out", modal_gen as usize), reduced)
        } else {
            crate::motion::appear(
                card,
                ("memory-modal-in", modal_gen as usize),
                crate::motion::BASE,
                10.,
                reduced,
            )
            .into_any_element()
        };
        let scrim = div()
            .id(("memory-modal-scrim", modal_gen as usize))
            .absolute()
            .inset_0()
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.dismiss_memory_modal(cx)))
            .child(card);
        Some(
            gpui::deferred(crate::motion::scrim(
                scrim,
                (
                    if leaving {
                        "memory-scrim-out"
                    } else {
                        "memory-scrim-in"
                    },
                    modal_gen as usize,
                ),
                0.24,
                leaving,
                reduced,
            ))
            .with_priority(3)
            .into_any_element(),
        )
    }
}

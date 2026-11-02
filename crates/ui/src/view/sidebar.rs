//! Sidebar: chats grouped into foldable space sections, attention-ordered inside.

use std::collections::HashMap;
use crate::style::zpx;

use gpui::{AnyElement, ClickEvent, Context, ElementId, IntoElement, div, prelude::*, px};
use keel_theme::Tokens;

use super::{KeelWindow, drag_region, icon_button, menu_item, menu_separator, sidebar_menu, status_dot};
use crate::icons::Icon;
use crate::state::{AppState, SPACE_ROW_CAP, SpaceSection};
use keel_proto::AttentionChat;
use crate::style::{HEADER_HEIGHT, KeelStyled, RADIUS_MD, RADIUS_SM, c};

/// Height of one chat row plus the list gap: 6 + 18 + 2 + 14 + 6, + 1.
/// Rows never wrap (ellipsis), so the pitch is constant.
const CHAT_ROW_PITCH: f32 = 47.;
const SECTION_HEADER_HEIGHT: f32 = 28.;
/// Air above every space header but the first.
const SECTION_GAP: f32 = 10.;
/// More air above the "Chats" group, so it reads apart from the folders.
const NO_PROJECT_GAP: f32 = 22.;
const MORE_ROW_HEIGHT: f32 = 26.;
const MORE_ROW_PITCH: f32 = MORE_ROW_HEIGHT + 1.;
const EMPTY_ROW_HEIGHT: f32 = 30.;

fn section_gap(first: bool, loose: bool) -> f32 {
    match (first, loose) {
        (true, _) => 0.,
        (false, true) => NO_PROJECT_GAP,
        (false, false) => SECTION_GAP,
    }
}

fn section_header_pitch(first: bool, loose: bool) -> f32 {
    SECTION_HEADER_HEIGHT + section_gap(first, loose) + 1.
}

/// One entry of the sidebar list, in draw order.
enum SidebarItem {
    ProjectsLabel,
    ProjectHeader(keel_proto::projects::Project, bool, bool),
    SpacesHeader,
    Header(SpaceSection, bool),
    Row(AttentionChat, keel_proto::DisplayStatus),
    /// A space's "Show N more" (0: "Show less").
    More(String, usize),
    /// The "Chats" group with nothing in it.
    Empty(String),
    /// No folders under "Spaces" yet.
    NoSpaces,
}

/// A static placeholder under an empty group ("No chats", "No spaces").
fn empty_row(id: String, icon: Icon, label: &'static str, t: Tokens) -> gpui::Stateful<gpui::Div> {
    div()
        .id(ElementId::Name(id.into()))
        .h(zpx(EMPTY_ROW_HEIGHT))
        .flex_none()
        .px_2()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .t_small()
        .text_color(c(t.text_faint))
        .child(div().flex_none().child(icon.el(zpx(12.))))
        .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(label))
}

/// A small hover action in a space header.
pub(super) fn header_button(id: String, icon: Icon, t: Tokens) -> gpui::Stateful<gpui::Div> {
    div()
        .id(ElementId::Name(id.into()))
        .size(zpx(20.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(RADIUS_SM)
        .text_color(c(t.text_faint))
        .cursor_pointer()
        .hover(|s| s.bg(c(t.hover)).text_color(c(t.text)))
        .child(icon.el(zpx(13.)))
}

impl KeelWindow {
    pub(super) fn render_sidebar(&mut self, t: Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .relative()
            .w(px(self.state.sidebar_width()))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(c(t.sidebar))
            .border_r_1()
            .border_color(c(t.border))
            .child(self.render_sidebar_titlebar(t, cx))
            .child(self.render_sidebar_body(t, cx))
            .child(self.render_resize_handle(t, cx))
    }

    /// The collapsed sidebar, peeking: a floating card under the header pill
    /// that slides in from the left. It overlays the content instead of
    /// pushing it, and closes when the pointer leaves.
    pub(super) fn render_sidebar_peek(&mut self, t: Tokens, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        use gpui::AnimationExt as _;
        if !((self.sidebar_peek || self.peek_closing) && self.state.ui_settings.sidebar_collapsed) {
            return None;
        }
        // In: 160 ms, decelerating. Out: 120 ms, accelerating away, so it
        // leaves quicker than it came.
        let leaving = self.peek_closing;
        let duration = match (self.reduced(), leaving) {
            (true, _) => std::time::Duration::from_millis(1),
            (false, false) => crate::motion::BASE,
            (false, true) => crate::motion::FAST,
        };
        Some(
            div()
                .id("sidebar-peek")
                .absolute()
                .top(HEADER_HEIGHT)
                .bottom(zpx(8.))
                .w(px(self.state.sidebar_width()))
                .occlude()
                .floating_surface(t)
                .bg(c(t.sidebar))
                .overflow_hidden()
                .flex()
                .flex_col()
                .pt_1()
                .on_hover(cx.listener(|this, hovered: &bool, _, cx| this.peek_hover(*hovered, cx)))
                .child(self.render_sidebar_body(t, cx))
                .with_animation(
                    ("sidebar-peek", self.peek_gen as usize),
                    gpui::Animation::new(duration)
                        .with_easing(if leaving { crate::motion::ease_in } else { crate::motion::ease_out }),
                    move |el, delta| {
                        let shown = if leaving { 1. - delta } else { delta };
                        el.left(px(8. - 16. * (1. - shown))).opacity(shown)
                    },
                ),
        )
    }

    /// Filter, prompts, the chat list and the selection bar: everything but
    /// the titlebar row and the resize grip, shared by docked and peeking.
    fn render_sidebar_body(&mut self, t: Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(self.render_sidebar_nav(t, cx))
            .children(self.render_rename_prompt(t, cx))
            .child(self.render_chat_list(t, cx))
            .children(self.render_selection_bar(t, cx))
    }

    /// Top row: room for the traffic lights, stationary collapse button, and window drag.
    fn render_sidebar_titlebar(&mut self, _t: Tokens, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .h(HEADER_HEIGHT)
            .flex_none()
            .pl(self.traffic_lights_inset() + zpx(26.))
            .pr_2()
            .flex()
            .flex_row()
            .items_center()
            .child(drag_region("sidebar-drag"))
    }

    /// A thin grab strip on the right edge; the drag itself is tracked on the root.
    fn render_resize_handle(&mut self, t: Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.resizing_sidebar;
        div()
            .id("sidebar-resize-handle")
            .absolute()
            .top_0()
            .bottom_0()
            .right(zpx(-3.))
            .w(zpx(6.))
            .cursor_col_resize()
            .flex()
            .justify_center()
            .child(
                div()
                    .w(zpx(2.))
                    .h_full()
                    .when(active, |d| d.bg(c(t.accent)))
                    .when(!active, |d| d.hover(|s| s.bg(c(t.border)))),
            )
            .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                cx.stop_propagation();
                if event.click_count >= 2 {
                    // Double-click resets the width.
                    this.state.ui_settings.sidebar_width = None;
                    this.state.save_settings();
                } else {
                    this.resizing_sidebar = true;
                }
                cx.notify();
            }))
    }

    /// "Spaces" over the sections, or a way back out of a focused space;
    /// the app's overflow menu on the right.
    fn render_sidebar_header(&mut self, t: Tokens, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let focused = self.state.space_filter.is_some();
        div()
            .id("spaces-header-group")
            .h(zpx(32.))
            .flex_none()
            .px_2()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .when(!focused, |d| {
                d.child(
                    div()
                        .px_2()
                        .t_caption()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(c(t.text_faint))
                        .child("Spaces"),
                )
            })
            .when(focused, |d| {
                d.child(
                    div()
                        .id("unfocus-space-btn")
                        .px_2()
                        .py_1()
                        .rounded(RADIUS_MD)
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .cursor_pointer()
                        .t_small()
                        .text_color(c(t.text_muted))
                        .hover(|s| s.bg(c(t.hover)).text_color(c(t.text)))
                        .child(Icon::ChevronDown.el(zpx(12.)).with_transformation(gpui::Transformation::rotate(gpui::percentage(0.25))))
                        .child("All spaces")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.state.space_filter = None;
                            cx.notify();
                        })),
                )
            })
            .child(
                div()
                    .relative()
                    .child(icon_button("overflow-btn", Icon::Ellipsis, t).on_click(cx.listener(
                        |this, _: &ClickEvent, _, cx| {
                            if this.menu_just_dismissed("overflow-menu") {
                                return;
                            }
                            this.state.show_overflow_menu = !this.state.show_overflow_menu;
                            this.state.space_menu = None;
                            cx.notify();
                        },
                    )))
                    .children(self.menu_phase("overflow-menu", self.state.show_overflow_menu, cx).map(|leaving| self.render_overflow_menu(leaving, t, cx))),
            )
    }

    /// A space header's ⋯: new chat here, focus, rename, delete.
    fn render_space_menu(&mut self, space_id: String, leaving: Option<u64>, t: Tokens, cx: &mut Context<Self>) -> AnyElement {
        let focused = self.state.space_filter.is_some();
        let (new_id, focus_id, ren, del) = (space_id.clone(), space_id.clone(), space_id.clone(), space_id);
        let items: Vec<AnyElement> = vec![
            menu_item("space-new-chat", t, true, false)
                .child("New chat")
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.state.space_menu = None;
                    this.new_chat_in_space(new_id.clone(), cx);
                }))
                .into_any_element(),
            menu_item("space-focus", t, true, false)
                .child(if focused { "Show all spaces" } else { "Show only this space" })
                .child(div().flex_1())
                .when(!focused, |d| d.child(div().flex_none().whitespace_nowrap().t_caption().text_color(c(t.text_faint)).child("⌥-click")))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.state.space_menu = None;
                    this.state.space_filter = if focused { None } else { Some(focus_id.clone()) };
                    cx.notify();
                }))
                .into_any_element(),
            menu_separator(t).into_any_element(),
            menu_item("rename-space-action", t, true, false)
                .child("Rename…")
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.start_rename(ren.clone(), window, cx);
                }))
                .into_any_element(),
            menu_item("delete-space-action", t, true, false)
                .text_color(c(t.danger))
                .child("Delete…")
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.state.confirming_delete_space = Some(del.clone());
                    this.state.space_menu = None;
                    cx.notify();
                }))
                .into_any_element(),
        ];
        let dismiss = cx.listener(|this, _: &gpui::MouseDownEvent, _, cx| {
            this.note_menu_dismissed("space-menu");
            this.state.space_menu = None;
            cx.notify();
        });
        sidebar_menu("space-menu", t, 240., leaving, self.reduced(), dismiss, items)
    }

    /// The new-chat screen, set to start in `space_id`.
    fn new_chat_in_space(&mut self, space_id: String, cx: &mut Context<Self>) {
        self.state.new_chat_space = Some(space_id);
        self.create_new_session(None, cx);
    }

    fn render_overflow_menu(&mut self, leaving: Option<u64>, t: Tokens, cx: &mut Context<Self>) -> AnyElement {
        let in_select = self.select_mode || !self.selected_chats.is_empty();
        let mut items: Vec<AnyElement> = vec![
            menu_item("select-sessions", t, true, false)
                .child(if in_select { "Exit selection" } else { "Select sessions…" })
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.state.show_overflow_menu = false;
                    if this.select_mode || !this.selected_chats.is_empty() {
                        this.exit_select_mode(cx);
                    } else {
                        this.enter_select_mode(cx);
                    }
                }))
                .into_any_element(),
        ];
        if let Some(chat_id) = self.state.active_tab.clone() {
            items.push(menu_separator(t).into_any_element());
            items.push(
                menu_item("archive-active-chat", t, true, false)
                    .child("Archive this chat")
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.state.show_overflow_menu = false;
                        this.archive_chat(chat_id.clone(), cx);
                    }))
                    .into_any_element(),
            );
            items.push(
                menu_item("track-mr", t, true, false)
                    .child("Track merge request…")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.state.show_overflow_menu = false;
                        this.mr_open_prompt(window, cx);
                    }))
                    .into_any_element(),
            );
        }
        items.push(menu_separator(t).into_any_element());
        items.push(
            menu_item("open-settings", t, true, false)
                .child("Settings…")
                .child(div().flex_1())
                .child(div().t_caption().text_color(c(t.text_faint)).child("⌘,"))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_settings(cx)))
                .into_any_element(),
        );
        let dismiss = cx.listener(|this, _: &gpui::MouseDownEvent, _, cx| {
            this.note_menu_dismissed("overflow-menu");
            this.state.show_overflow_menu = false;
            cx.notify();
        });
        sidebar_menu("overflow-menu", t, 200., leaving, self.reduced(), dismiss, items)
    }

    fn render_rename_prompt(&mut self, t: Tokens, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let (_, current) = self.state.renaming_space.clone()?;
        Some(
            inline_card(t, t.border)
                .child(div().t_small().text_color(c(t.text_muted)).child(format!("Rename “{current}”")))
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded(RADIUS_SM)
                        .bg(c(t.input))
                        .border_1()
                        .border_color(c(t.focus))
                        .t_body()
                        .child(self.rename_input.clone()),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap_1()
                        .child(text_button("cancel-rename-btn", "Cancel", t).on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| {
                                this.state.renaming_space = None;
                                cx.notify();
                            },
                        )))
                        .child(filled_button("confirm-rename-btn", "Save", t.accent, t).on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| this.save_rename(cx),
                        ))),
                ),
        )
    }

    pub(super) fn render_delete_prompt_card(&mut self, t: Tokens, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let space_id = self.state.confirming_delete_space.clone()?;
        let name = self.state.spaces.get(&space_id).map(|s| s.name.clone()).unwrap_or_default();
        Some(
            div()
                .id("delete-space-card")
                .w(zpx(360.))
                .max_w(gpui::relative(0.9))
                .p_4()
                .rounded(crate::style::RADIUS_LG)
                .bg(c(t.surface))
                .border_1()
                .border_color(c(t.border))
                .floating_surface(t)
                .flex()
                .flex_col()
                .gap_3()
                .on_click(|_: &ClickEvent, _, cx| cx.stop_propagation())
                .child(div().t_title().font_weight(gpui::FontWeight::SEMIBOLD).child(format!("Delete “{name}”?")))
                .child(
                    div()
                        .t_small()
                        .text_color(c(t.text_muted))
                        .child("Its chats are removed. Files on disk stay."),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap_1()
                        .child(text_button("cancel-delete-space-btn", "Cancel", t).on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| {
                                this.state.confirming_delete_space = None;
                                cx.notify();
                            },
                        )))
                        .child(filled_button("confirm-delete-space-btn", "Delete", t.danger, t).on_click(cx.listener(
                            move |this, _: &ClickEvent, _, cx| this.delete_space(space_id.clone(), cx),
                        ))),
                ),
        )
    }

    /// Pages above the chats: GitLab.
    fn render_sidebar_nav(&mut self, t: Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.gitlab_open;
        div().px_2().pb_1().flex().flex_col().child(
            div()
                .id("nav-gitlab")
                .h(zpx(30.))
                .px_2()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .rounded(RADIUS_MD)
                .cursor_pointer()
                .t_body()
                .text_color(c(if active { t.text } else { t.text_muted }))
                .when(active, |d| d.bg(c(t.selected)).font_weight(gpui::FontWeight::MEDIUM))
                .when(!active, |d| d.hover(|s| s.bg(c(t.hover)).text_color(c(t.text))))
                .child(Icon::GitLab.el(zpx(14.)))
                .child("GitLab")
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.open_gitlab(cx))),
        )
    }

    /// While chats are selected: how many, Clear, Done, Delete.
    fn render_selection_bar(&mut self, t: Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.selected_chats.is_empty() && !self.select_mode {
            return None;
        }
        let leaving = self.select_leaving.is_some();
        let count = self.selected_chats.len();
        let bar_n = self.select_mode_gen as usize;
        let reduced = self.reduced();
        let bar = div()
            .flex_none()
            .mx_2()
            .mb_2()
            .pl_3()
            .pr_1()
            .h(zpx(36.))
            .rounded(crate::style::RADIUS_LG)
            .bg(c(t.surface))
            .border_1()
            .border_color(c(t.border))
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .child(
                div()
                    .flex_1()
                    .t_small()
                    .text_color(c(t.text_muted))
                    .child(if count > 0 { format!("{count} selected") } else { "Select sessions".into() }),
            )
            .when(count > 0, |d| {
                d.child(text_button("clear-selection-btn", "Clear", t).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.selected_chats.clear();
                    this.select_anchor = None;
                    cx.notify();
                })))
            })
            .child(text_button("done-selection-btn", "Done", t).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.exit_select_mode(cx);
            })))
            .when(count > 0, |d| {
                d.child(filled_button("delete-selected-btn", "Delete", t.danger, t).on_click(cx.listener(
                    |this, _: &ClickEvent, _, cx| {
                        this.ask_delete_chats(this.selected_chats.clone(), cx);
                    },
                )))
            });
        Some(if leaving {
            crate::motion::dismiss(bar, ("selection-bar-out", bar_n), reduced)
        } else {
            crate::motion::appear(bar, ("selection-bar", bar_n), crate::motion::BASE, 10., reduced).into_any_element()
        })
    }

    fn render_chat_list(&mut self, t: Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let now = AppState::now_ms();
        let skip = self.linked_chat_ids();
        let sections = self.state.sidebar_sections_skipping(now, &skip);
        let empty = sections.iter().all(|s| s.total == 0)
            && self.projects.store.projects.iter().all(|p| self.project_chats(&p.id).is_empty());
        let focused = self.state.space_filter.is_some();
        let order: std::rc::Rc<Vec<String>> = std::rc::Rc::new(self.sidebar_row_ids());

        // Lay the list out as keyed items with known heights, so a fold or a
        // re-sort can glide everything below from where it was drawn.
        let mut items: Vec<(String, f32, SidebarItem)> = Vec::new();
        let show_projects = !focused && !self.projects.store.projects.is_empty();
        if show_projects {
            items.push(("l:projects".into(), 32. + 1., SidebarItem::ProjectsLabel));
            for (p_ix, project) in self.projects.store.projects.clone().into_iter().enumerate() {
                let p_id = project.id.clone();
                let open = self.projects.unfolded.contains(&p_id);
                let first = p_ix == 0;
                let pitch = SECTION_HEADER_HEIGHT + if first { 0. } else { 10. } + 1.;
                items.push((format!("p:{p_id}"), pitch, SidebarItem::ProjectHeader(project, open, first)));
                for (chat, status) in self.visible_project_chats(&p_id) {
                    items.push((chat.id.clone(), CHAT_ROW_PITCH, SidebarItem::Row(chat, status)));
                }
            }
        }

        items.push(("h:spaces-header".into(), 32. + 1., SidebarItem::SpacesHeader));
        // Under "Spaces", the same placeholder "Chats" gets when it's empty.
        let no_spaces = !focused && sections.iter().all(|s| s.loose);
        if no_spaces {
            items.push(("e:spaces".into(), EMPTY_ROW_HEIGHT + 1., SidebarItem::NoSpaces));
        }
        for (ix, section) in sections.into_iter().enumerate() {
            let space_id = section.space_id.clone();
            let first = ix == 0 && !no_spaces;
            items.push((format!("h:{space_id}"), section_header_pitch(first, section.loose), SidebarItem::Header(section.clone(), first)));
            for (chat, status) in section.rows {
                items.push((chat.id.clone(), CHAT_ROW_PITCH, SidebarItem::Row(chat, status)));
            }
            if section.loose && section.total == 0 {
                items.push((format!("e:{space_id}"), EMPTY_ROW_HEIGHT + 1., SidebarItem::Empty(space_id.clone())));
            }
            let all_shown = !focused && self.state.space_show_all.contains(&space_id) && section.total > SPACE_ROW_CAP;
            if section.hidden > 0 || all_shown {
                items.push((format!("m:{space_id}"), MORE_ROW_PITCH, SidebarItem::More(space_id, section.hidden)));
            }
        }
        let mut y = 0.;
        let positions: Vec<(String, f32)> = items
            .iter()
            .map(|(key, pitch, _)| {
                let at = y;
                y += pitch;
                (key.clone(), at)
            })
            .collect();
        self.plan_glides(&positions);

        let selecting = self.select_mode || !self.selected_chats.is_empty() || self.select_leaving.is_some();
        let select_leaving = self.select_leaving.is_some();
        let select_gen = self.select_mode_gen;
        let exit_gen = self.rows_exit_gen;
        // While the docked sidebar is opening, rows cascade in. Collapse fades
        // the whole panel in `render_sidebar_animated`, so no per-row exit here.
        let expanding = !self.state.ui_settings.sidebar_collapsed
            && self.sidebar_from == 0.
            && self
                .sidebar_anim
                .as_ref()
                .is_some_and(|(at, _)| at.elapsed() < crate::motion::SHEET_OPEN);
        let enter_gen = self.sidebar_anim.map(|(_, n)| n).unwrap_or(0);
        if expanding {
            // The open cascade is the entrance; nothing else should play after it.
            self.row_glide.clear();
            self.row_enter.clear();
        }
        let reduced = self.reduced();
        let glide_now = std::time::Instant::now();
        let gliding = !reduced && !self.row_glide.is_empty();
        let mut row_ix = 0;
        let children: Vec<AnyElement> = items
            .into_iter()
            .enumerate()
            .map(|(item_ix, (key, _, item))| {
                // A moved item is drawn offset from its slot, easing to 0. A
                // style, not an animation wrapper: the row keeps its identity
                // (hover, clicks) and a second move retargets smoothly.
                let offset = self
                    .row_glide
                    .get(&key)
                    .map_or(0., |&(from, at)| crate::motion::reorder_offset(from, glide_now.saturating_duration_since(at), reduced));
                let el = match item {
                    SidebarItem::ProjectsLabel => super::projects::render_projects_label(t),
                    SidebarItem::ProjectHeader(project, open, first) => self.render_project_header(project, open, first, t, cx),
                    SidebarItem::SpacesHeader => self.render_sidebar_header(t, cx),
                    SidebarItem::Header(section, first) => self.render_space_header(section, first, t, cx),
                    SidebarItem::More(space_id, hidden) => self.render_more_row(space_id, hidden, t, cx),
                    SidebarItem::Empty(space_id) => empty_row(format!("space-empty-row-{space_id}"), Icon::Comment, "No chats", t),
                    SidebarItem::NoSpaces => empty_row("spaces-empty-row".into(), Icon::Folder, "No spaces", t),
                    SidebarItem::Row(chat, status) => {
                        let ix = row_ix;
                        row_ix += 1;
                        self.render_chat_row(&chat, status, ix, order.clone(), selecting, select_gen, select_leaving, t, cx)
                    }
                };
                let el = el.when(offset != 0., |el| el.relative().top(px(offset)));
                if self.rows_exiting.iter().any(|id| id == &key) {
                    crate::motion::vanish(el, ElementId::Name(format!("row-exit-{exit_gen}-{key}").into()), reduced)
                        .into_any_element()
                } else if expanding {
                    crate::motion::stagger_in_light(
                        el,
                        ElementId::Name(format!("sidebar-enter-{enter_gen}-{key}").into()),
                        item_ix,
                        reduced,
                    )
                } else if let Some(&(batch_ix, generation, _)) = self.row_enter.get(&key) {
                    crate::motion::stagger_in(el, ElementId::Name(format!("row-in-{key}-{generation}").into()), batch_ix, reduced)
                } else {
                    el.into_any_element()
                }
            })
            .collect();

        div()
            .id("chat-list")
            .flex_1()
            .overflow_y_scroll()
            .on_scroll_wheel(cx.listener(|this, _, _, cx| {
                this.summary_hover.clear();
                this.projects.folder_card = None;
                cx.notify();
            }))
            .px_2()
            .pb_2()
            .flex()
            .flex_col()
            .gap_px()
            // The "No spaces" placeholder alone still leaves the list empty.
            .when(!show_projects && empty && children.len() <= usize::from(no_spaces) + 1, |d| {
                d.child(
                    div()
                        .px_2()
                        .py_6()
                        .t_small()
                        .text_color(c(t.text_faint))
                        .child("No chats yet. Press + or just start typing."),
                )
            })
            .children(children)
            .when(gliding, |d| {
                d.child(
                    gpui::canvas(|_, _, _| (), |_, _, window, _| window.request_animation_frame())
                        .absolute()
                        .top_0()
                        .left_0()
                        .size(px(0.)),
                )
            })
    }

    /// A space's header: chevron, name, and, folded, its most urgent status
    /// and chat count. Click folds; ⌥-click shows only this space. `+` and
    /// `⋯` appear on hover.
    fn render_space_header(
        &mut self,
        section: SpaceSection,
        first: bool,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let focused = self.state.space_filter.is_some();
        let reduced = self.reduced();
        let SpaceSection { space_id, name, total, urgent, collapsed, loose, .. } = section;
        // "No project" chats get a plain label like "Spaces", not a folder.
        let name = if loose { "Chats".to_string() } else { name };
        let group = format!("space-h-{space_id}");
        let menu_open = self.state.space_menu.as_deref() == Some(&space_id);
        // Folded points right. The turn plays only after a toggle, never on load.
        // Radians: gpui's `percentage` panics outside 0..1.
        let turn = |folded: bool| if folded { -std::f32::consts::FRAC_PI_2 } else { 0. };
        let chevron = Icon::ChevronDown.el(zpx(12.));
        let chevron: AnyElement = match self.space_fold_gen.get(&space_id) {
            Some(&generation) if !reduced => {
                use gpui::AnimationExt as _;
                let (from, to) = (turn(!collapsed), turn(collapsed));
                chevron
                    .with_animation(
                        ElementId::Name(format!("chevron-{space_id}-{generation}").into()),
                        gpui::Animation::new(crate::motion::FAST).with_easing(crate::motion::ease_out),
                        move |icon, delta| {
                            icon.with_transformation(gpui::Transformation::rotate(gpui::radians(from + (to - from) * delta)))
                        },
                    )
                    .into_any_element()
            }
            _ => chevron.with_transformation(gpui::Transformation::rotate(gpui::radians(turn(collapsed)))).into_any_element(),
        };
        let (toggle_id, new_id, menu_id) = (space_id.clone(), space_id.clone(), space_id.clone());
        div()
            .id(ElementId::Name(format!("space-header-{space_id}").into()))
            .flex_none()
            .pt(zpx(section_gap(first, loose)))
            .child(
                div()
                    .id(ElementId::Name(group.clone().into()))
                    .group(group.clone())
                    .h(zpx(SECTION_HEADER_HEIGHT))
                    .pl_1()
                    .pr_1()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .rounded(RADIUS_MD)
                    .when(!focused && !loose, |d| d.cursor_pointer().hover(|s| s.bg(c(t.hover))))
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        if this.state.space_filter.is_some() || loose {
                            return;
                        }
                        if event.modifiers().alt {
                            this.state.space_filter = Some(toggle_id.clone());
                        } else {
                            this.glide_gen += 1;
                            this.space_fold_gen.insert(toggle_id.clone(), this.glide_gen);
                            this.state.toggle_space_collapsed(&toggle_id);
                        }
                        cx.notify();
                    }))
                    .when(!loose, |d| {
                        d.child(
                            div()
                                .size(zpx(16.))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_color(c(t.text_faint))
                                .when(!focused, |d| d.child(chevron)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .t_small()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(c(t.text_muted))
                                .group_hover(group.clone(), |s| s.text_color(c(t.text)))
                                .child(name.clone()),
                        )
                    })
                    // Same as the "Spaces" label above the folders.
                    .when(loose, |d| {
                        d.child(
                            div()
                                .flex_1()
                                .px_1()
                                .t_caption()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(c(t.text_faint))
                                .child(name),
                        )
                    })
                    // Folded: what's inside, at a glance. Waiting chats show
                    // through as rows, so the dot matters most for running.
                    .when(collapsed, |d| {
                        d.children(match urgent {
                            keel_proto::DisplayStatus::AwaitingInput | keel_proto::DisplayStatus::Running => {
                                status_dot(urgent, &format!("space-{space_id}"), t, reduced)
                            }
                            _ => None,
                        })
                        .when(total > 0, |d| {
                            d.child(
                                div()
                                    .flex_none()
                                    .px_1()
                                    .t_caption()
                                    .font_family(crate::style::MONO)
                                    .text_color(c(t.text_faint))
                                    .child(total.to_string()),
                            )
                        })
                    })
                    .child(
                        div()
                            .relative()
                            .flex()
                            .flex_row()
                            .items_center()
                            .opacity(if menu_open { 1. } else { 0. })
                            .group_hover(group, |s| s.opacity(1.))
                            .child(header_button(format!("space-new-{space_id}"), Icon::Plus, t).on_click(cx.listener(
                                move |this, _: &ClickEvent, _, cx| {
                                    cx.stop_propagation();
                                    this.new_chat_in_space(new_id.clone(), cx);
                                },
                            )))
                            // A Keel-made folder: nothing to rename or delete.
                            .when(!loose, |d| d.child(header_button(format!("space-more-{space_id}"), Icon::Ellipsis, t).on_click(cx.listener(
                                move |this, _: &ClickEvent, _, cx| {
                                    cx.stop_propagation();
                                    if this.menu_just_dismissed("space-menu") {
                                        return;
                                    }
                                    let open = this.state.space_menu.as_deref() == Some(&menu_id);
                                    this.state.space_menu = (!open).then(|| menu_id.clone());
                                    this.state.show_overflow_menu = false;
                                    cx.notify();
                                },
                            ))))
                            .children(
                                self.menu_phase(format!("space-menu-{space_id}"), menu_open, cx)
                                    .map(|leaving| self.render_space_menu(space_id.clone(), leaving, t, cx)),
                            ),
                    ),
            )
    }

    /// "Show N more" under a capped space, or "Show less" once it's open.
    fn render_more_row(&mut self, space_id: String, hidden: usize, t: Tokens, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        div()
            .id(ElementId::Name(format!("space-more-row-{space_id}").into()))
            .h(zpx(MORE_ROW_HEIGHT))
            .flex_none()
            // Lines up with the chat titles.
            .pl(zpx(23.))
            .flex()
            .items_center()
            .rounded(RADIUS_MD)
            .cursor_pointer()
            .t_caption()
            .text_color(c(t.text_faint))
            .hover(|s| s.text_color(c(t.text_muted)))
            .child(if hidden > 0 { format!("Show {hidden} more") } else { "Show less".into() })
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                if !this.state.space_show_all.remove(&space_id) {
                    this.state.space_show_all.insert(space_id.clone());
                }
                cx.notify();
            }))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_chat_row(
        &mut self,
        chat: &AttentionChat,
        status: keel_proto::DisplayStatus,
        ix: usize,
        order: std::rc::Rc<Vec<String>>,
        selecting: bool,
        select_gen: u64,
        select_leaving: bool,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let (open_id, archive_id, delete_id, right_click_id) = (chat.id.clone(), chat.id.clone(), chat.id.clone(), chat.id.clone());
        let active = self.state.active_tab.as_deref() == Some(&chat.id);
        let selected = self.selected_chats.contains(&chat.id);
        let title = self
            .state
            .chats
            .get(&chat.id)
            .map(|c| c.title.clone())
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| "New chat".to_string());
        let model = self
            .state
            .chats
            .get(&chat.id)
            .filter(|c| !c.model.is_empty())
            .map(|c| self.model_name(&c.harness, &c.model).unwrap_or_else(|| c.model.clone()));
        let git = self.git_badge(&chat.id);
        let mr_badge = self.mr_badge(&chat.id, t);
        let group = format!("chat-row-{}", chat.id);
        let chat_id = chat.id.clone();
        div()
            .id(ElementId::Name(group.clone().into()))
            .group(group.clone())
            .when(!selecting, |d| {
                let cid = chat_id.clone();
                d.on_hover(cx.listener(move |this, hovered: &bool, window, cx| {
                    let mouse_y = f32::from(window.mouse_position().y);
                    this.chat_summary_hover(cid.clone(), mouse_y, *hovered, cx);
                }))
            })
            .px_2()
            .py_1p5()
            .rounded(RADIUS_MD)
            .flex()
            .flex_col()
            .gap_0p5()
            .cursor_pointer()
            .when(active || selected, |s| s.bg(c(t.selected)))
            .when(!active && !selected, |s| s.hover(|s| s.bg(c(t.hover))))
            // ⌘-click toggles, ⇧-click selects a range; while in select mode or
            // anything is selected a plain click toggles too. Otherwise it opens.
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                this.chat_summary_press(&open_id, cx);
                let m = event.modifiers();
                let click = Click { toggle: m.secondary(), range: m.shift };
                let in_select = this.select_mode || !this.selected_chats.is_empty();
                match next_selection(&this.selected_chats, this.select_anchor.as_deref(), &order, &open_id, click, in_select) {
                    Some((selection, anchor)) => {
                        let had = this.selected_chats.len();
                        if had > 0 && selection.is_empty() && !this.select_mode {
                            // Last item unchecked: play the exit, don't snap off.
                            this.clear_selection(cx);
                        } else {
                            this.selected_chats = selection;
                            this.select_anchor = anchor;
                            this.note_selection_changed(had, cx);
                        }
                    }
                    None => {
                        this.close_menus(cx);
                        this.close_pages();
                        this.finish_settings_exit(cx);
                        this.state.open_or_focus_tab(&open_id);
                    }
                }
                cx.notify();
            }))
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(move |this, _: &gpui::MouseDownEvent, _, cx| {
                    this.chat_summary_press(&right_click_id, cx);
                    if !this.select_mode {
                        this.enter_select_mode(cx);
                    }
                    if !this.selected_chats.contains(&right_click_id) {
                        let had = this.selected_chats.len();
                        this.selected_chats.push(right_click_id.clone());
                        this.select_anchor = Some(right_click_id.clone());
                        this.note_selection_changed(had, cx);
                    }
                    cx.notify();
                }),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .when(selecting, |d| {
                        d.child(check_circle(
                            selected,
                            t,
                            &chat.id,
                            select_gen,
                            ix,
                            select_leaving,
                            self.reduced(),
                        ))
                    })
                    .children(status_dot(status, &chat.id, t, self.reduced()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .t_body()
                            .font_weight(if active { gpui::FontWeight::MEDIUM } else { gpui::FontWeight::NORMAL })
                            .text_color(c(if active { t.text } else { t.text_muted }))
                            .child(title),
                    )
                    .children(self.unread_mark(&chat.id, t))
                    .child(
                        div()
                            .id(ElementId::Name(format!("archive-btn-{}", chat.id).into()))
                            .size(zpx(18.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(RADIUS_SM)
                            .text_color(c(t.text_faint))
                            .opacity(0.)
                            .group_hover(group.clone(), |s| s.opacity(1.))
                            .hover(|s| s.bg(c(t.hover)).text_color(c(t.text)))
                            .child(Icon::Archive.el(zpx(12.)))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                // Don't let the row's click reopen the tab.
                                cx.stop_propagation();
                                this.chat_summary_press(&archive_id, cx);
                                this.archive_chat(archive_id.clone(), cx);
                            })),
                    )
                    .child(
                        div()
                            .id(ElementId::Name(format!("delete-btn-{}", chat.id).into()))
                            .size(zpx(18.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(RADIUS_SM)
                            .text_color(c(t.text_faint))
                            .opacity(0.)
                            .group_hover(group, |s| s.opacity(1.))
                            .hover(|s| s.bg(c(t.hover)).text_color(c(t.danger)))
                            .child(Icon::Trash.el(zpx(12.)))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                this.chat_summary_press(&delete_id, cx);
                                this.ask_delete_chats(vec![delete_id.clone()], cx);
                            })),
                    ),
            )
            .child(
                div()
                    // Fixed, so the row pitch holds with an empty line.
                    .h(zpx(14.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .t_caption()
                    .text_color(c(t.text_faint))
                    .children(model.map(|name| div().flex_none().max_w(zpx(140.)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(name)))
                    // The space is the section header now; the branch gets the room.
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .font_family(crate::style::MONO)
                            .children(git),
                    )
                    .children(mr_badge),
            )
    }

    /// Compare this frame's layout with the last one. An item that moved
    /// glides from where it was drawn; an item that just appeared (a space
    /// or project unfolding, "Show more", a new chat) fades in, cascading.
    /// A filter change redraws the list without motion.
    fn plan_glides(&mut self, items: &[(String, f32)]) {
        let filter = Some(self.state.space_filter.clone());
        let filter_matches = self.row_order_filter.as_ref() == filter.as_ref();
        let reduced = self.reduced();
        plan_glides_map(
            items,
            filter_matches,
            reduced,
            &mut self.row_order,
            &mut self.row_glide,
            &mut self.row_enter,
            &mut self.glide_gen,
        );
        self.row_order_filter = filter;
    }
}

pub(super) fn plan_glides_map(
    items: &[(String, f32)],
    filter_matches: bool,
    reduced: bool,
    row_order: &mut HashMap<String, f32>,
    row_glide: &mut HashMap<String, (f32, std::time::Instant)>,
    row_enter: &mut HashMap<String, (usize, u64, std::time::Instant)>,
    glide_gen: &mut u64,
) {
    let now = std::time::Instant::now();
    if filter_matches && !row_order.is_empty() {
        let mut entering = 0;
        for (key, y) in items {
            match row_order.get(key) {
                Some(&old) if (old - y).abs() > 0.5 => {
                    // Start from where the row is drawn now (mid-glide
                    // included), so a retarget never jumps.
                    let drawn = old
                        + row_glide.get(key).map_or(0., |&(from, at)| {
                            crate::motion::reorder_offset(from, now.saturating_duration_since(at), reduced)
                        });
                    row_glide.insert(key.clone(), (drawn - y, now));
                    row_enter.remove(key);
                }
                Some(_) => {}
                None => {
                    *glide_gen += 1;
                    row_enter.insert(key.clone(), (entering, *glide_gen, now));
                    row_glide.remove(key);
                    entering += 1;
                }
            }
        }
    } else {
        row_glide.clear();
        row_enter.clear();
    }
    let order: HashMap<String, f32> = items.iter().cloned().collect();
    // Forget motion that has played (stagger: 35 ms apart, capped at 10).
    let enter_done = crate::motion::BASE + std::time::Duration::from_millis(350);
    row_glide.retain(|id, (_, at)| order.contains_key(id) && now.saturating_duration_since(*at) < crate::motion::REORDER);
    row_enter.retain(|id, (.., at)| order.contains_key(id) && at.elapsed() < enter_done);
    *row_order = order;
}

impl KeelWindow {

    /// `branch +a -d`, or whichever half is known.
    fn git_badge(&self, chat_id: &str) -> Option<String> {
        let chat = self.state.chats.get(chat_id)?;
        let diff = (!chat.checkout_id.is_empty())
            .then(|| self.state.diffs.get(&chat.checkout_id))
            .flatten()
            .filter(|d| d.additions > 0 || d.deletions > 0)
            .map(|d| format!("+{} −{}", d.additions, d.deletions));
        match (chat.branch.is_empty(), diff) {
            (false, Some(diff)) => Some(format!("{} {diff}", chat.branch)),
            (false, None) => Some(chat.branch.clone()),
            (true, diff) => diff,
        }
    }
}

/// Modifiers on a sidebar row click.
#[derive(Clone, Copy, Default)]
struct Click {
    toggle: bool,
    range: bool,
}

/// The selection after clicking `id`, with the new range anchor; `None`
/// means "just open it" (a plain click with nothing selected).
fn next_selection(
    selected: &[String],
    anchor: Option<&str>,
    order: &[String],
    id: &str,
    click: Click,
    in_select_mode: bool,
) -> Option<(Vec<String>, Option<String>)> {
    if click.range {
        let anchor = anchor.filter(|a| order.iter().any(|o| o == a)).unwrap_or(id);
        let (a, b) = (order.iter().position(|o| o == anchor)?, order.iter().position(|o| o == id)?);
        let mut next = selected.to_vec();
        for chat in &order[a.min(b)..=a.max(b)] {
            if !next.contains(chat) {
                next.push(chat.clone());
            }
        }
        return Some((next, Some(anchor.to_string())));
    }
    if !click.toggle && !in_select_mode && selected.is_empty() {
        return None;
    }
    let mut next = selected.to_vec();
    match next.iter().position(|s| s == id) {
        Some(ix) => {
            next.remove(ix);
        }
        None => next.push(id.to_string()),
    }
    Some((next, Some(id.to_string())))
}

/// Selection-mode checkbox at a row's left.
fn check_circle(
    checked: bool,
    t: Tokens,
    id: &str,
    enter_gen: u64,
    index: usize,
    leaving: bool,
    reduced: bool,
) -> AnyElement {
    use gpui::AnimationExt as _;
    let circle = div()
        .size(zpx(14.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .border_color(c(if checked { t.accent } else { t.text_faint }))
        .when(checked, |d| {
            d.bg(c(t.accent))
                .text_color(c(t.text_on_accent))
                .child(Icon::Check.el(zpx(9.)))
        });
    let eid = |kind: &str| ElementId::Name(format!("{kind}-{enter_gen}-{id}").into());
    // Checked pop, then enter/leave. Always go through AnyElement so the
    // branches share a type (AnimationElement vs Div is a mismatch).
    let circle: AnyElement = if checked && !reduced {
        circle
            .with_animation(
                eid("check-on"),
                gpui::Animation::new(crate::motion::FAST).with_easing(crate::motion::ease_out),
                |el, delta| el.opacity(0.55 + 0.45 * delta),
            )
            .into_any_element()
    } else {
        circle.into_any_element()
    };
    if leaving {
        crate::motion::dismiss(div().child(circle), eid("check-out"), reduced)
    } else {
        let delay_ix = index.min(8);
        crate::motion::slide_x(
            div().child(circle),
            eid("check-in"),
            -10. - delay_ix as f32,
            crate::motion::BASE,
            reduced,
        )
    }
}

fn inline_card(t: Tokens, edge: u32) -> gpui::Div {
    div()
        .mx_2()
        .mb_2()
        .p_2()
        .rounded(crate::style::RADIUS_LG)
        .bg(c(t.surface))
        .border_1()
        .border_color(c(edge))
        .flex()
        .flex_col()
        .gap_2()
}

pub(super) fn text_button(id: &'static str, label: &'static str, t: Tokens) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px_2p5()
        .py_1()
        .rounded(RADIUS_MD)
        .t_body()
        .text_color(c(t.text_muted))
        .cursor_pointer()
        .hover(|s| s.bg(c(t.hover)).text_color(c(t.text)))
        .child(label)
}

pub(super) fn filled_button(id: &'static str, label: &'static str, fill: u32, t: Tokens) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px_2p5()
        .py_1()
        .rounded(RADIUS_MD)
        .bg(c(fill))
        .t_body()
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(c(t.text_on_accent))
        .cursor_pointer()
        .hover(|s| s.opacity(0.9))
        .child(label)
}


impl KeelWindow {
    /// A chat's unread alerts in its row, per Settings → Notifications → Badge:
    /// a count pill or a dot (amber while it waits on you, red after an error).
    fn unread_mark(&self, chat_id: &str, t: Tokens) -> Option<gpui::AnyElement> {
        use crate::notify::{NotifyEvent, RowBadge};
        let badge = self.unread.row_badge(chat_id, self.state.ui_settings.notifications.badge)?;
        let color = match self.unread.latest(chat_id) {
            Some(NotifyEvent::NeedsInput) => t.warning,
            Some(NotifyEvent::Error) => t.danger,
            _ => t.accent,
        };
        Some(match badge {
            RowBadge::Dot => div().size(zpx(7.)).flex_none().rounded_full().bg(c(color)).into_any_element(),
            RowBadge::Count(n) => div()
                .flex_none()
                .min_w(zpx(16.))
                .h(zpx(16.))
                .px(zpx(4.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(c(color))
                .t_caption()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(c(t.text_on_accent))
                .child(if n > 99 { "99+".to_string() } else { n.to_string() })
                .into_any_element(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_clicks_open_toggle_and_select_ranges() {
        let order: Vec<String> = ["a", "b", "c", "d"].map(String::from).to_vec();
        let plain = Click::default();
        let cmd = Click { toggle: true, range: false };
        let shift = Click { toggle: false, range: true };
        // Nothing selected and not in select mode: a plain click opens.
        assert!(next_selection(&[], None, &order, "b", plain, false).is_none());
        // In select mode: a plain click toggles even with nothing selected.
        let (sel, anchor) = next_selection(&[], None, &order, "b", plain, true).unwrap();
        assert_eq!((sel.clone(), anchor.as_deref()), (vec!["b".to_string()], Some("b")));
        // ⌘ starts a selection; then plain clicks toggle.
        let (sel, anchor) = next_selection(&[], None, &order, "b", cmd, false).unwrap();
        assert_eq!((sel.clone(), anchor.as_deref()), (vec!["b".to_string()], Some("b")));
        let (sel, _) = next_selection(&sel, Some("b"), &order, "d", plain, false).unwrap();
        assert_eq!(sel, ["b", "d"]);
        let (sel, _) = next_selection(&sel, Some("d"), &order, "b", plain, false).unwrap();
        assert_eq!(sel, ["d"]);
        // ⇧ fills from the anchor, either direction, without duplicates.
        let (sel, _) = next_selection(&sel, Some("d"), &order, "b", shift, false).unwrap();
        assert_eq!(sel, ["d", "b", "c"]);
    }

    #[test]
    fn project_and_space_glides_are_unified() {
        let mut row_order = HashMap::new();
        let mut row_glide = HashMap::new();
        let mut row_enter = HashMap::new();
        let mut glide_gen = 0u64;

        // Frame 1: Project 1 open with 2 chats, followed by Spaces header and a space chat
        let initial_items = vec![
            ("l:projects".to_string(), 0.0),
            ("p:p1".to_string(), 33.0),
            ("c1".to_string(), 62.0),
            ("c2".to_string(), 109.0),
            ("h:spaces-header".to_string(), 156.0),
            ("h:s1".to_string(), 189.0),
            ("c3".to_string(), 218.0),
        ];
        plan_glides_map(&initial_items, true, false, &mut row_order, &mut row_glide, &mut row_enter, &mut glide_gen);
        assert!(row_glide.is_empty());
        assert!(row_enter.is_empty());

        // Frame 2: Project 1 folds -> c1 and c2 are hidden
        // h:spaces-header moves from 156 to 62 (drawn - new = 94.0)
        // h:s1 moves from 189 to 95 (drawn - new = 94.0)
        // c3 moves from 218 to 124 (drawn - new = 94.0)
        let folded_items = vec![
            ("l:projects".to_string(), 0.0),
            ("p:p1".to_string(), 33.0),
            ("h:spaces-header".to_string(), 62.0),
            ("h:s1".to_string(), 95.0),
            ("c3".to_string(), 124.0),
        ];
        plan_glides_map(&folded_items, true, false, &mut row_order, &mut row_glide, &mut row_enter, &mut glide_gen);
        assert!(!row_order.contains_key("c1"));
        assert!(!row_order.contains_key("c2"));

        let (spaces_header_offset, _) = *row_glide.get("h:spaces-header").unwrap();
        assert!((spaces_header_offset - 94.0).abs() < 1e-4);
        let (s1_offset, _) = *row_glide.get("h:s1").unwrap();
        assert!((s1_offset - 94.0).abs() < 1e-4);
        let (c3_offset, _) = *row_glide.get("c3").unwrap();
        assert!((c3_offset - 94.0).abs() < 1e-4);

        // Simulate completion of the fold glide
        row_glide.clear();

        // Frame 3: Project 1 unfolds -> c1 and c2 enter, Spaces and below glide down (-94.0)
        plan_glides_map(&initial_items, true, false, &mut row_order, &mut row_glide, &mut row_enter, &mut glide_gen);
        assert_eq!(row_enter.get("c1").map(|&(ix, ..)| ix), Some(0));
        assert_eq!(row_enter.get("c2").map(|&(ix, ..)| ix), Some(1));

        let (spaces_header_offset_down, _) = *row_glide.get("h:spaces-header").unwrap();
        assert!((spaces_header_offset_down - (-94.0)).abs() < 1e-4);
        let (s1_offset_down, _) = *row_glide.get("h:s1").unwrap();
        assert!((s1_offset_down - (-94.0)).abs() < 1e-4);
        let (c3_offset_down, _) = *row_glide.get("c3").unwrap();
        assert!((c3_offset_down - (-94.0)).abs() < 1e-4);
    }
}

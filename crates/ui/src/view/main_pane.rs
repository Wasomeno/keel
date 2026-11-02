//! Window layout: sidebar | (tab strip, transcript, notice, composer).

use gpui::{
    ClickEvent, Context, ElementId, IntoElement, Render, ScrollWheelEvent, Window, WindowAppearance, div, list,
    prelude::*, px,
};
use keel_proto::ChatStatus;
use keel_theme::{Tokens, resolve_syntax_theme};
use crate::style::zpx;

use super::{KeelWindow, drag_region, editor_style, icon_button};
use crate::icons::Icon;
use crate::style::{
    HEADER_HEIGHT, KeelStyled, MONO, RADIUS_MD, RADIUS_SM, SANS, apply_base_theme, c,
};
use crate::transcript::{RowAction, RowContext, render_row};

impl Render for KeelWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.frame_now.set(Some(std::time::Instant::now()));
        self.sync_active_chat(window, cx);
        self.sync_prompt_suggestion(window, cx);
        self.fullscreen = window.is_fullscreen();
        self.mr_note_window_active(window.is_window_active(), cx);

        let system_dark = matches!(window.appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark);
        let t = self.state.effective_tokens(system_dark);
        // Live: follows the overflow-menu override; the OS value is cached.
        self.transcript.reduced_motion = self.state.effective_reduced_motion(self.os_reduced_motion);
        super::tooltip::set_reduced(self.transcript.reduced_motion);

        if self.input_style_for != Some(t) {
            self.input_style_for = Some(t);
            apply_base_theme(t, cx);
            let style = editor_style(t);
            for input in [&self.composer, &self.question_input, &self.dotify_desc] {
                input.update(cx, |s, cx| {
                    s.set_editor_style(style.clone());
                    cx.notify();
                });
            }
            if let Some(editor) = self.files.editor() {
                let code = super::files_panel::code_editor_style(t, resolve_syntax_theme(self.state.ui_settings.theme, system_dark));
                editor.update(cx, |s, cx| {
                    s.set_editor_style(code);
                    cx.notify();
                });
            }
            self.picker_filter.update(cx, |s, cx| {
                s.set_editor_style(style.clone());
                cx.notify();
            });
            for input in [&self.rename_input, &self.gitlab_host, &self.browser_sites, &self.gitlab_token, &self.dotify_token, &self.dotify_search, &self.gitlab_filter, &self.mr_input, &self.browser.address] {
                input.update(cx, |s, cx| {
                    s.set_editor_style(style.clone());
                    cx.notify();
                });
            }
        }

        let panel_needs_text = self
            .current_chat
            .as_ref()
            .and_then(|c| self.question_panels.get(c))
            .map(|p| p.needs_text());
        let stopped = self.current_chat.as_ref().is_some_and(|c| self.question_stopped.contains(c));
        let show_question = self.current_chat.is_some()
            && (panel_needs_text.is_some()
                || (!stopped && self.active_status() == Some(ChatStatus::AwaitingInput))
                || self.question_exit_showing());
        if self.focus_question && show_question {
            self.focus_question = false;
            if panel_needs_text == Some(true) {
                self.question_input.update(cx, |s, cx| s.focus(window, cx));
            } else {
                window.focus(&self.focus_handle, cx);
            }
        }

        let reduced = self.reduced();

        // New-chat hero: on entry its animations replay; on exit the composer
        // drops from the middle to its dock while the transcript fills in.
        let page = self.settings_open || self.gitlab_open;
        let hero = self.hero_visible() && !show_question && !page && !self.projects.home_open();
        if hero != self.was_hero {
            self.hero_gen += 1;
            self.hero_exit = (!hero).then(|| (0., std::time::Instant::now()));
            self.hero_from = self.hero_panel.get();
            self.dock_slot.set(None);
            self.sync_right_panel_hero_transition(hero);
            self.was_hero = hero;
        }
        // Page switch (chat ↔ GitLab): the arriving page glides 16 px in from
        // its side and fades up; the sidebar holds still. Settings has its
        // own sheet. Instant under reduced motion.
        let page_key = if self.gitlab_open { 2 } else if self.projects.home_open() { 3 } else { 0 };
        if page_key != self.page_key {
            self.page_from = std::mem::replace(&mut self.page_key, page_key);
            self.page_at = (!reduced).then(|| (0., std::time::Instant::now()));
        }
        // Progress advances at most one 60 Hz frame per frame: the new page's
        // slow first build delays the glide rather than skipping half of it.
        let page_look = self.page_at.and_then(|(t, last)| {
            let t = t + last.elapsed().as_secs_f32().min(1. / 60.) / crate::motion::SLOW.as_secs_f32();
            self.page_at = (t < 1.).then(|| (t, std::time::Instant::now()));
            let e = crate::motion::ease_out(t.min(1.));
            let side = if self.page_key > self.page_from { 1. } else { -1. };
            (t < 1.).then_some((e, side * 16. * (1. - e)))
        });
        if page_look.is_some() {
            window.request_animation_frame();
        }
        let hero_t = if !reduced && self.hero_exiting() {
            self.hero_exit_progress()
        } else if self.hero_exit.is_some() {
            // Reduced motion: finish at once.
            self.hero_exit = None;
            1.
        } else {
            1.
        };
        let exiting = !reduced && hero_t < 1.;
        if exiting {
            window.request_animation_frame();
        }
        // Landing dock height once the drop knows its rest slot, so the
        // transcript's tail padding is right before the chat fades in (cold
        // launch used to keep dock_h at 0 for the whole exit, then snap).
        if exiting {
            if let Some(slot) = self.dock_slot.get() {
                let h = f32::from(slot.size.height);
                if h > 1. && (self.dock_h.get() - h).abs() > 0.5 {
                    self.dock_h.set(h);
                }
            }
        }
        let hero_gen = self.hero_gen;

        // The timer's corner layer: over the transcript when there is one
        // (so bottom corners clear the composer), else under the header.
        // Settings covers the pane, so a corner clock would sit on it.
        let page_timer = ((hero || page) && !self.settings_open && self.settings_leaving.is_none())
            .then(|| self.render_timer_corners(t, HEADER_HEIGHT, cx))
            .flatten();

        let card_moving = self.question_exit_showing()
            || self.current_panel().is_some_and(|p| p.shown_at.elapsed() < std::time::Duration::from_millis(600));
        let sliding = self.sidebar_anim.is_some_and(|(at, _)| at.elapsed() < crate::motion::SHEET_OPEN + std::time::Duration::from_millis(400));
        let right_sliding = self.right_panel_toggling();
        let expanding = self.browser_expanding();
        if sliding || right_sliding || expanding {
            window.request_animation_frame();
        }
        let dotify_entering = self.dotify.open && super::dotify::ms_since(self.dotify.open_at) < crate::dotify::ENTRY_DONE_MS + 100.;
        let boxing = self.lightbox_fx.animating(std::time::Instant::now());
        let probe = (crate::frame_probe::enabled() && (sliding || right_sliding || card_moving || dotify_entering || boxing)).then(|| {
            self.frame_probe.frame(if boxing {
                "lightbox"
            } else if dotify_entering {
                "dotify"
            } else if sliding || right_sliding {
                if self.state.ui_settings.sidebar_collapsed { "collapse" } else { "expand" }
            } else {
                "card"
            })
        });
        if probe.is_none() {
            self.frame_probe.reset();
        }
        let (probe, probe_end) = probe.unzip();

        let root = div();
        self.bind_actions(root, cx)
            .child(gpui_base::TextSelectionLayer)
            .children(probe)
            .relative()
            .size_full()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .flex()
            .flex_row()
            .font_family(SANS)
            .t_body()
            .bg(c(t.window))
            .text_color(c(t.text))
            .when(self.resizing_sidebar || self.browser.resizing, |d| d.cursor_col_resize())
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, window, cx| {
                if this.resizing_sidebar {
                    this.state.set_sidebar_width(f32::from(event.position.x));
                    cx.notify();
                } else if this.browser.resizing {
                    this.drag_browser_edge(f32::from(event.position.x), window);
                    cx.notify();
                }
            }))
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                    if this.resizing_sidebar {
                        this.resizing_sidebar = false;
                        this.state.save_settings();
                        cx.notify();
                    }
                    if this.browser.resizing {
                        this.end_browser_resize();
                        cx.notify();
                    }
                }),
            )
            .children(self.render_sidebar_animated(t, cx))
            .child(
                // The still layer: an opaque backing and the hero's picture, which
                // must not ride the page glide (a sliding full-pane image
                // reads as the screen shaking).
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .bg(c(t.window))
                    .when(hero || exiting || (!page && !self.projects.home_open() && self.current_chat.is_some() && !self.state.ui_settings.chat_glow_off), |d| {
                        let u = if reduced || hero {
                            0.
                        } else {
                            hero_t
                        };
                        d.child(self.render_hero_backdrop(t, u, window))
                    })
                    .when(exiting, |d| d.children(self.render_hero_title_exit(hero_gen, reduced)))
                    .child(
                div()
                    .relative()
                    .size_full()
                    .flex()
                    .flex_col()
                    .when_some(page_look, |d, (opacity, dx)| d.opacity(opacity).left(zpx(dx)))
                    .map(|d| {
                        // Settings paints over the whole window, sidebars
                        // included; the column keeps the page underneath.
                        if self.settings_open {
                            d
                        } else if self.gitlab_open {
                            d.child(self.render_header(t, true, false, false, cx)).child(self.render_gitlab(t, cx))
                        } else if self.projects.home_open() {
                            d.child(self.render_header(t, true, false, false, cx)).child(self.render_project_home(t, cx))
                        } else if hero {
                            let composer = self.render_composer(t, window, true, cx).into_any_element();
                            d.child(self.render_header(t, true, false, false, cx))
                                .child(self.render_hero_center(composer, hero_gen, reduced))
                        } else {
                            d.child(self.render_header(t, false, exiting, false, cx))
                                .child(if self.subagent_open.is_some() {
                                    self.render_subagent_session(t, cx)
                                } else {
                                    self.render_transcript(t, window, cx).into_any_element()
                                })
                                // The loader rides inline after the newest row; with
                                // no rows yet there is nothing to sit under.
                                .children(self.transcript.items.is_empty().then(|| self.render_thinking(t)).flatten())
                        }
                    })
                    .children(page_timer)
                    .children(self.render_subagent_menu(t, cx))
                    .children(self.render_mr_prompt(t, cx))
                    .children(self.render_lightbox(t, window, cx))
                    .when(!hero && !page && !self.projects.home_open(), |d| {
                        if self.subagent_open.is_some() || show_question {
                            self.dock_h.set(0.);
                            return d.child(if show_question {
                                self.render_question_panel(t, window, cx).into_any_element()
                            } else {
                                self.render_subagent_note(t)
                            });
                        }
                        let composer = self.render_composer(t, window, false, cx).into_any_element();
                        let composer = if exiting { self.render_composer_drop(composer, window, hero_gen, reduced) } else { composer };
                        // The composer floats over the transcript's foot; the
                        // transcript reserves its height as room under the newest row. Held still while
                        // the hero drop morphs it.
                        let dock_h = self.dock_h.clone();
                        // The session card's gutter, as the transcript keeps it
                        // (gliding with it), so the composer shares its column.
                        let (room_l, room_r) = self.target_card_column_room(window);
                        // Under the composer, over the transcript: the band the
                        // suggestion hint reads on.
                        let band = (!exiting).then(|| self.render_hint_band(t, cx)).flatten();
                        // Mid-slide the composer shares the transcript's left
                        // edge, but its width glides to the landing one on the
                        // same clock (`ColumnGeo::dock_w`): snapping it to the
                        // landing width made the right edge jump one way, then
                        // travel back the other.
                        let dock_w = self.column_slide.map(|_| self.column_last.dock_w);
                        d.children(band).child(
                            div()
                                .absolute()
                                .bottom_0()
                                .when_some(dock_w, |d, w| d.left(px(room_l)).w(px(w)))
                                .when(dock_w.is_none(), |d| d.left_0().right_0().pl(px(room_l)).pr(px(room_r)))
                                .child(composer)
                                .children(super::dev::GeoTrace::probe(&self.geo_trace, |g| &g.dock))
                                .when(!exiting, |d| {
                                d.child(
                                    gpui::canvas(
                                        move |bounds, window, _| {
                                            let h = f32::from(bounds.size.height);
                                            if (dock_h.get() - h).abs() > 0.5 {
                                                dock_h.set(h);
                                                window.refresh();
                                            }
                                        },
                                        |_, _, _, _| {},
                                    )
                                    .absolute()
                                    .inset_0(),
                                )
                            }),
                        )
                        .children(self.render_latest_pill(t, cx))
                    }),
                ),
            )
            .children(self.render_browser_panel(t, window, cx))
            .children(self.render_files_panel(t, window, cx))
            .children(self.render_terminal_panel(t, t == keel_theme::Tokens::dark(), window, cx))
            // Last children: paint over the content.
            .children(self.render_sidebar_trigger(t, cx))
            .children(self.render_sidebar_peek(t, cx))
            .children(self.render_zoom_toast(t))
            .children(self.render_notice(t, cx))
            .children(self.render_timer_menu(t, cx))
            .children(if self.dotify_visible() { self.render_dotify_overlay(t, window, cx) } else { Vec::new() })
            .children(self.render_timer_drag(t, cx))
            .children(self.render_mr_card(t, cx))
            .children(self.render_mr_menu(t, cx))
            .children(self.render_session_summary_card(t, window, cx))
            .children(self.render_project_folder_card(t, window, cx))
            // Settings covers both sidebars; the delete scrims cover that too.
            .children((self.settings_open || self.settings_leaving.is_some()).then(|| self.render_settings_page(t, window, cx)))
            .children(self.render_delete_space_modal(t, cx))
            .children(self.render_delete_chats_modal(t, cx))
            .children(probe_end)
    }
}

const HERO_TITLE: &str = "What you will be working on?";

/// The headline's type: Cormorant Garamond SemiBold Italic (bundled, OFL;
/// set large, it runs small; SemiBold so its hairlines hold over bright parts
/// of the picture), in a warm ivory; words wrap centered on narrow panes.
fn hero_title_text() -> gpui::Div {
    div()
        .relative()
        .w_full()
        .px(zpx(24.))
        .flex()
        .flex_wrap()
        .justify_center()
        .gap_x(zpx(12.))
        .font_family("Cormorant Garamond")
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .italic()
        .text_size(zpx(54.))
        .line_height(zpx(60.))
        .text_color(gpui::rgba(0xF3EEE4FF))
}

/// Space under the newest message, on top of the input slot's own 16 px.
const TAIL_ROOM: f32 = 64.;
/// The suggestion hint's band: solid from this far above the input slot
/// (the hint sits 4 px above it, 14 px tall) down to the pane's foot.
/// How much of the picture the chat glow shows at the pane's foot, and how
/// far up the pane it rises (fractions).
const CHAT_GLOW: f32 = 0.08;
const CHAT_GLOW_HEIGHT: f32 = 0.45;

/// The longest a switched-to chat stays hidden waiting for its history.
const CHAT_REVEAL_WAIT: std::time::Duration = std::time::Duration::from_millis(400);
/// A history load shorter than this shows no skeleton.
const SKELETON_DELAY: std::time::Duration = std::time::Duration::from_millis(150);
/// How long each loader verb stays up.
const VERB_SECS: u64 = 3;
/// Loader verbs per phase: before output and while reasoning, while a tool
/// runs, while answer text streams.
pub(super) const THINKING_VERBS: &[&str] =
    &["Thinking", "Formulating", "Pondering", "Reasoning", "Considering", "Mulling it over", "Connecting dots"];
pub(super) const WORKING_VERBS: &[&str] =
    &["Working", "Investigating", "Digging in", "Exploring", "Inspecting", "Running checks", "Tinkering"];
/// Loader verbs while a tool runs: its kind, never its target (the tool row
/// already names the file or command).
pub(super) fn tool_verbs(name: &str, kind: &str) -> &'static [&'static str] {
    use crate::trace::StepKind::*;
    match crate::trace::step_kind(name, kind) {
        Read => &["Reading files"],
        Edit => &["Editing code"],
        Delete => &["Deleting files"],
        Move => &["Moving files"],
        Search => &["Searching"],
        Execute => &["Running a command"],
        Fetch => &["Fetching from the web"],
        Browser => &["Using the browser"],
        Subagent => &["Waiting on a subagent"],
        Plan => &["Planning"],
        Think => THINKING_VERBS,
        SwitchMode | Sources | Other => WORKING_VERBS,
    }
}
pub(super) const WRITING_VERBS: &[&str] = &["Writing", "Composing", "Drafting", "Putting it together", "Phrasing"];

impl KeelWindow {
    /// A chat with nothing in it yet, or no chat at all (first run too): the
    /// composer sits mid-window over the landscape.
    fn hero_visible(&self) -> bool {
        match &self.current_chat {
            // A never-used chat needs no history load, so it skips the skeleton.
            Some(id) => {
                self.transcript.items.is_empty()
                    && (self.chat_stream.is_some()
                        || self.state.chats.get(id).is_some_and(|chat| chat.last_message_at == 0))
            }
            None => !self.creating_chat,
        }
    }

    /// The sidebar, with its width easing open or shut after a toggle. The
    /// content keeps its full width and is clipped, so nothing reflows; items
    /// fade and slide with the panel so the clip is not a hard cut.
    /// The pane's current available width taking the animated sidebar and right panel into account.
    pub(super) fn pane_width(&self, window: &Window) -> Option<f32> {
        let viewport = f32::from(window.viewport_size().width);
        let sidebar = self.state.sidebar_width() * self.sidebar_open_now();
        let browser = if self.browser_allowed() {
            self.browser_width(window) * self.right_panel_open_now()
        } else {
            0.
        };
        Some((viewport - sidebar - browser).max(1.))
    }

    fn render_sidebar_animated(&mut self, t: Tokens, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let collapsed = self.state.ui_settings.sidebar_collapsed;
        if !self.sidebar_toggling() {
            return (!collapsed).then(|| self.render_sidebar(t, cx).into_any_element());
        }
        // The sidebar pushes the pane: its in-flow width follows the same
        // clock as the reading column (`sidebar_open_now`), so the chat moves
        // 1:1 with its edge. Its content stays put under the traffic lights
        // (a short drift in) and the moving edge reveals it.
        let width = self.state.sidebar_width();
        let open = crate::motion::unit(self.sidebar_open_now());
        let cascade = self.sidebar_from == 0.;
        let opacity = if collapsed || !cascade {
            1.0 - 0.85 * crate::motion::ease_out(((1.0 - open) / 0.6).min(1.0))
        } else {
            1.0
        };
        Some(
            div()
                .relative()
                .h_full()
                .flex_none()
                .overflow_hidden()
                .w(px(width * open))
                .opacity(opacity.max(0.001))
                .child(div().absolute().top_0().left_0().w(px(width)).h_full().child(self.render_sidebar(t, cx)))
                .into_any_element(),
        )
    }

    /// Stationary sidebar trigger button pinned adjacent to the macOS traffic lights.
    /// Follows Emil Kowalski craft principles:
    /// - Stationary anchor: 0px mouse travel across toggle states.
    /// - Continuous morph: smooth crossfade and micro-rotation between `SidebarOpen` and `SidebarClose`.
    /// - Elevation transition: high-contrast floating card over desktop wallpapers when collapsed,
    ///   smoothly dissolving into an integrated sidebar titlebar action when expanded.
    /// - Active tactile feedback: crisp scale / press feedback on click.
    pub(super) fn render_sidebar_trigger(&mut self, t: Tokens, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if self.settings_open || self.browser.expanded {
            return None;
        }
        let open = if self.sidebar_toggling() {
            self.sidebar_open_now()
        } else if self.state.ui_settings.sidebar_collapsed {
            0.0
        } else {
            1.0
        };

        let top_inset = (HEADER_HEIGHT - zpx(26.)) / 2.;
        let tooltip_text = if open > 0.5 { "Collapse sidebar (⌘B)" } else { "Expand sidebar (⌘B)" };

        // Subtle micro-rotation on the chevrons during the morph (Emil Kowalski craft principle)
        let open_rot = -std::f32::consts::PI * (12.0 / 180.0) * open;
        let close_rot = std::f32::consts::PI * (12.0 / 180.0) * (1.0 - open);

        let open_icon = div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .opacity((1.0 - open).max(0.0))
            .child(
                Icon::SidebarOpen
                    .el(zpx(15.))
                    .with_transformation(gpui::Transformation::rotate(gpui::radians(open_rot))),
            );

        let close_icon = div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .opacity(open.max(0.0))
            .child(
                Icon::SidebarClose
                    .el(zpx(15.))
                    .with_transformation(gpui::Transformation::rotate(gpui::radians(close_rot))),
            );

        Some(
            div()
                .id("sidebar-toggle-btn")
                .absolute()
                .top(top_inset)
                .left(self.traffic_lights_inset())
                .size(zpx(26.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(RADIUS_MD)
                // When collapsed: high-contrast floating card backing over wallpapers.
                // As the sidebar expands under it: smoothly dissolves into the sidebar background.
                .bg(c(t.floating).opacity((1.0 - open).clamp(0.0, 1.0)))
                .border_1()
                .border_color(c(t.border).opacity((1.0 - 0.5 * open).clamp(0.0, 1.0)))
                .when(open < 0.95, |d| {
                    d.shadow(vec![gpui::BoxShadow {
                        color: c(t.shadow).opacity(1.0 - open),
                        offset: gpui::point(zpx(0.), zpx(1.5)),
                        blur_radius: zpx(6.),
                        spread_radius: zpx(0.),
                        inset: false,
                    }])
                })
                .text_color(c(t.text))
                .cursor_pointer()
                .hover(|s| s.bg(c(t.surface)).text_color(c(t.text)))
                .active(|s| s.bg(c(t.selected)).opacity(0.85))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_sidebar(cx)))
                .tooltip(move |_, cx| cx.new(|_| crate::view::dotify_ui::Tip::new(tooltip_text, t)).into())
                .tooltip_show_delay(crate::view::tooltip::delay())
                .child(div().relative().size_full().child(open_icon).child(close_icon))
                .into_any_element(),
        )
    }

    /// Your own picture wins: Settings → Background, else hero.jpg / .png /
    /// .webp dropped in the data dir. None: the built-in one.
    pub(super) fn hero_image_path(&self) -> Option<std::path::PathBuf> {
        let mut cached = self.hero_path.borrow_mut();
        if let Some((at, path)) = cached.as_ref()
            && at.elapsed() < HERO_PATH_TTL
        {
            return path.clone();
        }
        let chosen = crate::background::background_path(&self.state.data_dir, self.state.ui_settings.background.as_deref());
        let path = chosen.or_else(|| {
            ["png", "jpg", "jpeg", "webp"]
                .iter()
                .map(|ext| self.state.data_dir.join(format!("hero.{ext}")))
                .find(|path| path.is_file())
        });
        *cached = Some((std::time::Instant::now(), path.clone()));
        path
    }

    /// The landscape behind the pane. On the hero it covers the pane and
    /// eases into the window color toward the bottom. In a chat the same
    /// picture is a faint glow at the foot.
    ///
    /// Leaving the hero does not swap layers. The picture stays put and the
    /// window color closes over it, heaviest at the top, until only the foot
    /// still shows. Two layers (the picture fading out, then a glow fading
    /// in) left a frame of solid window between them, which read as a seam.
    ///
    /// The picture is laid out window-wide and pinned to the pane's right
    /// edge, so a sidebar toggle only uncovers or covers its left part.
    fn render_hero_backdrop(&self, t: Tokens, u: f32, window: &Window) -> gpui::AnyElement {
        let window_color = c(t.window);
        let frame_w = f32::from(window.viewport_size().width);
        let dim = self.state.ui_settings.background_dim.unwrap_or(0.).clamp(0., keel_proto::MAX_BACKGROUND_DIM);
        let path = self.hero_image_path();
        div()
            .absolute()
            .inset_0()
            .overflow_hidden()
            .child({
                let image = path
                    .as_ref()
                    .map_or_else(|| gpui::img(crate::icons::HERO_IMAGE), |p| gpui::img(p.clone()))
                    .size_full()
                    .object_fit(gpui::ObjectFit::Cover);
                // Static: no drift, so nothing redraws while the pane sits idle.
                div().absolute().top_0().bottom_0().right_0().w(px(frame_w)).child(image).when(dim > 0., |d| {
                    d.child(div().absolute().inset_0().bg(gpui::hsla(0., 0., 0., dim)))
                })
            })
            .child(
                gpui::canvas(
                    move |_, _, _| {},
                    move |bounds, _, window, _| paint_backdrop_veil(bounds, window, window_color, crate::motion::glide_curve(u)),
                )
                .absolute()
                .inset_0(),
            )
            .into_any_element()
    }


    /// Advance the hero → chat exit by at most one 60 Hz frame and return
    /// its 0..1 progress. Cleared when finished.
    pub(super) fn hero_exit_progress(&mut self) -> f32 {
        let Some((t, last)) = self.hero_exit else {
            return 1.;
        };
        let t = (t + last.elapsed().as_secs_f32().min(1. / 60.) / crate::motion::SPATIAL.as_secs_f32()).min(1.);
        self.hero_exit = (t < 1.).then_some((t, std::time::Instant::now()));
        t
    }

    /// Current hero → chat exit progress without advancing (1 when idle).
    pub(super) fn hero_exit_t(&self) -> f32 {
        self.hero_exit.map_or(1., |(t, _)| t)
    }

    /// Whether the hero → chat exit is still gliding.
    pub(super) fn hero_exiting(&self) -> bool {
        self.hero_exit.is_some()
    }

    /// The composer leaving the hero: it glides from the exact spot the hero
    /// panel was painted to its dock, widening from the hero width to the
    /// transcript width on the way and interpolating both X and Y coordinates
    /// so the shift to the reading column (left of the session card) is seamless.
    fn render_composer_drop(&self, composer: gpui::AnyElement, window: &Window, _n: u64, reduced: bool) -> gpui::AnyElement {
        let (from, slot) = (self.hero_from, self.dock_slot.clone());
        let record = self.dock_slot.clone();
        let composer_slot = self.composer_slot.clone();
        let viewport_h = f32::from(window.viewport_size().height);
        // Timestamp-driven, styled on a plain div: an animation wrapper would
        // put its id into the composer's identity, and dropping it at the end
        // re-keyed every chip in the panel (their state reset: the flicker).
        let delta = if reduced {
            1.
        } else {
            let t = self.hero_exit_t();
            if t >= 1. { 1. } else { crate::motion::glide_curve(t) }
        };
        let moved = from.map(|from| {
            let rest = slot.get();
            // Bottom edges: the panel shrinks into place from its top
            // (see render_composer), so its bottom is what travels.
            let rest_bottom = rest.map_or(viewport_h, |r| f32::from(r.bottom())) - f32::from(zpx(16.));
            let dy = f32::from(from.bottom()) - rest_bottom;

            // Horizontal transition: the hero composer was centered in the full pane,
            // while the docked composer is centered in the reading column (shifted left
            // to leave room for the session card). We track both centers so the 2D path
            // glides continuously with zero horizontal snap.
            let (room_l, room_r) = self.target_card_column_room(window);
            let sidebar_w = self.state.sidebar_width() * self.sidebar_open_now();
            let viewport_w = f32::from(window.viewport_size().width);
            let browser_w = if self.browser_allowed() && self.right_panel_open() { self.browser_width(window) } else { 0. };
            let col_left = self.column_slide.map_or(sidebar_w + room_l, |s| s.to.x);
            let col_w = self.column_slide.map_or((viewport_w - sidebar_w - browser_w - room_l - room_r).max(0.), |s| s.to.w);
            let fallback_center_x = col_left + col_w / 2.;
            let rest_center_x = rest.map_or(fallback_center_x, |r| f32::from(r.origin.x) + f32::from(r.size.width) / 2.);
            let from_center_x = f32::from(from.origin.x) + f32::from(from.size.width) / 2.;
            let dx = from_center_x - rest_center_x;

            // + the px_4 gutters on both sides.
            let gutters = 2. * f32::from(gpui::rems(1.).to_pixels(window.rem_size()));
            let from_w = f32::from(from.size.width) + gutters;
            // Aim at the docked panel's own cap (the transcript width, less
            // the usage meter's room): past it the widening would finish
            // early and stall, short of it the panel would jump at the end.
            let dock_w = self.composer_max.get() + gutters;
            let rest_w = rest.map_or(dock_w, |r| f32::from(r.size.width).min(dock_w));
            (dy * (1. - delta), dx * (1. - delta), from_w + (rest_w - from_w) * delta)
        });
        div()
            .relative()
            .w_full()
            .flex_none()
            .flex()
            .justify_center()
            .child(
                gpui::canvas(
                    move |bounds, window, _| {
                        record.set(Some(bounds));
                        // The composer can't measure its slot while squeezed; this is it.
                        super::composer_view::record_slot(&composer_slot, bounds, window);
                    },
                    |_, _, _, _| {},
                )
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
            )
            .child(
                div()
                    .w_full()
                    .child(composer)
                    .children(super::dev::GeoTrace::probe(&self.geo_trace, |g| &g.dock))
                    .when_some(moved, |el, (top, left, max_w)| el.relative().top(px(top)).left(px(left)).max_w(px(max_w))),
            )
            .into_any_element()
    }

    /// "110%", top center, for a moment after each zoom change: fades and
    /// rises in over the first ~120 ms, holds, fades out over the last ~200.
    fn render_zoom_toast(&self, t: Tokens) -> Option<gpui::AnyElement> {
        use gpui::AnimationExt as _;
        let (generation, continuing) = self.zoom_toast?;
        let percent = (crate::style::zoom() * 100.).round() as u32;
        let total = super::ZOOM_TOAST.as_secs_f32();
        let (fade_in, fade_out) = (0.12 / total, 0.2 / total);
        let pill = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_1p5()
            .rounded_full()
            .floating_surface(t)
            .child(div().t_body().font_weight(gpui::FontWeight::SEMIBOLD).font_family(crate::style::MONO).child(format!("{percent}%")))
            .when(percent != 100, |d| d.child(div().t_caption().text_color(c(t.text_faint)).child("⌘0 resets")));
        let shown = move |p: f32| -> f32 {
            if p < fade_in && continuing {
                1.
            } else if p < fade_in {
                crate::motion::ease_out(p / fade_in)
            } else if p > 1. - fade_out {
                1. - crate::motion::ease_in((p - (1. - fade_out)) / fade_out)
            } else {
                1.
            }
        };
        let pill: gpui::AnyElement = if self.reduced() {
            pill.into_any_element()
        } else {
            pill.with_animation(("zoom-toast", generation as usize), gpui::Animation::new(super::ZOOM_TOAST), move |el, p| {
                let a = shown(p);
                // Only the entrance moves; the exit just fades.
                let rise = if p < fade_in && !continuing { 6. * (1. - a) } else { 0. };
                el.opacity(a).relative().top(px(rise))
            })
            .into_any_element()
        };
        Some(div().absolute().top(HEADER_HEIGHT + px(8.)).left_0().right_0().flex().justify_center().child(pill).into_any_element())
    }

    /// The top bar, no tabs: a floating pill with the sidebar toggle; in a
    /// chat, its merge request and the pane toggles. Everything else drags
    /// the window. The chat's context, folder and branch live in the session
    /// card (`session_card`), not here. `bare` is the Settings header: no
    /// clock in / clock out. That stays off through the exit, when
    /// `settings_open` is already false.
    fn render_header(&mut self, t: Tokens, hero: bool, _entering: bool, bare: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let open = if self.sidebar_toggling() {
            self.sidebar_open_now()
        } else if self.state.ui_settings.sidebar_collapsed {
            0.0
        } else {
            1.0
        };
        // The session timer rides beside the sidebar toggle.
        let timer = (!bare)
            .then(|| self.render_timer_header(t, cx))
            .flatten();
        // The session's merge request, after its title (nothing when none).
        let mr_chip = if hero { None } else { self.render_mr_chip(t, cx) };
        // Left padding smoothly interpolates from clearing the stationary toggle button (collapsed)
        // to tight margin (expanded), coordinating with the sliding main pane.
        let collapsed_pl = f32::from(self.traffic_lights_inset() + zpx(26.) + zpx(6.));
        let expanded_pl = f32::from(zpx(6.));
        let left_pad = (1.0 - open) * collapsed_pl + open * expanded_pl;
        div()
            .h(HEADER_HEIGHT)
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .pr_2()
            .gap_1()
            .pl(px(left_pad))
            .children(timer)
            .children(mr_chip.map(|chip| div().pl_2().child(chip)))
            .child(drag_region("header-drag"))
            .when(!hero && !self.browser.open, |d| {
                let background = self.browser_in_background();
                d.child(
                    icon_button("toggle-browser-btn", Icon::Globe, t)
                        .relative()
                        // An agent is using the page behind the closed pane.
                        .when_some(background, |b, recent| {
                            b.child(
                                div()
                                    .absolute()
                                    .top(zpx(5.))
                                    .right(zpx(5.))
                                    .size(zpx(6.))
                                    .rounded_full()
                                    .bg(c(t.info))
                                    .opacity(if recent { 1. } else { 0.7 }),
                            )
                        })
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_browser(window, cx))),
                )
            })
            .when(!hero && !self.files.open, |d| {
                d.child(
                    icon_button("toggle-files-btn", Icon::FileDiff, t)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_files(window, cx))),
                )
            })
            .when(!hero, |d| {
                d.child(
                    icon_button("toggle-terminal-btn", Icon::Tool, t)
                        .when(self.terminal.open, |b| b.bg(c(t.selected)).text_color(c(t.text)))
                        // A dot while a command runs in one of the shells.
                        .when(self.terminal.busy, |b| {
                            b.relative().child(
                                div()
                                    .absolute()
                                    .top(zpx(5.))
                                    .right(zpx(5.))
                                    .size(zpx(6.))
                                    .rounded_full()
                                    .bg(c(t.accent)),
                            )
                        })
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_terminal(window, cx))),
                )
            })
            .when(!hero && !self.subagents.is_empty(), |d| {
                let running = self
                    .subagents
                    .values()
                    .any(|sub| sub.get("status").and_then(|v| v.as_str()) == Some("running"));
                d.child(
                    icon_button("subagents-btn", Icon::Brain, t)
                        .when(self.show_subagent_menu, |b| b.bg(c(t.selected)).text_color(c(t.text)))
                        // A dot while any subagent is still running.
                        .when(running, |b| {
                            b.relative().child(
                                div()
                                    .absolute()
                                    .top(zpx(5.))
                                    .right(zpx(5.))
                                    .size(zpx(6.))
                                    .rounded_full()
                                    .bg(c(t.accent)),
                            )
                        })
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_subagent_menu(cx))),
                )
            })
    }

    /// The composer dead center in the window, rising in as the screen opens.
    fn render_hero_center(&self, composer: gpui::AnyElement, n: u64, reduced: bool) -> impl IntoElement {
        // The header takes HEADER_HEIGHT off the top; matching it underneath
        // puts the composer on the window's true center line.
        div()
            .flex_1()
            .min_h_0()
            .pb(HEADER_HEIGHT)
            .flex()
            .flex_col()
            .justify_center()
            .child(self.render_hero_title(n, reduced))
            .child(crate::motion::travel(div().w_full().child(composer), ("hero-center-in", n), 14., reduced))
    }

    /// The hero's headline. Words rise in one after another (rare screen:
    /// the delight budget); reduced motion keeps only the fade.
    fn render_hero_title(&self, n: u64, reduced: bool) -> impl IntoElement {
        use gpui::AnimationExt as _;
        let bounds = self.hero_title.clone();
        let words = HERO_TITLE.split(' ').enumerate().map(move |(i, word)| {
            let (delay, length) = (i as f32 * 45., 520.);
            let total = std::time::Duration::from_millis((delay + length) as u64);
            div().relative().child(word).with_animation(
                ("hero-title-word", n * 16 + i as u64),
                gpui::Animation::new(total)
                    .with_easing(move |t| crate::motion::ease_out_strong(((t * (delay + length) - delay) / length).clamp(0., 1.))),
                move |el, d| {
                    let el = el.opacity(d);
                    if reduced { el } else { el.top(zpx(14. * (1. - d))) }
                },
            )
        });
        hero_title_text()
            .mb(zpx(26.))
            .child(gpui::canvas(move |b, _, _| bounds.set(Some(b)), |_, _, _, _| {}).absolute().inset_0())
            .children(words)
    }

    /// The headline leaving with the hero: fades and lifts away from the
    /// composer's drop, quicker than it came in.
    fn render_hero_title_exit(&self, n: u64, reduced: bool) -> Option<gpui::AnyElement> {
        use gpui::AnimationExt as _;
        let at = self.hero_title.get()?;
        Some(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(at.top())
                .child(hero_title_text().children(HERO_TITLE.split(' ').map(|w| div().child(w))))
                .with_animation(
                    ("hero-title-out", n),
                    gpui::Animation::new(crate::motion::SLOW).with_easing(crate::motion::ease_out),
                    move |el, d| {
                        let el = el.opacity(1. - d);
                        if reduced { el } else { el.mt(zpx(-8. * d)) }
                    },
                )
                .into_any_element(),
        )
    }

    fn render_transcript(&mut self, t: Tokens, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let syntax = resolve_syntax_theme(self.state.ui_settings.theme, matches!(
            window.appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        ));
        let reduced = self.reduced();
        let running = self.active_status() == Some(ChatStatus::Running);
        // A switched-to chat stays hidden until its history is in and parsed
        // (or CHAT_REVEAL_WAIT passes, for a chat that is streaming), then
        // fades in already at the end. The first settled frame paints at 0,
        // which gives the list one layout pass to land on the end.
        let reveal_opacity = match self.chat_reveal {
            Some(None) => {
                let settled = self.chat_stream.is_some() && !self.parse_in_flight && self.dirty.is_empty() && !self.transcript.items.is_empty();
                if settled || self.transcript.opened_at.elapsed() >= CHAT_REVEAL_WAIT {
                    // Land on the end before the fade: first measures leave
                    // Tail mid-list when rows grow from 0-height estimates.
                    self.transcript.on_own_send();
                    self.transcript.scroll_to_bottom();
                    self.chat_reveal = Some(Some(std::time::Instant::now()));
                } else if !self.transcript.items.is_empty() {
                    self.transcript.on_own_send();
                    self.transcript.scroll_to_bottom();
                }
                window.request_animation_frame();
                0.
            }
            Some(Some(at)) => {
                // Keep the end while rows remasure under the fade.
                self.transcript.on_own_send();
                self.transcript.scroll_to_bottom();
                let t = at.elapsed().as_secs_f32() / crate::motion::FAST.as_secs_f32();
                if reduced || t >= 1. {
                    self.chat_reveal = None;
                    1.
                } else {
                    window.request_animation_frame();
                    crate::motion::ease_out(t)
                }
            }
            None => 1.,
        };
        // Inline loader: the last line of the active turn, under the newest row.
        let loader_row = (running && self.current_chat.is_some()).then(|| self.transcript.items.len().checked_sub(1)).flatten();
        // The loader fades and rises in when a run starts (not when it hops
        // to a new last row), and fades out collapsing when the run ends.
        let loader_now = std::time::Instant::now();
        if loader_row.is_some() && self.loader_row.is_none() {
            self.loader_at = Some(loader_now);
            self.loader_exit = None;
        }
        if loader_row.is_none() && !reduced {
            if let Some(ix) = self.loader_row {
                self.loader_exit = Some((ix, loader_now));
            }
        }
        if loader_row != self.loader_row {
            // Off-screen rows keep a cached height; refresh the rows the loader
            // left and joined (visible rows are measured every frame anyway).
            let len = self.transcript.items.len();
            for ix in [self.loader_row, loader_row].into_iter().flatten().filter(|&ix| ix < len) {
                self.transcript.list_state.remeasure_items(ix..ix + 1);
            }
            self.loader_row = loader_row;
        }
        // The newest row carries the room under it; when rows come or go, the
        // row that had it (and the one that gets it) measure again.
        let len = self.transcript.items.len();
        if len != self.tail_len {
            for ix in [self.tail_len.checked_sub(1), len.checked_sub(1)].into_iter().flatten().filter(|&ix| ix < len) {
                self.transcript.list_state.remeasure_items(ix..ix + 1);
            }
            self.tail_len = len;
        }
        // The floating composer's height is part of that room.
        let dock_h = self.dock_h.get();
        if dock_h != self.tail_dock_h {
            if let Some(last) = len.checked_sub(1) {
                self.transcript.list_state.remeasure_items(last..len);
            }
            self.tail_dock_h = dock_h;
        }
        let exit = self
            .loader_exit
            .filter(|(_, at)| loader_now.saturating_duration_since(*at) < crate::motion::ROW_EXIT)
            .filter(|_| loader_row.is_none());
        if exit.is_none() {
            self.loader_exit = None;
        }
        let look = match (loader_row, exit) {
            (Some(_), _) => self.loader_at.and_then(|at| crate::motion::row_enter(at, loader_now, reduced)),
            (None, Some((_, at))) => crate::motion::row_exit(loader_now.saturating_duration_since(at), reduced),
            _ => None,
        };
        if look.is_some() {
            window.request_animation_frame();
        }
        // While it leaves it stays on the row it was under (or the last).
        let last = self.transcript.items.len().checked_sub(1);
        let loader_row = loader_row.or_else(|| exit.and_then(|(ix, _)| last.map(|l| ix.min(l))));
        let loader = loader_row.map(|_| {
            let (since, seed) = self.loader_seed;
            let verbs = self.loader_verbs;
            InlineLoader { tokens: t, verbs, since, seed, reduced, look }
        });
        let timer = self.render_timer_corners(t, px(0.), cx);
        // The pill's glide to the end runs before layout. Frames are asked
        // for only while it is moving.
        let pill_now = std::time::Instant::now();
        if self.transcript.advance_glide(pill_now) {
            window.request_animation_frame();
        }
        // Rows entering or leaving live: frames only while one moves.
        if self.transcript.reap(pill_now) {
            window.request_animation_frame();
        }
        // Shown for the whole time the user is away from the tail, including
        // the glide home. Hiding it for that glide was a flash: the pill
        // appeared, then vanished for the travel, then the list snapped.
        let want_pill = self.current_chat.is_some() && !self.transcript.stick_to_bottom;
        self.latest_pill.set(want_pill, pill_now, reduced);
        if self.latest_pill.animating(pill_now) {
            window.request_animation_frame();
        }
        // MR offers, Undo and notes ride under their turn's last row.
        let mr_rows = self.mr_inline_rows();
        let on_mr = self.mr_on_action(cx);
        // The session card's room: a right gutter, or a strip under its pill.
        let card_room = self.card_room(window, cx);
        if self.geo_trace.on.get() {
            let top = self.transcript.list_state.logical_scroll_top();
            self.geo_trace.push(serde_json::json!({
                "pin": self.column_slide.map(|s| s.to.w),
                "room": [card_room.0, card_room.1, card_room.2],
                "dock_room": self.target_card_column_room(window),
                "dock_pin": self.column_slide.map(|s| s.to.w),
                "pane_w": self.pane_width(window),
                "side": self.sidebar_open_now(),
                "right": self.right_panel_open_now(),
                "compact": self.session_card.compact,
                "top": [top.item_ix, f32::from(top.offset_in_item)],
                "stacked": self.composer_fit.stacked,
                "fit_anim": self.composer_fit_at.is_some(),
                "panel_w": self.composer_panel_w.get(),
                "dock_cell_h": self.dock_h.get(),
                "slide": self.column_slide.is_some(),
            }));
        }
        let card = self.render_session_card(t, window, cx);
        let card_measure = self.card_measure();
        div()
            .id("transcript-container")
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, cx| {
                // Wheel-up unsticks the follow; only wheel-down that reaches the bottom re-engages it.
                let dy = event.delta.pixel_delta(window.line_height()).y;
                this.transcript.on_wheel(dy);
                // Beside the reading region (the card's centring margins) the
                // list isn't under the pointer: scroll it from here.
                if this.card_wheel_outside_column(f32::from(event.position.x)) {
                    this.transcript.list_state.scroll_by(-dy);
                }
                cx.notify();
            }))
            .when(self.current_chat.is_some() && self.transcript.items.is_empty(), |d| {
                d.child(self.render_empty_chat(t))
            })
            .when(self.current_chat.is_some(), |d| {
                // No scroll from render: the list's Tail follow mode keeps the
                // latest row in view during layout, only while following.
                let items = self.transcript.items.clone();
                let weak = cx.entity().downgrade();
                let hover_cell = self.hovered_row.clone();
                let handle = window.window_handle();
                let wrap_hold = self.column_slide.map(|s| s.wrap_w() - 2. * f32::from(window.rem_size()));
                let rc = RowContext {
                    tokens: t,
                    syntax,
                    reduced: self.reduced(),
                    toggles: std::rc::Rc::new(self.fold_toggles.clone()),
                    copied: self.copied.as_ref().map(|(row, _)| row.clone()),
                    on_action: std::rc::Rc::new(move |action, cx| {
                        // Opening a file needs the window (the browser pane);
                        // the click is still dispatching, so do it right after.
                        if let RowAction::OpenFile { path } = action {
                            let weak = weak.clone();
                            cx.defer(move |cx| {
                                let _ = handle.update(cx, |_, window, cx| {
                                    weak.update(cx, |view, cx| view.open_file_result(path.into(), window, cx))
                                });
                            });
                            return;
                        }
                        if let RowAction::OpenFileDiff { path } = action {
                            let weak = weak.clone();
                            cx.defer(move |cx| {
                                let _ = handle.update(cx, |_, window, cx| {
                                    weak.update(cx, |view, cx| view.show_file_diff(path, window, cx))
                                });
                            });
                            return;
                        }
                        // Run and a diagram node also need the window (the
                        // terminal panel, the composer).
                        if let RowAction::Run { .. } | RowAction::AskAbout { .. } = &action {
                            let weak = weak.clone();
                            cx.defer(move |cx| {
                                let _ = handle.update(cx, |_, window, cx| {
                                    weak.update(cx, |view, cx| match action {
                                        RowAction::Run { row_id, command, risky } => view.run_command(row_id, command, risky, window, cx),
                                        RowAction::AskAbout { label } => view.ask_about(&label, window, cx),
                                        _ => {}
                                    })
                                });
                            });
                            return;
                        }
                        let _ = weak.update(cx, |view, cx| match action {
                            RowAction::Toggle { row_id } => view.toggle_row(&row_id, cx),
                            RowAction::Copy { row_id, content } => view.copy_code(row_id, content, cx),
                            RowAction::OpenImage { path } => view.open_lightbox(super::Lightbox::Image(path.into()), cx),
                            RowAction::OpenDiagram { row_id, source } => {
                                let view_state = Default::default();
                                view.open_lightbox(super::Lightbox::Diagram { row_id, source, view: view_state }, cx);
                            }
                            RowAction::CopyImage { row_id, source, tokens } => {
                                view.copy_image(row_id, source, tokens, cx)
                            }
                            RowAction::OpenSubagent { subagent_id } => view.open_subagent_session(&subagent_id, cx),
                            RowAction::Choose { text } => view.choose(text, cx),
                            RowAction::OpenUrl { url } => cx.open_url(&url),
                            RowAction::Merge { row_id, message_id, part_id } => view.merge_mr(row_id, message_id, part_id, cx),
                            RowAction::TrackMr { mr } => {
                                if let Some(chat) = view.current_chat.clone() {
                                    view.mr_track(chat, mr, cx);
                                }
                            }
                            RowAction::OpenFullOutput { title, content, error } => {
                                view.open_lightbox(super::Lightbox::Output { title, content, error }, cx);
                            }
                            RowAction::OpenFile { .. } | RowAction::OpenFileDiff { .. } | RowAction::Run { .. } | RowAction::AskAbout { .. } => {}
                        });
                    }),
                    order: 0,
                    live_choices: self.live_choices(),
                    merges: std::rc::Rc::new(self.mr_merges.clone()),
                    armed_runs: std::rc::Rc::new(self.armed_runs.clone()),
                    now: std::time::Instant::now(),
                    now_ms: crate::trace_view::now_ms(),
                    clock_epoch: self.clock_epoch,
                    viewport_h: f32::from(window.viewport_size().height),
                    open_lines: std::rc::Rc::new(self.open_lines()),
                    row_heights: self.transcript.row_heights.clone(),
                    tracked_mr: self
                        .current_chat
                        .as_deref()
                        .and_then(|chat| self.mr.tracked(chat))
                        .map(|tracked| std::rc::Rc::new(tracked.clone())),
                    // Mid-slide the text wraps once, at the narrower end
                    // of the slide (inside a row's px_4), and holds it.
                    prose_max_w: wrap_hold.map_or(crate::transcript::PROSE_MAX_W, |w| crate::transcript::PROSE_MAX_W.min(w / crate::style::zoom())),
                    bubble_max_w: wrap_hold.map(|w| 0.85 * w),
                    probe: self.geo_trace.on.get().then(|| {
                        let tr = self.geo_trace.clone();
                        std::rc::Rc::new(move |which: crate::transcript::RowProbe| match which {
                            crate::transcript::RowProbe::Code => super::dev::GeoTrace::probe(&tr, |g| &g.code),
                            crate::transcript::RowProbe::Tool => super::dev::GeoTrace::probe(&tr, |g| &g.tool),
                        }) as std::rc::Rc<dyn Fn(crate::transcript::RowProbe) -> Option<gpui::AnyElement>>
                    }),
                };
                let column = self.column_max();
                // While a panel slides, the list's box glides with the
                // composer's (`ColumnGeo::dock_w`, left edge from `card_room`),
                // so card edges and right-aligned icons move with it every
                // frame. Text inside holds one wrap (`wrap_hold` above).
                let held = self.column_slide.map(|_| px(self.column_last.dock_w));
                d.flex().flex_row().overflow_hidden().child(
                    // Match empty-chat `pt_6`: keep ListAlignment::Top, but leave
                    // comfortable room under the header so the first bubble isn't flush.
                    // The list spans the pane so the wheel scrolls it anywhere;
                    // each row centers itself in the reading column.
                    div()
                        .h_full()
                        .w_full()
                        .pt_6()
                        .pl(px(card_room.0))
                        .pr(px(card_room.1))
                        .opacity(reveal_opacity)
                        .flex()
                        .flex_col()
                        .child(div().flex_none().h(px(card_room.2)))
                        .child(div().relative().flex_1().min_h_0().when_some(held, |d, w| d.w(w)).when(held.is_none(), |d| d.w_full()).children(super::dev::GeoTrace::probe(&self.geo_trace, |g| &g.list)).child(
                        list(self.transcript.list_state.clone(), move |ix, _, _| {
                            let row = match items.get(ix) {
                                Some(item) => {
                                    let lines = mr_rows.get(&ix);
                                    let loader = loader.filter(|_| loader_row == Some(ix));
                                    let copy = crate::transcript::message_text(&items, ix);
                                    // Its message's Copy shows while any of its rows is hovered.
                                    let shown = hover_cell.row.get().and_then(|h| items.get(h)).is_some_and(|h| h.message_id == item.message_id);
                                    if lines.is_none() && loader.is_none() && copy.is_none() {
                                        render_row(ix, item, ix.checked_sub(1).and_then(|p| items.get(p)), &rc)
                                    } else {
                                        div()
                                            .w_full()
                                            .flex()
                                            .flex_col()
                                            .child(render_row(ix, item, ix.checked_sub(1).and_then(|p| items.get(p)), &rc))
                                            .children(copy.map(|text| crate::transcript::message_copy(item, text, shown, &rc)))
                                            .children(lines.into_iter().flatten().map(|line| super::mr_chip::render_inline(line, t, reduced, &on_mr)))
                                            .children(loader.map(|loader| loader.render()))
                                            .into_any()
                                    }
                                }
                                None => div().into_any(),
                            };
                            let hover = hover_cell.clone();
                            div()
                                .relative()
                                .w_full()
                                .flex()
                                .justify_center()
                                // Which row the mouse is over, checked every frame (not
                                // only on mouse moves), so a scroll under a still mouse
                                // follows too. Rows stack with no gaps; leaving clears
                                // only its own row, so moving within a message never
                                // drops the hover, whatever order the rows paint in.
                                .child(
                                    gpui::canvas(
                                        |bounds, window, _| window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal),
                                        move |_, hitbox, window, _| {
                                            // This row's verdict: set it, true when that changed.
                                            let settle = |hover: &super::RowHover, window: &gpui::Window| {
                                                let now = hover.row.get();
                                                let over = !hover.outside.get() && hitbox.is_hovered(window);
                                                let next = if over { Some(ix) } else { now.filter(|h| *h != ix) };
                                                hover.row.set(next);
                                                next != now
                                            };
                                            // A refresh asked for mid-draw is dropped: a change
                                            // found while painting (a scroll moved rows under a
                                            // still mouse) shows on the next frame instead.
                                            if settle(&hover, window) {
                                                window.request_animation_frame();
                                            }
                                            let exit = hover.clone();
                                            window.on_mouse_event(move |_: &gpui::MouseExitEvent, _, window, _| {
                                                exit.outside.set(true);
                                                if exit.row.take().is_some() {
                                                    window.refresh();
                                                }
                                            });
                                            // A move settles it at once and repaints (moves
                                            // repaint only when something asks).
                                            let moved = hover.clone();
                                            let hitbox = hitbox.clone();
                                            window.on_mouse_event(move |_: &gpui::MouseMoveEvent, phase, window, _| {
                                                if phase != gpui::DispatchPhase::Bubble {
                                                    return;
                                                }
                                                moved.outside.set(false);
                                                let now = moved.row.get();
                                                let next = if hitbox.is_hovered(window) { Some(ix) } else { now.filter(|h| *h != ix) };
                                                if next != now {
                                                    moved.row.set(next);
                                                    window.refresh();
                                                }
                                            });
                                        },
                                    )
                                    .absolute()
                                    .inset_0(),
                                )
                                // Room under the newest message (and its loader), so
                                // the turn ends well clear of the input.
                                .when(ix + 1 == items.len(), |d| d.pb(zpx(TAIL_ROOM) + px(dock_h)))
                                .child(div().w_full().max_w(column).child(row))
                                .into_any()
                        })
                        .w_full()
                        .h_full(),
                    )),
                )
            })
            .child(card_measure)
            .children(timer)
            .children(card)
    }

    /// The "Latest" pill, painted over the composer. Inside the transcript
    /// the page column clips it, so a scroll that lands it on that edge
    /// covers it for a frame.
    fn render_latest_pill(&mut self, t: Tokens, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let now = std::time::Instant::now();
        if !self.latest_pill.mounted(now) {
            return None;
        }
        let reduced = self.reduced();
        let running_since = (self.active_status() == Some(ChatStatus::Running)).then_some(self.loader_seed.0);
        // Floating (absolute), so its motion never shifts the list. Scale is
        // drawn by sizing about its centre: gpui divs have no transform.
        // Leaving, it takes no clicks or hover.
        let look = self.latest_pill.look_at(now);
        let live = !self.latest_pill.leaving(now);
        let k = look.scale;
        let rise = 12. - look.dy + 14. * (1. - k);
        let dock_h = self.dock_h.get();
        // Deferred so the page column's clip and the composer's shadow
        // cannot cover it. Priority 1 keeps it under menus.
        Some(
            gpui::deferred(
            div()
                .absolute()
                .bottom(px(dock_h) + zpx(rise))
                .left_0()
                .right_0()
                .pl(px(self.card_column_room().0))
                .pr(px(self.card_column_room().1))
                .flex()
                .justify_center()
                .child(
                    div()
                        .id("jump-to-latest")
                        .opacity(look.opacity)
                        .h(zpx(28. * k))
                        .pl(zpx(8. * k))
                        .pr(zpx(12. * k))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(zpx(4. * k))
                        .rounded_full()
                        .floating_surface(t)
                        // Over the floating surface: a 6% text-colour lift and
                        // an 18% edge, so it reads on the transcript without shouting.
                        .bg(c(lift(t.floating, t.text, 0.06)))
                        .border_color(c(t.text & 0xFFFF_FF00 | 0x2E))
                        .t_small()
                        .text_size(zpx(crate::style::SMALL.size * k))
                        .line_height(zpx(crate::style::SMALL.line * k))
                        .text_color(c(t.text & 0xFFFF_FF00 | 0xE6))
                        .when(live, |d| d.cursor_pointer().hover(|s| s.text_color(c(t.text))))
                        .map(|d| match running_since {
                            // Run is live but out of view: the one small cue
                            // (the inline loader is up there, not duplicated here).
                            Some(since) => d
                                .gap(zpx(6. * k))
                                .child(crate::motion::dot_grid(c(t.text), "latest-running-dots", reduced))
                                .child(div().child("Running"))
                                .child(div().text_color(c(t.text_muted)).child(crate::motion::elapsed_label(since.elapsed())))
                                .child(Icon::ChevronDown.el(zpx(13. * k))),
                            None => d.child(Icon::ChevronDown.el(zpx(13. * k))).child("Latest"),
                        })
                        .when(live, |d| {
                            d.on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.transcript.glide_to_latest(std::time::Instant::now());
                                cx.notify();
                            }))
                        }),
                )
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    /// A chat with no rows yet: a skeleton while its history loads, then a hint.
    fn render_empty_chat(&self, t: Tokens) -> impl IntoElement {
        let loading = self.chat_stream.is_none();
        let reduced = self.reduced();
        let chat = self.current_chat.as_ref().and_then(|id| self.state.chats.get(id));
        let space = chat
            .and_then(|chat| self.state.spaces.get(&chat.space_id))
            .map(|space| space.name.clone())
            .unwrap_or_default();
        let logo = chat.map(|chat| crate::icons::Icon::harness(&chat.harness));
        let harness = chat
            .map(|chat| {
                self.harnesses
                    .iter()
                    .find(|h| h.id == chat.harness)
                    .map(|h| h.display_name.clone())
                    .unwrap_or_else(|| chat.harness.clone())
            })
            .unwrap_or_default();
        let bar = |id: &'static str, width: f32| {
            crate::motion::pulse(
                div().h(zpx(12.)).w(gpui::relative(width)).rounded(RADIUS_SM).bg(c(t.surface)),
                id,
                reduced,
            )
        };
        // History usually lands within a frame or two: the skeleton waits
        // SKELETON_DELAY before fading in, so a quick load never flashes it.
        let load = self.chat_gen as usize;
        let skeleton = move |d: gpui::Div| {
            use gpui::AnimationExt as _;
            let bars = d.child(bar("skeleton-1", 0.55)).child(bar("skeleton-2", 0.8)).child(bar("skeleton-3", 0.4));
            let (wait, fade) = (SKELETON_DELAY.as_secs_f32(), crate::motion::BASE.as_secs_f32());
            bars.with_animation(("skeleton-in", load), gpui::Animation::new(SKELETON_DELAY + crate::motion::BASE), move |el, t| {
                el.opacity(((t * (wait + fade) - wait) / fade).clamp(0., 1.))
            })
            .into_any_element()
        };
        div().absolute().inset_0().flex().justify_center().child(
            div().w_full().max_w(self.column_max()).px_4().pt_6().flex().flex_col().gap_3().map(|d| {
                if loading {
                    d.child(skeleton(div().flex().flex_col().gap_3()))
                } else {
                    d.items_center().pt_16().child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1p5()
                            .t_small()
                            .text_color(c(t.text_faint))
                            .when(!harness.is_empty(), |d| d.children(logo.map(|logo| logo.el(zpx(13.)))))
                            .child(match (space.is_empty(), harness.is_empty()) {
                                (false, false) => format!("New chat in {space} · {harness}"),
                                (false, true) => format!("New chat in {space}"),
                                _ => "New chat".to_string(),
                            }),
                    )
                }
            }),
        )
    }

    /// Settings over the whole window, sidebars included. The header stays
    /// so the sidebar toggle still works. The page fades and rises 8 px
    /// into place; leaving drops the same 8 px, quicker. A close keeps it
    /// painted until that exit finishes.
    fn render_settings_page(&mut self, t: Tokens, window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui::AnimationExt as _;
        if !self.browser_sites_synced {
            self.browser_sites_synced = true;
            let saved = self.state.ui_settings.browser_allowed_sites.join(", ");
            self.browser_sites.update(cx, |input, cx| input.set_value(saved, window, cx));
        }
        let reduced = self.reduced();
        let sheet = self.settings_gen;
        let leaving = self.settings_leaving == Some(sheet);
        // ⌘, opens this often enough that a sheet-length move feels late.
        // Enter 220, leave 160: the response is quicker than the arrival.
        let duration = if reduced {
            std::time::Duration::from_millis(1)
        } else if leaving {
            crate::motion::BASE
        } else {
            crate::motion::SLOW + crate::motion::FAST / 6
        };
        // `relative` + `top`, not margin and not the overlay's inset: an
        // absolute inset-0 layer stretches when its top inset moves. gpui
        // has no transform on a div, so this is the move that doesn't reflow.
        let page = div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(c(t.window))
            .child(self.render_header(t, true, false, true, cx))
            .child(self.render_settings(t, cx));
        let page = page.with_animation(
            ("settings-sheet", sheet as usize),
            gpui::Animation::new(duration).with_easing(crate::motion::ease_out),
            move |el, delta| {
                let (opacity, dy) = crate::motion::sheet_look(delta, leaving, reduced);
                el.opacity(opacity).top(zpx(dy))
            },
        );
        div().absolute().inset_0().occlude().overflow_hidden().child(page)
    }

    /// Delete-a-folder confirmation, same scrim as the chat one, over the
    /// whole window rather than inline in the sidebar.
    fn render_delete_space_modal(&mut self, t: Tokens, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        self.state.confirming_delete_space.as_ref()?;
        Some(gpui::deferred(
            div()
                .id("delete-space-modal")
                .absolute()
                .inset_0()
                .occlude()
                .bg(gpui::rgba(0x000000B3))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.state.confirming_delete_space = None;
                    cx.notify();
                }))
                .children(self.render_delete_prompt_card(t, cx)),
        )
        .with_priority(3))
    }

    /// Centered delete confirmation: scrim + card, with enter and leave motion.
    fn render_delete_chats_modal(&mut self, t: Tokens, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let (ids, leaving, modal_gen) = match (&self.confirming_delete_chats, &self.delete_confirm_leaving) {
            (Some(ids), _) => (ids.clone(), false, self.delete_confirm_gen),
            (None, Some((n, ids))) => (ids.clone(), true, *n),
            _ => return None,
        };
        let heading = match ids.as_slice() {
            [one] => {
                let title = self.state.chats.get(one).map(|c| c.title.clone()).filter(|t| !t.is_empty());
                format!("Delete “{}”?", title.unwrap_or_else(|| "New chat".into()))
            }
            many => format!("Delete {} chats?", many.len()),
        };
        let reduced = self.reduced();
        let delete_ids = ids;
        let card = div()
            .id(("delete-chats-modal-card", modal_gen as usize))
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
            .child(div().t_title().font_weight(gpui::FontWeight::SEMIBOLD).child(heading))
            .child(
                div()
                    .t_body()
                    .text_color(c(t.text_muted))
                    .child("Their worktrees go too; your folders stay."),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap_2()
                    .child(super::sidebar::text_button("cancel-delete-chats-btn", "Cancel", t).on_click(cx.listener(
                        |this, _: &ClickEvent, _, cx| this.dismiss_delete_confirm(cx),
                    )))
                    .child(super::sidebar::filled_button("confirm-delete-chats-btn", "Delete", t.danger, t).on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.delete_chats(delete_ids.clone(), cx);
                        }),
                    )),
            );
        let card = if leaving {
            crate::motion::dismiss(card, ("delete-modal-out", modal_gen as usize), reduced)
        } else {
            crate::motion::appear(card, ("delete-modal-in", modal_gen as usize), crate::motion::BASE, 14., reduced)
                .into_any_element()
        };
        Some(gpui::deferred(
            div()
                .id(("delete-chats-modal", modal_gen as usize))
                .absolute()
                .inset_0()
                .occlude()
                .bg(gpui::rgba(0x000000B3))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.dismiss_delete_confirm(cx)))
                .child(card),
        )
        .with_priority(3))
    }

    /// The overlay: the backdrop fades while the content rises 8 px and
    /// grows from 96% (the "Latest" pill's motion); closing plays it back,
    /// taking no clicks on the way out. A flip mid-way turns from where it is.
    fn render_lightbox(&mut self, t: Tokens, window: &mut Window, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let now = std::time::Instant::now();
        if !self.lightbox_fx.mounted(now) {
            self.lightbox = None;
            return None;
        }
        // Its own frames: the transcript's render (which asks for the
        // pill's) does not run on the new-chat page.
        if self.lightbox_fx.animating(now) {
            window.request_animation_frame();
        }
        let look = self.lightbox_fx.look_at(now);
        let live = !self.lightbox_fx.leaving(now);
        let content = match self.lightbox.clone()? {
            super::Lightbox::Image(path) => {
                gpui::img(path).size_full().object_fit(gpui::ObjectFit::Contain).into_any_element()
            }
            super::Lightbox::Diagram { row_id, source, view } => div()
                .id("lightbox-diagram")
                .relative()
                .size_full()
                .rounded(crate::style::RADIUS_LG)
                .bg(crate::style::c(t.code))
                .border_1()
                .border_color(crate::style::c(t.border))
                .overflow_hidden()
                // Panning ends in a click: keep it from closing the overlay.
                .on_click(|_, _, cx| cx.stop_propagation())
                .child(crate::mermaid_viewer::viewer(source.clone(), t, view.clone()))
                .child(self.render_diagram_toolbar(t, row_id, source, view, cx))
                .into_any_element(),
            super::Lightbox::Output { title, content, error } => self.render_output_lightbox(t, title, content, error, cx),
        };
        // Scale is drawn by sizing about the centre: gpui divs have no transform.
        let k = 0.9 * look.scale;
        Some(gpui::deferred(
            div()
                .id("lightbox")
                .absolute()
                .inset_0()
                .bg(gpui::black().opacity(0.7 * look.opacity))
                .flex()
                .items_center()
                .justify_center()
                .when(live, |d| {
                    d.occlude().cursor_pointer().on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.close_lightbox(cx);
                    }))
                })
                .child(
                    div()
                        .relative()
                        .top(zpx(look.dy))
                        .opacity(look.opacity)
                        .w(gpui::relative(k))
                        .h(gpui::relative(k))
                        .child(content),
                ),
        )
        .with_priority(2))
    }

    /// Zoom and copy controls floating over the expanded diagram.
    fn render_diagram_toolbar(
        &self,
        t: Tokens,
        row_id: String,
        source: String,
        view: crate::mermaid_viewer::SharedViewport,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        use crate::transcript::{bar_button, image_copy_key};
        let natural = crate::mermaid::size(&source).unwrap_or((1., 1.));
        let percent = (view.borrow().scale(natural) * 100.).round();
        let copied = self.copied.as_ref().map(|(row, _)| row.as_str());
        let (code_done, image_done) = (copied == Some(row_id.as_str()), copied == Some(image_copy_key(&row_id).as_str()));
        let tint = |done: bool| if done { t.success } else { t.text_muted };
        let zoom = |id: &'static str, icon: Icon, factor: f32| {
            let view = view.clone();
            bar_button(id.into(), icon, "", t.text_muted, t).on_click(move |_, window, _| {
                view.borrow_mut().zoom_center(factor, natural);
                window.refresh();
            })
        };
        let fit = view.clone();
        let (image_id, image_src, code_id, code_src) = (row_id.clone(), source.clone(), row_id, source);
        div()
            .absolute()
            .top_3()
            .right_3()
            .flex()
            .flex_row()
            .items_center()
            .gap_0p5()
            .p_1()
            .rounded(RADIUS_MD)
            .bg(crate::style::c(t.floating))
            .border_1()
            .border_color(crate::style::c(t.border))
            .t_caption()
            .text_color(crate::style::c(t.text_muted))
            .child(zoom("viewer-zoom-out", Icon::Minus, 1. / 1.25))
            .child(
                bar_button("viewer-fit".into(), Icon::Shrink, "", t.text_muted, t)
                    .child(format!("{percent}%"))
                    .on_click(move |_, window, _| {
                        fit.borrow_mut().reset();
                        window.refresh();
                    }),
            )
            .child(zoom("viewer-zoom-in", Icon::Plus, 1.25))
            .child(div().w_px().h(zpx(14.)).mx_1().bg(crate::style::c(t.border)))
            .child(
                bar_button(
                    "viewer-copy-image".into(),
                    if image_done { Icon::Check } else { Icon::Image },
                    if image_done { "Copied" } else { "Copy image" },
                    tint(image_done),
                    t,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.copy_image(image_id.clone(), image_src.clone(), t, cx)
                })),
            )
            .child(
                bar_button(
                    "viewer-copy-code".into(),
                    if code_done { Icon::Check } else { Icon::Copy },
                    if code_done { "Copied" } else { "Copy code" },
                    tint(code_done),
                    t,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.copy_code(code_id.clone(), code_src.clone(), cx)
                })),
            )
            .child(
                bar_button("viewer-close".into(), Icon::Close, "", t.text_muted, t).on_click(cx.listener(
                    |this, _: &ClickEvent, _, cx| {
                        this.close_lightbox(cx);
                    },
                )),
            )
    }

    /// Full-window scrollable overlay for command outputs and code blocks.
    fn render_output_lightbox(
        &self,
        t: Tokens,
        title: Option<String>,
        content: String,
        error: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        use crate::transcript::bar_button;
        let title_text = title.unwrap_or_else(|| (if error { "Command Error" } else { "Command Output" }).to_string());
        let line_count = content.lines().count();
        let bytes = content.len();
        let bytes_text = if bytes < 1024 {
            format!("{bytes} B")
        } else if bytes < 1024 * 1024 {
            format!("{:.1} KB", bytes as f32 / 1024.0)
        } else {
            format!("{:.1} MB", bytes as f32 / (1024.0 * 1024.0))
        };
        let copy_content = content.clone();
        let copied = self.copied.as_ref().map(|(row, _)| row.as_str()) == Some("lightbox-output");

        let variant = if error { crate::code_block::CodeVariant::Destructive } else { crate::code_block::CodeVariant::Default };
        let bg_color = crate::code_block::surface(t, variant);
        let border_color = crate::code_block::border_of(t, variant);
        let header_bg = if error {
            crate::code_block::mix(t.danger, t.surface, 0.12)
        } else {
            t.surface
        };
        let header_border = if error {
            crate::style::c(t.danger).opacity(0.35)
        } else {
            crate::style::c(t.border)
        };

        div()
            .id("lightbox-output")
            .relative()
            .size_full()
            .rounded(crate::style::RADIUS_LG)
            .bg(crate::style::c(bg_color))
            .border_1()
            .border_color(border_color)
            .overflow_hidden()
            .flex()
            .flex_col()
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .h(zpx(40.))
                    .px_4()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(header_border)
                    .bg(crate::style::c(header_bg))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .items_center()
                            .gap_2()
                            .when(error, |d| {
                                d.child(
                                    div()
                                        .flex_none()
                                        .size(zpx(6.))
                                        .rounded_full()
                                        .bg(crate::style::c(t.danger)),
                                )
                            })
                            .child(
                                div()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .t_caption()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(crate::style::c(if error { t.danger } else { t.text }))
                                    .child(title_text),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .t_caption()
                                    .text_color(if error {
                                        crate::style::c(t.danger).opacity(0.75)
                                    } else {
                                        crate::style::c(t.text_faint)
                                    })
                                    .child(format!("{line_count} lines • {bytes_text}")),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                bar_button(
                                    "lightbox-output-copy".into(),
                                    if copied { Icon::Check } else { Icon::Copy },
                                    if copied { "Copied" } else { "Copy" },
                                    if copied { t.success } else if error { t.danger } else { t.text },
                                    t,
                                )
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.copy_code("lightbox-output".into(), copy_content.clone(), cx);
                                })),
                            )
                            .child(
                                bar_button(
                                    "lightbox-output-close".into(),
                                    Icon::Close,
                                    "",
                                    if error { t.danger } else { t.text_muted },
                                    t,
                                )
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.close_lightbox(cx);
                                })),
                            ),
                    ),
            )
            .child(
                div()
                    .id("lightbox-output-body")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .p_4()
                    .overflow_y_scroll()
                    .font_family(crate::style::MONO)
                    .text_size(zpx(12.))
                    .line_height(zpx(18.))
                    .text_color(crate::style::c(t.text))
                    .whitespace_normal()
                    .child(content),
            )
            .into_any_element()
    }

    /// The turn's loader, standalone: only used while a running chat has no
    /// rows yet. Otherwise it renders inline as the turn's last line.
    fn render_thinking(&self, t: Tokens) -> Option<impl IntoElement> {
        if self.active_status() != Some(ChatStatus::Running) {
            return None;
        }
        let (since, seed) = self.loader_seed;
        let loader = InlineLoader { tokens: t, verbs: self.loader_verbs, since, seed, reduced: self.reduced(), look: None };
        // Same column as the transcript list; the loader brings the rows' `px_4`.
        // Clear of the floating composer.
        Some(div().flex_none().pb(px(self.dock_h.get())).flex().justify_center().child(
            div().w_full().max_w(self.column_max()).child(loader.render()),
        ))
    }

    /// The error toast: top center under the header. Drops in, leaves on
    /// its own after [`super::NOTICE_HOLD`] or on ×, lifting back up.
    fn render_notice(&mut self, t: Tokens, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        use gpui::AnimationExt as _;
        let message = self.status_message.clone()?;
        let generation = self.notice_gen;
        let leaving = self.notice_leaving;
        if !leaving && self.notice_hold.as_ref().is_none_or(|(g, _)| *g != generation) {
            let hold = cx.spawn(async move |this, cx| {
                cx.background_executor().timer(super::NOTICE_HOLD).await;
                let _ = this.update(cx, |this, cx| {
                    if this.notice_gen == generation {
                        this.dismiss_notice(cx);
                    }
                });
            });
            self.notice_hold = Some((generation, hold));
        }
        let danger = c(t.danger);
        let card = div()
            .id("status-message")
            .max_w(zpx(440.))
            .min_w_0()
            .pl(zpx(10.))
            .pr(zpx(6.))
            .py(zpx(6.))
            .floating_surface(t)
            .rounded(zpx(12.))
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(10.))
            .child(
                div()
                    .size(zpx(22.))
                    .flex_none()
                    .rounded_full()
                    .bg(danger.opacity(0.14))
                    .text_color(danger)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Icon::Alert.el(zpx(12.))),
            )
            .child(div().flex_1().min_w_0().t_small().text_color(c(t.text)).line_clamp(2).child(message))
            .child(
                div()
                    .id("status-message-close")
                    .size(zpx(22.))
                    .flex_none()
                    .rounded(RADIUS_SM)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(c(t.text_faint))
                    .cursor_pointer()
                    .hover(|s| s.bg(c(t.hover)).text_color(c(t.text)))
                    .child(Icon::Close.el(zpx(11.)))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.dismiss_notice(cx))),
            );
        // In: drop 10 px from above, strong ease-out. Out: lift 6 px, faster.
        // Reduced motion keeps the fades, drops the travel.
        let reduced = self.reduced();
        let travel = if reduced { 0. } else { 1. };
        let card: gpui::AnyElement = if leaving {
            card.with_animation(
                ElementId::Name(format!("notice-out-{generation}").into()),
                gpui::Animation::new(super::NOTICE_EXIT).with_easing(crate::motion::ease_in),
                move |el, d| el.opacity(1. - d).relative().top(zpx(-6. * d * travel)),
            )
            .into_any_element()
        } else {
            card.with_animation(
                ElementId::Name(format!("notice-in-{generation}").into()),
                gpui::Animation::new(super::NOTICE_ENTER).with_easing(crate::motion::ease_out_strong),
                move |el, d| el.opacity(d).relative().top(zpx(-10. * (1. - d) * travel)),
            )
            .into_any_element()
        };
        Some(
            div()
                .absolute()
                .top(HEADER_HEIGHT + px(8.))
                .left_0()
                .right_0()
                .px_4()
                .flex()
                .justify_center()
                .child(card)
                .into_any_element(),
        )
    }

    /// The open subagent's session, in place of the transcript: its own
    /// transcript (the engine's `subagents.{id}` view) under a header with
    /// status. The takeover composer is the docked bar below.
    fn render_subagent_session(&mut self, t: Tokens, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(record) = self.open_subagent().cloned() else {
            return div().flex_1().min_h_0().into_any_element();
        };
        let name = record.get("name").and_then(|v| v.as_str()).unwrap_or("subagent").to_string();
        let description = record.get("description").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let status = record.get("status").and_then(|v| v.as_str()).unwrap_or("running").to_string();
        let blocked = record.get("blocked_question").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let (color, label) = subagent_status(&status, t);

        let parts = record.get("parts").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let rows: Vec<gpui::AnyElement> = parts
            .iter()
            .filter_map(|part| {
                let kind = part.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                match kind {
                    "text" => {
                        let body = part.get("body").and_then(|v| v.as_str()).unwrap_or_default();
                        (!body.trim().is_empty())
                            .then(|| div().t_body().text_color(c(t.text)).child(body.to_string()).into_any_element())
                    }
                    "reasoning" => {
                        let body = part.get("body").and_then(|v| v.as_str()).unwrap_or_default();
                        (!body.trim().is_empty()).then(|| {
                            div()
                                .t_small()
                                .text_color(c(t.text_muted))
                                .border_l_1()
                                .border_color(c(t.border))
                                .pl_3()
                                .child(body.to_string())
                                .into_any_element()
                        })
                    }
                    "tool" => {
                        let name = part.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
                        let state = part.get("state").and_then(|v| v.as_str()).unwrap_or("done");
                        let summary = part.get("summary").and_then(|v| v.as_str()).unwrap_or_default();
                        let dot = match state {
                            "error" => t.danger,
                            "running" => t.accent,
                            _ => t.success,
                        };
                        Some(
                            div()
                                .h(zpx(24.))
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .t_small()
                                .child(div().size(zpx(6.)).flex_none().rounded_full().bg(c(dot)))
                                .child(div().flex_none().font_family(MONO).text_color(c(t.text)).child(name.to_string()))
                                .child(
                                    div()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .text_color(c(t.text_muted))
                                        .child(summary.to_string()),
                                )
                                .into_any_element(),
                        )
                    }
                    _ => None,
                }
            })
            .collect();

        div()
            .id("subagent-session")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px_4()
                    .pt_3()
                    .pb_2()
                    .w_full()
                    .max_w(self.column_max())
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .id("subagent-back")
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1p5()
                            .t_small()
                            .text_color(c(t.text_muted))
                            .cursor_pointer()
                            .hover(|s| s.text_color(c(t.text)))
                            .child(Icon::Back.el(zpx(13.)))
                            .child("Back to chat")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_subagent_session(cx))),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .t_title()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(c(t.text))
                                    .child(name.clone()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .px_1p5()
                                    .rounded(RADIUS_SM)
                                    .bg(c(color & 0xFFFF_FF00 | 0x1A))
                                    .t_caption()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(c(color))
                                    .child(label),
                            ),
                    )
                    .when(!description.is_empty(), |d| {
                        d.child(div().t_small().text_color(c(t.text_faint)).child(description))
                    }),
            )
            .child(
                div()
                    .px_4()
                    .pb_4()
                    .w_full()
                    .max_w(self.column_max())
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(rows)
                    .when(!blocked.is_empty(), |d| {
                        d.child(
                            div()
                                .rounded(RADIUS_MD)
                                .bg(c(t.surface))
                                .border_1()
                                .border_color(c(t.warning))
                                .px_3()
                                .py_2()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .t_small()
                                .text_color(c(t.text))
                                .child(div().text_color(c(t.warning)).child(Icon::Alert.el(zpx(13.))))
                                .child(format!("Waiting for input: {blocked}")),
                        )
                    }),
            )
            .into_any_element()
    }

    /// In the composer slot while a subagent session is open: where it
    /// stands. Subagents take no input of their own.
    fn render_subagent_note(&self, t: Tokens) -> gpui::AnyElement {
        let note = |text: &str| {
            div()
                .px_4()
                .pb_3()
                .flex()
                .justify_center()
                .child(div().t_small().text_color(c(t.text_faint)).child(text.to_string()))
                .into_any_element()
        };
        let Some(record) = self.open_subagent() else {
            return note("This subagent is gone.");
        };
        if record.get("status").and_then(|v| v.as_str()).unwrap_or("running") != "running" {
            return note("This subagent has ended.");
        }
        note("This subagent is working. Go back to the chat to reply.")
    }

    /// The header button's dropdown: every subagent of the chat with its
    /// status; click one to open its session.
    fn render_subagent_menu(&mut self, t: Tokens, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.show_subagent_menu || self.current_chat.is_none() {
            return None;
        }
        let rows: Vec<gpui::AnyElement> = self
            .subagent_list()
            .into_iter()
            .map(|sub| {
                let id = sub.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let name = sub.get("name").and_then(|v| v.as_str()).unwrap_or("subagent").to_string();
                let status = sub.get("status").and_then(|v| v.as_str()).unwrap_or("running").to_string();
                let (color, label) = subagent_status(&status, t);
                div()
                    .id(ElementId::Name(format!("subagent-menu-{id}").into()))
                    .h(zpx(30.))
                    .px_2()
                    .rounded(RADIUS_SM)
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .t_small()
                    .cursor_pointer()
                    .hover(|s| s.bg(c(t.hover)))
                    .child(div().size(zpx(7.)).flex_none().rounded_full().bg(c(color)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_color(c(t.text))
                            .child(name),
                    )
                    .child(div().flex_none().t_caption().text_color(c(color)).child(label))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_subagent_session(&id, cx)))
                    .into_any_element()
            })
            .collect();
        let panel = div()
            .id("subagent-menu")
            .w(zpx(300.))
            .max_h(zpx(360.))
            .overflow_y_scroll()
            .p_1p5()
            .flex()
            .flex_col()
            .gap(zpx(2.))
            .floating_surface(t)
            .child(div().px_2().pt_1().t_caption().text_color(c(t.text_faint)).child("SUBAGENTS"))
            .children(rows);
        Some(
            div()
                .absolute()
                .top(HEADER_HEIGHT + zpx(6.))
                .right(zpx(8.))
                .child(panel)
                .into_any_element(),
        )
    }

    /// While the suggestion hint shows, the transcript fades into the window
    /// color above the input, so scrolled text never runs behind the hint.
    /// Paint only: no hitbox, so scrolling and clicks reach the transcript.
    fn render_hint_band(&mut self, t: Tokens, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        use gpui::AnimationExt as _;
        let phase = self.menu_phase("prompt-suggestion-band", self.can_accept_suggestion(cx), cx)?;
        let color = c(t.window);
        let height = px(self.dock_h.get()) + zpx(HINT_BAND_SOLID + HINT_BAND_RAMP);
        let ramp = f32::from(zpx(HINT_BAND_RAMP));
        let band = div().absolute().left_0().right_0().bottom_0().h(height).child(
            gpui::canvas(|_, _, _| {}, move |bounds, _, window, _| paint_hint_band(bounds, ramp, color, window))
                .absolute()
                .inset_0(),
        );
        Some(match phase {
            _ if self.reduced() => band.into_any_element(),
            None => band
                .with_animation(
                    "hint-band-in",
                    gpui::Animation::new(crate::motion::BASE).with_easing(crate::motion::ease_out),
                    |el, d| el.opacity(crate::motion::unit(d)),
                )
                .into_any_element(),
            Some(n) => band
                .with_animation(
                    ("hint-band-out", n as usize),
                    gpui::Animation::new(crate::motion::FAST).with_easing(crate::motion::ease_out),
                    |el, d| el.opacity(crate::motion::unit(1. - d)),
                )
                .into_any_element(),
        })
    }
}

const HINT_BAND_SOLID: f32 = 14.;
const HINT_BAND_RAMP: f32 = TAIL_ROOM - HINT_BAND_SOLID;
const HINT_BAND_STEPS: usize = 8;

/// Clear at the top, eased out to solid `color` over `ramp` px, solid below.
fn paint_hint_band(bounds: gpui::Bounds<gpui::Pixels>, ramp: f32, color: gpui::Hsla, window: &mut Window) {
    let scale = window.scale_factor();
    let top = f32::from(bounds.origin.y);
    let ramp = ramp.min(f32::from(bounds.size.height));
    let edge = |y: f32| px(((top + ramp * y) * scale).round() / scale);
    let alpha = |y: f32| color.a * (1. - (1. - y) * (1. - y));
    for i in 0..HINT_BAND_STEPS {
        let (a, b) = (i as f32 / HINT_BAND_STEPS as f32, (i + 1) as f32 / HINT_BAND_STEPS as f32);
        let piece = gpui::Bounds::from_corners(gpui::point(bounds.origin.x, edge(a)), gpui::point(bounds.right(), edge(b)));
        let stop = |y: f32| gpui::Hsla { a: alpha(y), ..color };
        window.paint_quad(gpui::fill(
            piece,
            gpui::linear_gradient(180., gpui::linear_color_stop(stop(a), 0.), gpui::linear_color_stop(stop(b), 1.)),
        ));
    }
    let solid = gpui::Bounds::from_corners(gpui::point(bounds.origin.x, edge(1.)), bounds.bottom_right());
    window.paint_quad(gpui::fill(solid, color));
}

/// Fixed height of the loader line, so rows below never shift as it appears,
/// changes verb, or goes away.
const LOADER_HEIGHT: f32 = 28.;

/// The running turn's loader line: a verb for what the agent is doing,
/// rotating every `VERB_SECS`, and the elapsed time.
#[derive(Clone, Copy)]
struct InlineLoader {
    tokens: Tokens,
    verbs: &'static [&'static str],
    since: std::time::Instant,
    seed: usize,
    reduced: bool,
    /// Mid entrance or exit (timestamp-driven); `None` at rest.
    look: Option<crate::motion::RowLook>,
}

impl InlineLoader {
    fn render(self) -> gpui::AnyElement {
        let t = self.tokens;
        // The grid's animation re-renders every frame, so the verb and timer tick on their own.
        let verb = self.verbs[(self.seed + (self.since.elapsed().as_secs() / VERB_SECS) as usize) % self.verbs.len()];
        // `px_4`: the transcript rows' inset (`render_row`), so the grid's
        // left edge is the assistant text's left edge.
        let look = self.look.unwrap_or(crate::motion::RowLook { opacity: 1., dy: 0., height: 1. });
        div()
            .w_full()
            .flex_none()
            .overflow_hidden()
            .h(zpx(LOADER_HEIGHT * look.height))
            .mt(zpx(4. * look.height))
            .px_4()
            .child(
                div()
                    .relative()
                    .top(zpx(look.dy))
                    .opacity(look.opacity)
                    .h(zpx(LOADER_HEIGHT))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .t_small()
                    .line_height(zpx(LOADER_HEIGHT))
                    .text_color(c(t.text_muted))
                    .child(div().flex_none().flex().items_center().child(crate::motion::dot_grid(c(t.text_muted), "thinking-dots", self.reduced)))
                    .child(div().text_color(c(t.text_muted)).child(format!("{verb}…")))
                    .child(div().text_color(c(t.text_faint)).child(crate::motion::elapsed_label(self.since.elapsed()))),
            )
            .into_any_element()
    }
}

/// The window color over the pane picture. `u` is 0 on the hero (clean landscape
/// with no veil) and smoothly closes the solid window color down over the picture
/// as the user enters a chat, ending with the subtle [`CHAT_GLOW`] foot glow.
/// A settled chat also blurs that foot. gpui gradients take two stops, so
/// the curve is short bands with edges snapped to a device pixel.
fn paint_backdrop_veil(bounds: gpui::Bounds<gpui::Pixels>, window: &mut Window, window_color: gpui::Hsla, u: f32) {
    if u <= 0. {
        return;
    }
    if u >= 1. {
        let foot_top = bounds.origin.y + bounds.size.height * (1. - CHAT_GLOW_HEIGHT);
        let foot = gpui::Bounds::from_corners(gpui::point(bounds.origin.x, foot_top), bounds.bottom_right());
        window.paint_backdrop_blur(foot, gpui::Corners::default(), zpx(48.));
    }
    let cover = |y: f32| {
        let glow_line = 1. - CHAT_GLOW_HEIGHT;
        let chat = if y <= glow_line {
            1.
        } else {
            let t = ((y - glow_line) / CHAT_GLOW_HEIGHT).clamp(0., 1.);
            1. - CHAT_GLOW * t
        };
        chat * u
    };
    let scale = window.scale_factor();
    let (top, height) = (f32::from(bounds.origin.y), f32::from(bounds.size.height));
    let edge = |y: f32| px((((top + height * y) * scale).round()) / scale);
    for i in 0..FADE_BANDS {
        let (a, b) = (i as f32 / FADE_BANDS as f32, (i + 1) as f32 / FADE_BANDS as f32);
        let band = gpui::Bounds::from_corners(gpui::point(bounds.origin.x, edge(a)), gpui::point(bounds.right(), edge(b)));
        let stop = |y: f32| gpui::Hsla { a: window_color.a * cover(y), ..window_color };
        window.paint_quad(gpui::fill(
            band,
            gpui::linear_gradient(180., gpui::linear_color_stop(stop(a), 0.), gpui::linear_color_stop(stop(b), 1.)),
        ));
    }
}

/// Linear pieces of the hero's eased fade into the window.
const FADE_BANDS: usize = 14;

/// How long a `hero_image_path` lookup is reused.
const HERO_PATH_TTL: std::time::Duration = std::time::Duration::from_secs(2);

/// A subagent status's color and label, shared by the indicator row, the
/// menu, and the session header.
fn subagent_status(status: &str, t: Tokens) -> (u32, String) {
    match status {
        "running" => (t.accent, "running".to_string()),
        "blocked" => (t.warning, "waiting for input".to_string()),
        "error" => (t.danger, "failed".to_string()),
        "interrupted" => (t.text_muted, "stopped".to_string()),
        other => (t.text_muted, other.to_string()),
    }
}

/// `base` moved `amount` of the way toward `toward` (RGBA, alpha kept).
fn lift(base: u32, toward: u32, amount: f32) -> u32 {
    let ch = |v: u32, s: u32| ((v >> s) & 0xFF) as f32;
    let mix = |s: u32| ((ch(base, s) + (ch(toward, s) - ch(base, s)) * amount).round() as u32) << s;
    mix(24) | mix(16) | mix(8) | (base & 0xFF)
}

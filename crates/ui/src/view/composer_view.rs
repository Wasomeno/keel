//! Composer dock: text area, picker chips, primary Send / Steer / Stop.

use crate::hover_fade::{HoverFadeExt, reveal_on_hover};
use crate::style::zpx;
use gpui::{AnyElement, ClickEvent, Context, IntoElement, div, prelude::*, px};
use keel_proto::{ChatStatus, PermissionMode};
use keel_theme::Tokens;

use super::{KeelWindow, floating_menu_at, menu_item, menu_label, menu_separator};
use crate::composer::{PrimaryButtonAction, compute_button_action};
use crate::icons::Icon;
use crate::style::{KeelStyled, RADIUS_LG, RADIUS_MD, RADIUS_XL, c};

/// Send button disc and icon ink: the same dark disc whether or not it can
/// send; disabled only dims the whole button. Swaps at once (it follows
/// typing, and keyboard changes don't animate).
fn send_look(enabled: bool, t: Tokens) -> (gpui::Hsla, gpui::Hsla, f32) {
    (
        c(t.text),
        c(t.text_on_accent),
        if enabled { 1. } else { SEND_DISABLED },
    )
}

/// The disabled send button's opacity.
const SEND_DISABLED: f32 = 0.45;

/// Which picker menu is open. At most one. Model carries harness and
/// effort too: one chip in the composer, one menu.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Picker {
    Model,
    Branch,
    Folder,
    Repo,
}

impl Picker {
    pub(super) fn menu_id(self) -> &'static str {
        match self {
            Picker::Model => "model-picker-panel",
            Picker::Branch => "branch-picker-panel",
            Picker::Folder => "folder-picker-panel",
            Picker::Repo => "repo-picker-panel",
        }
    }

    /// Every picker opens on its search box, focused.
    fn searchable(self) -> bool {
        true
    }

    fn search_placeholder(self) -> &'static str {
        match self {
            Picker::Folder => "Search folders",
            Picker::Branch => "Search branches",
            Picker::Repo => "Search issue repos",
            Picker::Model => "Search models",
        }
    }
}

impl KeelWindow {
    pub(super) fn open_picker(&self) -> Option<Picker> {
        if self.show_model_picker {
            Some(Picker::Model)
        } else if self.state.show_branch_picker {
            Some(Picker::Branch)
        } else if self.show_harness_picker {
            // The folder menu reuses the old harness flag, the repo menu the effort one.
            Some(Picker::Folder)
        } else if self.show_effort_picker {
            Some(Picker::Repo)
        } else {
            None
        }
    }

    fn close_effort_menu(&mut self, cx: &mut Context<Self>) {
        self.show_effort_menu = false;
        cx.notify();
    }

    fn close_permission_menu(&mut self, cx: &mut Context<Self>) {
        self.show_permission_menu = false;
        cx.notify();
    }

    pub(super) fn set_picker(&mut self, picker: Option<Picker>, cx: &mut Context<Self>) {
        self.projects.close_scope();
        // Opening a picker leaves the create sheet behind, except when the
        // folder picker is already up and the sheet is what it is showing.
        let opening_folder = picker == Some(Picker::Folder) && !self.show_harness_picker;
        if picker != Some(Picker::Folder) || opening_folder {
            self.projects.close_sheet();
        }
        self.show_effort_menu = false;
        self.show_permission_menu = false;
        if picker != Some(Picker::Branch) || !self.session_card.picking {
            self.card_end_picking();
        }
        if picker == Some(Picker::Model) && !self.show_model_picker {
            self.model_menu_opened();
        }
        self.show_model_picker = picker == Some(Picker::Model);
        self.state.show_branch_picker = picker == Some(Picker::Branch);
        self.show_harness_picker = picker == Some(Picker::Folder);
        self.show_effort_picker = picker == Some(Picker::Repo);
        match picker {
            Some(Picker::Model) if !self.selected_harness.is_empty() => {
                self.ensure_models(self.selected_harness.clone(), cx);
            }
            Some(Picker::Branch) => self.load_branches(cx),
            Some(Picker::Repo) => self.load_issue_repos(cx),
            _ => {}
        }
        cx.notify();
    }

    /// Toggle `picker`; opening, a searchable one starts on a fresh,
    /// focused search.
    pub(super) fn open_picker_with_search(
        &mut self,
        picker: Picker,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_picker(picker, cx);
        if picker.searchable() && self.open_picker() == Some(picker) {
            self.picker_query.clear();
            self.picker_filter_task = None;
            let placeholder = match picker {
                Picker::Model if !self.selected_harness.is_empty() => {
                    format!("Search {} models", self.harness_label())
                }
                _ => picker.search_placeholder().to_string(),
            };
            self.picker_filter.update(cx, |s, cx| {
                s.set_value("", window, cx);
                s.set_placeholder(placeholder, window, cx);
                s.focus(window, cx);
            });
        }
    }

    fn toggle_picker(&mut self, picker: Picker, cx: &mut Context<Self>) {
        if self.menu_just_dismissed(picker.menu_id()) {
            return;
        }
        let next = (self.open_picker() != Some(picker)).then_some(picker);
        self.set_picker(next, cx);
    }

    /// Apply a picker menu's search a beat after typing stops, so each
    /// keystroke doesn't reshuffle the list.
    pub(super) fn debounce_picker_query(&mut self, cx: &mut Context<Self>) {
        self.picker_filter_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PICKER_SEARCH_DELAY).await;
            let _ = this.update(cx, |this, cx| {
                let query = this.picker_filter.read(cx).value().trim().to_lowercase();
                if query != this.picker_query {
                    this.picker_query = query;
                    cx.notify();
                }
            });
        }));
    }

    /// Fetch the new-chat folder's branches. The checked-out one becomes the
    /// selection unless the user picked one; an answer for an older fetch or
    /// another folder is dropped. A folder seen before shows its cached
    /// branches right away (same frame); otherwise HEAD alone is asked for
    /// first, so the label lands before the full listing.
    fn load_branches(&mut self, cx: &mut Context<Self>) {
        let Some(space) = self
            .state
            .target_space_for_new_session()
            .filter(|s| !self.state.is_scratch_space(s))
        else {
            self.state.clear_branches_for_no_project();
            return;
        };
        let Some(client) = self.client.clone() else {
            return;
        };
        let path = space.path;
        if self.state.branch_folder.as_deref() != Some(path.as_str()) {
            if self.state.branch_folder.is_some() {
                self.state.reset_branch_for_folder();
            }
            self.state.branch_folder = Some(path.clone());
        }
        self.branch_fetch_gen += 1;
        let generation = self.branch_fetch_gen;
        let cached = self.branch_cache.get(&path).cloned();
        self.branches_loading = cached.is_none();
        if let Some(info) = cached {
            self.state
                .apply_branches(&path, info.branches, info.current, info.detached);
        } else {
            let (client, path) = (client.clone(), path.clone());
            cx.spawn(async move |this, cx| {
                let info = client.branch_head_info(&path).await;
                let _ = this.update(cx, |this, cx| {
                    // Only fills the wait: the full listing may already be in.
                    if this.branch_fetch_gen != generation || !this.state.branch_pending {
                        return;
                    }
                    if let Ok(info) = info {
                        this.state.apply_branches(
                            &path,
                            info.branches,
                            info.current,
                            info.detached,
                        );
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        cx.spawn(async move |this, cx| {
            let info = client.list_branches_info(&path).await;
            let _ = this.update(cx, |this, cx| {
                let info = match info {
                    Ok(info) => info,
                    // Not a repo (or no git): nothing to pick.
                    Err(err) => {
                        tracing::debug!("list_branches {path}: {err}");
                        keel_rpc::BranchInfo::default()
                    }
                };
                this.branch_cache.insert(path.clone(), info.clone());
                if this.branch_fetch_gen != generation {
                    return;
                }
                this.branches_loading = false;
                // Same label as the cached one: no roll (the label dedupes).
                this.state
                    .apply_branches(&path, info.branches, info.current, info.detached);
                cx.notify();
            });
        })
        .detach();
    }

    /// Warm the branch cache for folders about to be picked (the folder
    /// menu's visible rows). Each folder once; fetches run off the UI thread.
    fn prefetch_branches(&mut self, paths: Vec<String>, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        for path in paths {
            if self.branch_cache.contains_key(&path)
                || !self.branch_prefetching.insert(path.clone())
            {
                continue;
            }
            let client = client.clone();
            cx.spawn(async move |this, cx| {
                let info = client.list_branches_info(&path).await.unwrap_or_default();
                let _ = this.update(cx, |this, _| {
                    this.branch_prefetching.remove(&path);
                    this.branch_cache.entry(path).or_insert(info);
                });
            })
            .detach();
        }
    }

    /// On the hero: fetch branches once per showing and whenever the target
    /// folder changes (a folder change also drops the user's pick).
    fn ensure_branches(&mut self, cx: &mut Context<Self>) {
        let Some(space) = self
            .state
            .target_space_for_new_session()
            .filter(|s| !self.state.is_scratch_space(s))
        else {
            self.state.clear_branches_for_no_project();
            return;
        };
        let same_folder = self.state.branch_folder.as_deref() == Some(space.path.as_str());
        if same_folder && self.branches_fresh {
            return;
        }
        if !same_folder && self.state.branch_folder.is_some() {
            self.state.reset_branch_for_folder();
            self.create_error = None;
        }
        self.branches_fresh = true;
        self.load_branches(cx);
    }

    /// The composer. On the new-chat screen (`hero`) the input sits above its
    /// controls, with folder, branch and machine above the panel; in a chat it
    /// is one row: attach, input, model, send.
    pub(super) fn render_composer(
        &mut self,
        t: Tokens,
        window: &gpui::Window,
        hero: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        if hero {
            self.ensure_branches(cx);
        } else {
            self.branches_fresh = false;
        }
        let running = self.active_status() == Some(ChatStatus::Running);
        let text = self.composer.read(cx).value().to_string();
        let action = compute_button_action(running, &text);
        let open = self.open_picker();
        let fit = if hero {
            // The session layout is decided afresh from the docked panel.
            self.composer_fit_known = false;
            self.composer_panel_w.set(0.);
            ComposerFit::default()
        } else {
            self.update_composer_fit()
        };
        let _fit_p = self.composer_fit_progress(window);

        let attach = icon_chip("attach-btn", Icon::Paperclip, t).on_click(cx.listener(
            |this, _: &ClickEvent, _, cx| {
                this.set_picker(None, cx);
                this.pick_images(cx);
            },
        ));

        let model_effort = {
            let model_open = open == Some(Picker::Model);
            let model_menu = self
                .menu_phase(Picker::Model.menu_id(), model_open, cx)
                .map(|leaving| self.render_picker_menu(Picker::Model, leaving, true, t, cx));
            let model_cap = (!hero && fit.compact >= 2).then(|| f32::from(zpx(88.)));
            let model_label = self.model_roll_label("model-chip-roll", window, model_cap);

            let has_effort = self.effort_stops().len() > 1;
            let effort_open = self.show_effort_menu;
            let effort_menu = has_effort
                .then(|| {
                    self.menu_phase(EFFORT_MENU_ID, effort_open, cx)
                        .map(|leaving| self.render_effort_menu(leaving, t, cx))
                })
                .flatten();

            let model_chip_bounds = self.model_chip_bounds.clone();
            let model_chip = div()
                .relative()
                .flex_none()
                .child(
                    gpui::canvas(
                        move |bounds, _, _| {
                            model_chip_bounds.set(Some(bounds));
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .size_full(),
                )
                .children(model_menu)
                .child(
                    div()
                        .id("model-picker-btn")
                        .relative()
                        .flex_none()
                        .h(zpx(26.))
                        .px_1p5()
                        .flex()
                        .items_center()
                        .rounded(RADIUS_MD)
                        .t_small()
                        .cursor_pointer()
                        .when(model_open, |s| s.bg(c(t.selected)))
                        .hover(|s| s.text_color(c(t.text)))
                        .hover_fade(c(t.hover), None)
                        .child(
                            div()
                                .text_color(c(t.text))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(model_label),
                        )
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.open_picker_with_search(Picker::Model, window, cx);
                        })),
                );

            let effort_chip_bounds = self.effort_chip_bounds.clone();
            let effort_chip = has_effort.then(|| {
                div()
                    .relative()
                    .flex_none()
                    .child(
                        gpui::canvas(
                            move |bounds, _, _| {
                                effort_chip_bounds.set(Some(bounds));
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .children(effort_menu)
                    .child(
                        div()
                            .id("effort-picker-btn")
                            .relative()
                            .flex_none()
                            .h(zpx(26.))
                            .px_1p5()
                            .flex()
                            .items_center()
                            .rounded(RADIUS_MD)
                            .t_small()
                            .cursor_pointer()
                            .when(effort_open, |s| s.bg(c(t.selected)))
                            .hover(|s| s.text_color(c(t.text)))
                            .hover_fade(c(t.hover), None)
                            .child(
                                div()
                                    .w(zpx(EFFORT_CHIP_LABEL_W))
                                    .text_color(c(t.text_muted))
                                    .child(self.effort_roll("effort-chip-roll", false)),
                            )
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                if this.menu_just_dismissed(EFFORT_MENU_ID) {
                                    return;
                                }
                                let open = !this.show_effort_menu;
                                this.set_picker(None, cx);
                                this.show_permission_menu = false;
                                this.show_effort_menu = open;
                                cx.notify();
                            })),
                    )
            });

            div()
                .id("model-effort-chip")
                .relative()
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .gap_0p5()
                .child(model_chip)
                .when_some(effort_chip, |d, effort| {
                    d.child(
                        div()
                            .flex_none()
                            .px_0p5()
                            .text_color(c(t.text_faint))
                            .t_small()
                            .child("·"),
                    )
                    .child(effort)
                })
        };

        // Permission mode: unified chip in bottom row for both hero and docked modes.
        let permission_chip_bounds = self.permission_chip_bounds.clone();
        let mut permission_chip = (self.permission_modes().len() > 1).then(|| {
            let is_open = self.show_permission_menu;
            let menu = self
                .menu_phase(PERMISSION_MENU_ID, is_open, cx)
                .map(|leaving| self.render_permission_menu(leaving, t, cx));
            let mode = self.effective_permission();
            let planning = self.plan_on();
            let full = mode == PermissionMode::FullAccess && !planning;
            let label = if planning {
                div()
                    .flex()
                    .items_center()
                    .whitespace_nowrap()
                    .child(Icon::ListChecks.el(zpx(PERMISSION_ICON)))
                    .when(hero || fit.compact < 1, |d| {
                        d.child(div().ml(zpx(PERMISSION_GAP)).child("Plan"))
                    })
                    .into_any_element()
            } else {
                self.permission_roll_label(
                    "composer-permission-chip-roll",
                    window,
                    !hero && fit.compact >= 1,
                )
            };
            div()
                .relative()
                .flex_none()
                .child(
                    gpui::canvas(
                        move |bounds, _, _| {
                            permission_chip_bounds.set(Some(bounds));
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .size_full(),
                )
                .children(menu)
                .child(
                    chip("permission-picker-btn", t, is_open)
                        .when(full, |d| d.text_color(c(t.warning)))
                        .child(label)
                        .child(Icon::ChevronDown.el(zpx(10.)).text_color(c(if full {
                            t.warning
                        } else {
                            t.text_faint
                        })))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            if this.menu_just_dismissed(PERMISSION_MENU_ID) {
                                return;
                            }
                            let open = !this.show_permission_menu;
                            this.set_picker(None, cx);
                            this.show_effort_menu = false;
                            this.show_permission_menu = open;
                            cx.notify();
                        })),
                )
        });

        let mut scope_chip = self.project_scope_chip(t, if hero { 0 } else { fit.compact }, cx);

        // Round, light-on-dark send; a square for stop.
        let primary = {
            let has_images = !self.pending_images.is_empty();
            let enabled = self.attaching == 0
                && (action == PrimaryButtonAction::Stop
                    || !text.trim().is_empty()
                    || !self.pending_refs.is_empty()
                    || (has_images && action == PrimaryButtonAction::Send));
            let icon = if action == PrimaryButtonAction::Stop {
                Icon::Stop
            } else {
                Icon::Send
            };
            let (bg, fg, opacity) = send_look(enabled, t);
            div()
                .id("composer-primary-btn")
                .relative()
                .children(super::dev::GeoTrace::probe(&self.geo_trace, |g| &g.send))
                .size(zpx(30.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(bg)
                .text_color(fg)
                .opacity(opacity)
                .when(enabled, |s| s.cursor_pointer().hover(|s| s.opacity(0.85)))
                .child(icon.el(zpx(if icon == Icon::Stop { 10. } else { 15. })))
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.handle_primary_button(window, cx)
                }))
        };

        let suggestion = text
            .is_empty()
            .then(|| self.prompt_suggestion.clone())
            .flatten();
        let hint = t.text_faint;
        let pill = self.render_suggestion_pill(t, window, cx);
        let input = div()
            .id("composer-input")
            .relative()
            .flex_1()
            .min_w_0()
            .t_reading()
            .map(|d| match suggestion {
                Some(text) => d
                    // One line, so the box is the empty composer's height.
                    .child(div().w_full().truncate().text_color(c(hint)).child(text))
                    .child(div().absolute().inset_0().child(self.composer.clone()))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.composer
                            .update(cx, |input, cx| input.focus(window, cx));
                    })),
                None => d.child(self.composer.clone()),
            })
            .children(pill);
        let input = self.ease_input_height(input, hero, window);
        // Hero: the text starts at the top of its area, growing down as it
        // wraps (eased).
        // Picked references: cards flowing in the text's own row, ahead of the
        // input, which wraps below them once the row is full.
        let reduced = self.reduced();
        let chips: Vec<AnyElement> = self
            .pending_refs
            .clone()
            .into_iter()
            .filter(|r| !(reduced && self.refs_leaving.contains_key(&r.key())))
            .map(|item| self.ref_chip(item, t, cx))
            .collect();
        let input = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap_x(zpx(CHIP_GAP))
            .children(chips)
            .child(div().flex_1().min_w(zpx(120.)).child(input));

        let hero_panel = self.hero_panel.clone();
        let mention_menu = self.mention_menu_frame(t, cx);
        let panel = div()
            .id("composer-panel")
            .relative()
            .w_full()
            .children(super::dev::GeoTrace::probe(&self.geo_trace, |g| &g.panel))
            // While the `@` menu is open it takes the keys the input would:
            // up/down move, Enter picks (Tab on a folder opens it), Escape closes.
            .capture_action(cx.listener(|this, _: &gpui_base::input::MoveUp, _, cx| {
                if this.mention_step(false, cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &gpui_base::input::MoveDown, _, cx| {
                if this.mention_step(true, cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(
                cx.listener(|this, enter: &gpui_base::input::Enter, window, cx| {
                    if !enter.shift && this.accept_mention(None, false, window, cx) {
                        cx.stop_propagation();
                    }
                }),
            )
            .capture_action(cx.listener(|this, _: &crate::keys::ApprovePlan, window, cx| {
                if this.approve_pending_plan(window, cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(
                cx.listener(|this, _: &gpui_base::input::IndentInline, window, cx| {
                    if this.accept_mention(None, true, window, cx) {
                        cx.stop_propagation();
                    }
                }),
            )
            // Backspace at the very start takes out the command card before it,
            // as it would a word.
            .capture_action(cx.listener(|this, _: &gpui_base::input::Backspace, _, cx| {
                let at_start = {
                    let input = this.composer.read(cx);
                    input.cursor() == 0 && input.selected_range().is_empty()
                };
                let command = this
                    .live_refs()
                    .find(|r| matches!(r, super::MentionItem::Command(_)))
                    .cloned();
                if let (true, Some(command)) = (at_start, command) {
                    this.remove_ref(command, cx);
                    cx.stop_propagation();
                }
            }))
            // The field's ⌘V runs before any key listener: an image on the
            // clipboard attaches here; text goes on to the field.
            .capture_action(cx.listener(|this, _: &gpui_base::input::Paste, _, cx| {
                if this.paste_images(cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &gpui_base::input::Escape, _, cx| {
                if this.mention.take().is_some() {
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .children(mention_menu)
            .rounded(zpx(18.))
            .bg(c(t.floating))
            .border_1()
            .border_color(c(t.border))
            .map(|d| {
                // Multi-tier ambient shadow (tight contact rim, mid elevation, and wide soft diffusion)
                // lifting the stone card off the background smoothly without pops during transitions.
                let shadow = c(t.shadow);
                let layer = |y: f32, blur: f32, spread: f32, alpha: f32| gpui::BoxShadow {
                    color: gpui::Hsla {
                        a: shadow.a * alpha,
                        ..shadow
                    },
                    offset: gpui::point(zpx(0.), zpx(y)),
                    blur_radius: zpx(blur),
                    spread_radius: zpx(spread),
                    inset: false,
                };
                d.shadow(vec![
                    layer(1., 3., 0., 0.6),
                    layer(6., 18., -1., 0.4),
                    layer(20., 48., -3., 0.3),
                ])
            })
            .flex()
            .flex_col()
            // Drop images anywhere on the composer.
            .drag_over::<gpui::ExternalPaths>(move |style, _, _, _| style.border_color(c(t.accent)))
            .on_drop(cx.listener(|this, paths: &gpui::ExternalPaths, _, cx| {
                this.attach_paths(paths.paths().to_vec(), cx);
            }))
            // Where the hero panel sits, so the drop to the dock starts from it.
            .when(hero, |d| {
                d.child(
                    gpui::canvas(
                        move |bounds, _, _| hero_panel.set(Some(bounds)),
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
                )
            })
            .when(!hero, |d| {
                let panel_w = self.composer_panel_w.clone();
                d.child(
                    gpui::canvas(
                        // Measured every frame, slides included: mid-slide the
                        // dock already has its landing width (`column_slide`),
                        // so this is the width the settled frame decides from.
                        // (Skipping slides left a stale width that flipped the
                        // layout and back just after landing.)
                        move |bounds, window, _| {
                            let w = f32::from(bounds.size.width);
                            if (panel_w.replace(w) - w).abs() >= 0.5 {
                                window.request_animation_frame();
                            }
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
                )
            })
            .children(self.render_pending_images(t, hero, cx))
            .map(|d| {
                if hero {
                    // One inset all round: the text, the paperclip's glyph and
                    // the send button's edge sit HERO_INSET in from the sides;
                    // the same padding above the text and below the controls.
                    d.px(zpx(HERO_INSET))
                        .pt(zpx(HERO_PAD_Y))
                        .pb(zpx(HERO_PAD_Y))
                        .gap(zpx(HERO_GAP))
                        .child(input)
                        .child(
                            div()
                                .w_full()
                                .min_w_0()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_1()
                                // Its 30 px hit area pads the 16 px glyph by 7 px:
                                // pulled out so the glyph, not the box, lines up.
                                .child(attach.ml(zpx(-ATTACH_GLYPH_PAD)))
                                .children(permission_chip.take())
                                .children(scope_chip.take())
                                .child(div().flex_1())
                                .child(model_effort)
                                .child(primary),
                        )
                } else {
                    let ctrl_w = self.composer_ctrl_w.clone();
                    let level = fit.compact as usize;
                    let cluster = div()
                        .relative()
                        .flex_none()
                        .when(level >= 2, |d| d.flex_shrink(1.).min_w_0())
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .child(model_effort)
                        .child(primary)
                        .child(
                            gpui::canvas(
                                move |bounds, window, _| {
                                    let w = f32::from(bounds.size.width);
                                    let mut widths = ctrl_w.borrow_mut();
                                    if (widths[level] - w).abs() >= 0.5 {
                                        widths[level] = w;
                                        window.request_animation_frame();
                                    }
                                },
                                |_, _, _, _| {},
                            )
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full(),
                        );
                    let text_box = div()
                        .w_full()
                        .px_1()
                        .min_h(zpx(32.))
                        .flex()
                        .items_center()
                        .child(div().flex_1().min_w_0().child(input));
                    let bottom_bar = div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .child(attach)
                        .children(permission_chip.take())
                        .children(scope_chip.take())
                        .child(div().flex_1())
                        .child(cluster);

                    d.px_3().pt_2p5().pb_2().gap_2().child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(text_box)
                            .child(div().h(zpx(1.)).w_full().bg(c(t.border)).opacity(0.35))
                            .child(bottom_bar),
                    )
                }
            });

        // Leaving the hero, the docked panel starts at the hero panel's height
        // and shrinks to its own settled height, controls pinned to the bottom.
        // It grows up past its slot (negative margin), so the slot and the
        // transcript above never reflow; the folder tab rides its top edge out.
        let reduced = self.reduced();
        let leaving = (!hero)
            .then(|| self.hero_exit.is_some().then_some(self.hero_from).flatten())
            .flatten();
        let panel = match leaving.filter(|_| !reduced) {
            Some(from) => {
                let natural = f32::from(zpx(32.)) + f32::from(zpx(30.)) + 28.;
                let extra = (f32::from(from.size.height) - natural).max(0.);
                let elapsed = std::time::Duration::from_secs_f32(
                    self.hero_exit_t() * crate::motion::SPATIAL.as_secs_f32(),
                );
                let e = morph_extra(extra, elapsed, crate::motion::SPATIAL);
                window.request_animation_frame();
                let tab = self.render_hero_targets(None, window, t, cx);
                panel
                    .child(crate::motion::vanish(
                        div().absolute().inset_0().child(tab),
                        ("hero-tab-out", self.hero_gen as usize),
                        reduced,
                    ))
                    .when(e > 0., |el| {
                        el.justify_end().min_h(px(natural + e)).mt(px(-e))
                    })
                    .into_any_element()
            }
            None => panel.into_any_element(),
        };

        // The context meter that hung past the docked panel's right edge
        // now lives in the session card (see `session_card`).
        // Docked, the panel spans the reading column's text (the column less
        // the rows' own px_4), so its edges sit on the transcript's.
        let max = if hero {
            HERO_COMPOSER_WIDTH
        } else {
            self.column_max() - zpx(32.)
        };
        if !hero {
            self.composer_max.set(f32::from(max));
        }
        let width = div().w_full().max_w(max);
        // Mid-drop this box is squeezed by the drop's widening; the drop
        // measures the real slot instead (else the panel landed narrow, then jumped).
        let dropping = !hero && self.hero_exiting();
        let record = self.composer_slot.clone();
        // On the hero, folder / branch / machine ride in a tab on the panel's
        // top edge. Drawn after the panel so it covers that stretch of border.
        let tab = hero.then(|| self.render_hero_targets(open, window, t, cx));
        div()
            .flex_none()
            .w_full()
            .px_4() // Room between the last message and the docked input.
            // Symmetric, so the hero drop can land on the slot's center.
            .when(!hero, |d| d.pb(zpx(16.)).pt(zpx(16.)))
            .flex()
            .justify_center()
            .child(
                width.child(
                    div()
                        .relative()
                        .w_full()
                        .when(hero, |d| d.mt(zpx(TAB_HEIGHT)))
                        .child(panel)
                        .children(tab)
                        .children((!hero).then(|| self.render_points(t, cx)))
                        .children(self.render_create_error(t, cx)),
                ),
            )
            // The slot's inner width (inside px_4), for next frame's room check.
            .when(!dropping, |d| {
                d.child(
                    gpui::canvas(
                        move |bounds, window, _| record_slot(&record, bounds, window),
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .inset_0(),
                )
            })
    }

    /// The shortcut pill on the suggestion's first line, top-right of the
    /// text area. Opaque, so a long suggestion runs under it rather than
    /// reflowing. Rises in when a suggestion arrives; sinks out when the
    /// suggestion goes away or is taken by a click. Keyboard changes (typing,
    /// the shortcut itself) remove it at once.
    fn render_suggestion_pill(
        &mut self,
        t: Tokens,
        window: &gpui::Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let visible = self.can_accept_suggestion(cx);
        let exit = std::mem::take(&mut self.suggestion_pill_clicked)
            || self.composer.read(cx).value().is_empty();
        let reduced = self.reduced();
        let (opacity, dy) = self.suggestion_pill.step(
            visible,
            self.prompt_suggestion_at,
            exit,
            reduced,
            std::time::Instant::now(),
        )?;
        if self.suggestion_pill.animating() {
            window.request_animation_frame();
        }
        let shortcut = if cfg!(target_os = "macos") {
            "⌘⇧Space"
        } else {
            "Ctrl+Shift+Space"
        };
        let pill = div()
            .id("prompt-suggestion-pill")
            .relative()
            .top(zpx(dy))
            .flex_none()
            .flex()
            .items_center()
            .h(zpx(SUGGESTION_PILL_H))
            .px(zpx(6.))
            .rounded_full()
            .bg(c(mix_rgba(t.floating, t.text, PILL_TINT)))
            .t_caption()
            .text_color(c(t.text_muted))
            .opacity(opacity)
            .when(visible, |d| {
                d.cursor_pointer()
                    .hover(|s| s.text_color(c(t.text)))
                    .hover_fade(c(t.hover), None)
                    .tooltip(move |_, cx| cx.new(|_| super::Tip::new("Use suggestion", t)).into())
                    .tooltip_show_delay(crate::view::tooltip::delay())
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        this.suggestion_pill_clicked = true;
                        this.accept_suggestion(window, cx)
                    }))
            })
            .child(shortcut);
        Some(
            div()
                .absolute()
                .top_0()
                .right_0()
                .h(zpx(crate::style::READING.line))
                .flex()
                .items_center()
                .child(pill)
                .into_any_element(),
        )
    }

    /// The session's points. Sits on the
    /// composer's top-right; hidden on the hero, which has no transcript.
    fn render_points(&mut self, t: Tokens, cx: &mut Context<Self>) -> AnyElement {
        let open = self.show_points_menu;
        let points = self.transcript.user_points();
        let menu = self.menu_phase("points-menu", open, cx).map(|leaving| {
            let rows: Vec<AnyElement> = points
                .iter()
                .rev()
                .map(|(index, text)| {
                    let index = *index;
                    // The shared menu row (no check column: these rows are
                    // jumps, not settings), in secondary text that rises to
                    // primary on hover, one line.
                    menu_item(format!("point-{index}"), t, true, false)
                        .text_color(c(t.text_muted))
                        .child(div().flex_1().min_w_0().truncate().child(text.clone()))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.show_points_menu = false;
                            this.transcript.scroll_to_row(index);
                            cx.notify();
                        }))
                        .into_any_element()
                })
                .collect();
            let rows = if rows.is_empty() {
                vec![menu_label("No messages yet", t).into_any_element()]
            } else {
                rows
            };
            // Right edges line up with the pill's, so the menu opens over the
            // composer instead of hanging off past it.
            let dismiss = cx.listener(|this, _, _, cx| {
                this.show_points_menu = false;
                this.note_menu_dismissed("points-menu");
                cx.notify();
            });
            // A clear gap above the pill so the menu reads as its own layer.
            let panel = super::menu_panel("points-menu", t, POINTS_MENU_W, true, rows)
                .mb(zpx(POINTS_MENU_GAP))
                .when(leaving.is_none(), |d| d.on_mouse_down_out(dismiss));
            gpui::deferred(
                gpui::anchored()
                    .anchor(gpui::Anchor::BottomRight)
                    .snap_to_window_with_margin(zpx(8.))
                    .child(super::menu_motion(
                        panel,
                        "points-menu",
                        true,
                        leaving,
                        self.reduced(),
                    )),
            )
            .with_priority(1)
            .into_any_element()
        });
        div()
            .absolute()
            .top(zpx(-POINTS_PILL_H - 6.))
            .right_0()
            .flex()
            .flex_row()
            .items_center()
            .px(zpx(POINTS_PILL_PAD))
            .h(zpx(POINTS_PILL_H))
            .floating_surface(t)
            // After the surface, which sets its own radius: the input's
            // softness, not a capsule.
            .rounded(RADIUS_XL)
            // Out past the pill's 1px border, so the menu's right edge sits
            // flush with the pill's outer edge.
            .child(div().absolute().top(px(-1.)).right(px(-1.)).children(menu))
            .children(super::dev::GeoTrace::probe(&self.geo_trace, |g| &g.points))
            .child(div().child(
                points_chip("points-menu-btn", Icon::List, open, t).on_click(cx.listener(
                    |this, _: &ClickEvent, _, cx| {
                        if this.menu_just_dismissed("points-menu") {
                            return;
                        }
                        this.show_points_menu = !this.show_points_menu;
                        cx.notify();
                    },
                )),
            ))
            .into_any_element()
    }

    /// The `@` / `/` menu for this frame. The menu closes from many places
    /// (typing, picking, Escape, a click away, a send), so the change is read
    /// here against the last frame: opened (or switched between `@` and `/`)
    /// replays the rows' cascade; closed leaves a snapshot fading out.
    fn mention_menu_frame(&mut self, t: Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let last = self.mention_shown.take();
        let now = self.mention.clone();
        match (&last, &now) {
            (prev, Some(m)) if prev.as_ref().is_none_or(|p| p.slash != m.slash) => {
                self.mention_gen += 1;
                self.mention_leaving = None;
            }
            (Some(prev), None) => {
                self.mention_leave_gen += 1;
                let n = self.mention_leave_gen;
                self.mention_leaving = Some((n, prev.clone()));
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(crate::motion::FAST + std::time::Duration::from_millis(20))
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        if this.mention_leaving.as_ref().is_some_and(|(m, _)| *m == n) {
                            this.mention_leaving = None;
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
            _ => {}
        }
        self.mention_shown = now.clone();
        match now {
            Some(m) => Some(self.render_mention_menu(m, None, t, cx)),
            None => self
                .mention_leaving
                .clone()
                .map(|(n, m)| self.render_mention_menu(m, Some(n), t, cx)),
        }
    }

    /// The `@` / `/` menu, floating above the composer. `@`: the picked
    /// GitLab repo's open issues (when one is picked), then the folder's files
    /// and folders. `/`: the harness's commands and skills. Click or
    /// Enter/Tab takes one. Rows cascade in when it opens, skeletons stand in
    /// while a list loads, and `leaving` draws its fade out after a close.
    fn render_mention_menu(
        &mut self,
        mention: super::Mention,
        leaving: Option<u64>,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let reduced = self.reduced();
        let root = self.mention_root();
        let in_project = self
            .projects
            .mention_label(self.current_chat.as_deref(), root.as_deref())
            .is_some();
        let loading = if in_project {
            self.projects.mention_loading(
                self.current_chat.as_deref(),
                root.as_deref(),
                &self.folder_paths,
                &self.folder_paths_loading,
            )
        } else {
            root.as_ref()
                .is_some_and(|r| !self.folder_paths.contains_key(r))
        };
        let has_files = in_project || root.is_some();
        let matches = self.mention_matches_for(&mention);
        let row = |id: String, active: bool| {
            div()
                .id(gpui::ElementId::Name(id.into()))
                .h(zpx(28.))
                .px_2()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .rounded(RADIUS_MD)
                .cursor_pointer()
                .t_body()
                .when(active, |d| d.bg(c(t.selected)))
                .when(!active, |d| d.hover_fade(c(t.hover), None))
        };
        let faint = |text: String| {
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .t_small()
                .text_color(c(t.text_faint))
                .child(text)
        };
        let (mut issue_rows, mut file_rows, mut command_rows): (
            Vec<AnyElement>,
            Vec<AnyElement>,
            Vec<AnyElement>,
        ) = (Vec::new(), Vec::new(), Vec::new());
        for (ix, item) in matches.into_iter().enumerate() {
            let active = ix == mention.active;
            let pick = item.clone();
            let accept = cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.accept_mention(Some(pick.clone()), false, window, cx);
            });
            let cascade_in = |r: gpui::Stateful<gpui::Div>| r.into_any_element();
            match item {
                super::MentionItem::Command(command) => {
                    command_rows.push(cascade_in(
                        row(format!("mention-cmd-{}", command.insert), active)
                            .child(div().flex_none().text_color(c(t.text)).child(
                                if command.skill {
                                    command
                                        .insert
                                        .strip_prefix("/skill:")
                                        .map(|n| format!("/{n}"))
                                        .unwrap_or_else(|| command.insert.clone())
                                } else {
                                    command.insert.clone()
                                },
                            ))
                            .when(command.project, |d| {
                                d.child(
                                    div()
                                        .flex_none()
                                        .px_1()
                                        .rounded(zpx(3.))
                                        .bg(c(t.surface))
                                        .t_caption()
                                        .text_color(c(t.text_faint))
                                        .child("project"),
                                )
                            })
                            .child(faint(command.description))
                            .on_click(accept),
                    ))
                }
                super::MentionItem::Issue(issue) => issue_rows.push(cascade_in(
                    row(format!("mention-issue-{}", issue.iid), active)
                        .child(
                            div()
                                .flex_none()
                                .text_color(c(t.success))
                                .child(Icon::Issue.el(zpx(13.))),
                        )
                        .child(
                            div()
                                .flex_none()
                                .t_small()
                                .text_color(c(t.text_faint))
                                .child(format!("#{}", issue.iid)),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_color(c(t.text))
                                .child(issue.title),
                        )
                        .on_click(accept),
                )),
                super::MentionItem::Path(path) => {
                    let folder = path.ends_with('/');
                    let trimmed = path.trim_end_matches('/');
                    let (dir, name) = match trimmed.rsplit_once('/') {
                        Some((dir, name)) => (format!("{dir}/"), name.to_string()),
                        None => (String::new(), trimmed.to_string()),
                    };
                    file_rows.push(cascade_in(
                        row(format!("mention-{path}"), active)
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(c(if folder { t.accent } else { t.text_faint }))
                                    .child(Icon::for_path(&path).el(zpx(13.))),
                            )
                            .child(div().flex_none().text_color(c(t.text)).child(if folder {
                                format!("{name}/")
                            } else {
                                name
                            }))
                            .child(faint(dir))
                            .on_click(accept),
                    ));
                }
            }
        }
        let mut items: Vec<AnyElement> = Vec::new();
        let width = if mention.slash {
            // `/`: the harness's commands and skills, nothing else.
            let harness = self.slash_harness();
            let name = self
                .harnesses
                .iter()
                .find(|h| h.id == harness)
                .map(|h| h.display_name.clone())
                .unwrap_or(harness);
            items.push(menu_label(format!("Commands & skills · {name}"), t).into_any_element());
            if !self.commands.contains_key(&self.commands_key()) {
                items.extend(skeleton_rows("mention-cmd-skel", 4, t, reduced));
            } else if command_rows.is_empty() {
                items.push(
                    menu_label(
                        if mention.query.is_empty() {
                            "None found for this harness"
                        } else {
                            "No matches"
                        },
                        t,
                    )
                    .into_any_element(),
                );
            }
            items.extend(command_rows);
            420.
        } else {
            // Issues first, only with a repo picked; files as always below.
            if let Some((_, project)) = &self.issue_repo {
                items.push(menu_label(format!("Issues · {}", project.name), t).into_any_element());
                if self.repo_issues.is_none() {
                    items.extend(skeleton_rows("mention-issue-skel", 2, t, reduced));
                } else if issue_rows.is_empty() {
                    items.push(menu_label("No matching open issues", t).into_any_element());
                }
                items.extend(issue_rows);
                items.push(menu_separator(t).into_any_element());
            }
            let file_label = self
                .projects
                .mention_label(self.current_chat.as_deref(), root.as_deref())
                .unwrap_or_else(|| {
                    if root.is_none() {
                        "Pick a folder to reference its files".into()
                    } else {
                        "Reference a file".into()
                    }
                });
            items.push(menu_label(file_label, t).into_any_element());
            if has_files && loading {
                items.extend(skeleton_rows("mention-file-skel", 4, t, reduced));
            } else if has_files && file_rows.is_empty() {
                items.push(menu_label("No matches", t).into_any_element());
            }
            items.extend(file_rows);
            380.
        };
        let dismiss = cx.listener(|this, _: &gpui::MouseDownEvent, _, cx| {
            this.mention = None;
            cx.notify();
        });
        floating_menu_at(
            "mention-menu",
            t,
            width,
            true,
            leaving,
            reduced,
            dismiss,
            items,
        )
    }

    /// Why the new chat could not be created, under the composer, until
    /// dismissed or the next attempt.
    fn render_create_error(&self, t: Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let message = self.create_error.clone()?;
        Some(
            div()
                .id("create-chat-error")
                .mt_2()
                .w_full()
                .px_3()
                .py_1p5()
                .rounded(crate::style::RADIUS_MD)
                .bg(c(t.danger & 0xFFFF_FF00 | 0x1A))
                .border_1()
                .border_color(c(t.danger & 0xFFFF_FF00 | 0x66))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .t_small()
                .text_color(c(t.text))
                .child(
                    div()
                        .flex_none()
                        .text_color(c(t.danger))
                        .child(Icon::Alert.el(zpx(13.))),
                )
                .child(div().flex_1().min_w_0().child(message))
                .child(
                    div()
                        .id("create-chat-error-dismiss")
                        .flex_none()
                        .cursor_pointer()
                        .text_color(c(t.text_faint))
                        .hover(|s| s.text_color(c(t.text)))
                        .child("Dismiss")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.create_error = None;
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }

    /// Where the new chat will run (folder, branch) and, optionally, the
    /// GitLab repo its issue lives in. Right-aligned above the hero composer.
    fn render_hero_targets(
        &mut self,
        open: Option<Picker>,
        window: &gpui::Window,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (space_name, space_path) = self
            .state
            .target_space_for_new_session()
            .map(|s| (s.name.clone(), s.path.clone()))
            .unwrap_or_else(|| {
                if self.state.new_chat_no_project {
                    ("No project".into(), String::new())
                } else {
                    ("Choose folder".into(), String::new())
                }
            });
        let space = self.projects.folder_chip(
            (!space_path.is_empty()).then_some(space_path.as_str()),
            space_name,
        );
        // While a new folder's branches load, the previous label holds
        // (dimmed), so a folder change is one roll: old branch → new branch.
        let branch_label = self.state.branch_chip_label().unwrap_or_else(|| {
            if self.branch_label.now.is_empty() {
                "Branch".into()
            } else {
                self.branch_label.now.clone()
            }
        });
        let branch_label = self.branch_chip_label(branch_label, window);
        let repo = self
            .issue_repo
            .as_ref()
            .map(|(_, p)| p.name.clone())
            .unwrap_or_else(|| "Issue repo".into());
        let divider = || div().w(zpx(1.)).h(zpx(14.)).flex_none().bg(c(t.border));
        div()
            .absolute()
            .bottom(gpui::relative(1.))
            .right(zpx(18.))
            // One pixel into the panel, over its top border, so they read as one piece.
            .mb(zpx(-1.))
            .h(zpx(TAB_HEIGHT + 1.))
            .px_1()
            .pb(zpx(1.))
            .flex()
            .flex_row()
            .items_center()
            .gap_0p5()
            .rounded_t(zpx(12.))
            .bg(c(t.floating))
            .border_1()
            .border_b_0()
            .border_color(c(t.border))
            .child(self.picker_chip(
                Picker::Folder,
                "folder-picker-btn",
                Some(Icon::Folder),
                space.into_any_element(),
                180.,
                open,
                t,
                cx,
            ))
            .child(divider())
            .child(self.picker_chip(
                Picker::Branch,
                "branch-picker-btn",
                Some(Icon::Branch),
                branch_label,
                BRANCH_LABEL_MAX,
                open,
                t,
                cx,
            ))
            .child(divider())
            .child(self.picker_chip(
                Picker::Repo,
                "repo-picker-btn",
                Some(Icon::GitLab),
                repo.into_any_element(),
                180.,
                open,
                t,
                cx,
            ))
            .into_any_element()
    }

    /// One picked reference as a card: a document icon and `#33 Title` for
    /// an issue, the kind's icon and name for a file, folder, command or
    /// skill. It fades in; its × folds it away in place (fading as its width
    /// closes, so what follows slides over).
    fn ref_chip(&self, item: super::MentionItem, t: Tokens, cx: &mut Context<Self>) -> AnyElement {
        use gpui::AnimationExt as _;
        let reduced = self.reduced();
        let leaving = &self.refs_leaving;
        let widths = self.chip_widths.clone();
        let key = item.key();
        let (icon, tint, tag, label) = match &item {
            super::MentionItem::Command(cmd) => (
                if cmd.skill { Icon::Brain } else { Icon::Tool },
                t.text_muted,
                None,
                cmd.name.clone(),
            ),
            super::MentionItem::Issue(i) => (
                Icon::FileText,
                t.success,
                Some(format!("#{}", i.iid)),
                i.title.clone(),
            ),
            super::MentionItem::Path(path) => {
                let name = path
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or(path);
                let folder = path.ends_with('/');
                (
                    Icon::for_path(path),
                    if folder { t.accent } else { t.text_muted },
                    None,
                    if folder {
                        format!("{name}/")
                    } else {
                        name.to_string()
                    },
                )
            }
        };
        let exit = leaving.get(&key).copied();
        let width = widths.borrow().get(&key).copied();
        let remove = item;
        let chip = div()
            .id(gpui::ElementId::Name(format!("ref-{key}").into()))
            .relative()
            .flex_none()
            .h(zpx(22.))
            .max_w(zpx(320.))
            // Folding: hold the width it had so the contents don't reflow.
            .when_some(width.filter(|_| exit.is_some()), |d, w| d.w(px(w)))
            .pl_2()
            .pr_1()
            .flex()
            .flex_row()
            .items_center()
            .gap_1p5()
            .rounded(RADIUS_MD)
            .bg(c(t.surface))
            .border_1()
            .border_color(c(t.border))
            .t_small()
            .child(
                div()
                    .flex_none()
                    .text_color(c(tint))
                    .child(icon.el(zpx(13.))),
            )
            .children(tag.map(|tag| div().flex_none().text_color(c(t.text_faint)).child(tag)))
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_color(c(t.text))
                    .child(label),
            )
            .child(
                div()
                    .id(gpui::ElementId::Name(format!("ref-remove-{key}").into()))
                    .flex_none()
                    .size(zpx(18.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(zpx(4.))
                    .cursor_pointer()
                    .text_color(c(t.text_faint))
                    .hover(|d| d.text_color(c(t.text)))
                    .hover_fade(c(t.hover), None)
                    .child(Icon::Close.el(zpx(11.)))
                    .when(exit.is_none(), |d| {
                        d.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.remove_ref(remove.clone(), cx)
                        }))
                    }),
            )
            // Its painted width, for the fold when it leaves.
            .when(exit.is_none(), |d| {
                let (widths, key) = (widths.clone(), key.clone());
                d.child(
                    gpui::canvas(
                        move |bounds, _, _| {
                            widths
                                .borrow_mut()
                                .insert(key.clone(), f32::from(bounds.size.width));
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .inset_0(),
                )
            });
        match exit {
            None => crate::motion::appear(
                chip,
                gpui::ElementId::Name(format!("ref-in-{key}").into()),
                crate::motion::BASE,
                4.,
                reduced,
            )
            .into_any_element(),
            Some(n) => {
                let full = width.unwrap_or(160.);
                div()
                    .flex_none()
                    .overflow_hidden()
                    .child(chip)
                    .with_animation(
                        gpui::ElementId::Name(format!("ref-out-{key}-{n}").into()),
                        gpui::Animation::new(crate::motion::SLOW)
                            .with_easing(crate::motion::ease_out),
                        move |el, d| {
                            // Gone from sight in the first half; the space closes
                            // over the whole run, taking the gap after it too.
                            el.opacity((1. - d * 1.8).max(0.))
                                .w(px(full * (1. - d)))
                                .mr(zpx(-CHIP_GAP * d))
                        },
                    )
                    .into_any_element()
            }
        }
    }

    /// Thumbnails of images waiting to be sent, with remove buttons. The row
    /// opens with the first image (or its spinner) and closes with the last,
    /// like the chips row, the last thumbnails staying put while it closes.
    fn render_pending_images(
        &mut self,
        t: Tokens,
        hero: bool,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        use gpui::AnimationExt as _;
        let reduced = self.reduced();
        let closing = self.pending_images.is_empty() && self.attaching == 0;
        // Shown in place: what's attached, plus any thumbnail folding away
        // (`Some(exit number)`) back where it sat.
        let mut images: Vec<_> = if closing {
            &self.images_leaving
        } else {
            &self.pending_images
        }
        .iter()
        .map(|i| (i, None))
        .collect();
        if !closing {
            for (ix, image, n) in &self.images_folding {
                images.insert((*ix).min(images.len()), (image, Some(*n)));
            }
        }
        if closing && (images.is_empty() || reduced) {
            return None;
        }
        let row = div()
            .px_1()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap_1p5()
            .children(images.into_iter().map(|(image, exit)| {
                let id = image.attachment.id.clone();
                let path = image.path.clone();
                let group = format!("pending-{}", &id[..12]);
                let tile = div()
                    .id(gpui::ElementId::Name(group.clone().into()))
                    .hover_probe(group.clone())
                    .group(group.clone())
                    .relative()
                    .size(zpx(56.))
                    .rounded(RADIUS_MD)
                    .overflow_hidden()
                    .border_1()
                    .border_color(c(t.border))
                    .cursor_pointer()
                    .when(exit.is_none(), |d| {
                        d.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.open_lightbox(super::Lightbox::Image(path.clone()), cx)
                        }))
                    })
                    .child(
                        gpui::img(image.path.clone())
                            .size_full()
                            .object_fit(gpui::ObjectFit::Cover),
                    )
                    .child(reveal_on_hover(
                        "remove-reveal",
                        group.clone(),
                        0.,
                        1.,
                        div()
                            .id(gpui::ElementId::Name(format!("remove-{group}").into()))
                            .absolute()
                            .top(zpx(3.))
                            .right(zpx(3.))
                            .size(zpx(18.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .bg(gpui::rgba(0x000000A6))
                            .text_color(gpui::rgb(0xFFFFFF))
                            .cursor_pointer()
                            .child(Icon::Close.el(zpx(10.)))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                // Not also the tile's open.
                                cx.stop_propagation();
                                this.remove_image(&id, cx)
                            })),
                    ));
                match exit {
                    None => tile.into_any_element(),
                    // Gone from sight in the first half; the space closes over
                    // the whole run, taking the gap after it too.
                    Some(n) => div()
                        .flex_none()
                        .overflow_hidden()
                        .child(tile)
                        .with_animation(
                            ("image-out", n as usize),
                            gpui::Animation::new(crate::motion::SLOW)
                                .with_easing(crate::motion::ease_out),
                            move |el, d| {
                                el.opacity((1. - d * 1.8).max(0.))
                                    .w(zpx(56. * (1. - d)))
                                    .mr(zpx(-6. * d))
                            },
                        )
                        .into_any_element(),
                }
            }))
            .when(self.attaching > 0, |d| {
                d.child(
                    div()
                        .size(zpx(56.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(RADIUS_MD)
                        .border_1()
                        .border_color(c(t.border))
                        .text_color(c(t.text_faint))
                        .child(crate::motion::spin(
                            Icon::Spinner.el(zpx(16.)),
                            "attach-spinner",
                            reduced,
                        )),
                )
            });
        let sized = move |el, p| reveal_row(el, p, 56. + 4., hero);
        let row = if reduced {
            row.pt_1().into_any_element()
        } else if closing {
            row.with_animation(
                ("images-row-out", self.images_gen as usize),
                gpui::Animation::new(crate::motion::SLOW).with_easing(crate::motion::ease_out),
                move |el, d| sized(el, 1. - d),
            )
            .into_any_element()
        } else {
            row.with_animation(
                ("images-row-in", self.images_row_gen as usize),
                gpui::Animation::new(crate::motion::BASE).with_easing(crate::motion::ease_out),
                move |el, d| if d >= 1. { el.pt_1() } else { sized(el, d) },
            )
            .into_any_element()
        };
        Some(row)
    }

    /// A chip that opens one picker; its menu floats above it.
    #[allow(clippy::too_many_arguments)]
    fn picker_chip(
        &mut self,
        picker: Picker,
        id: &'static str,
        icon: Option<Icon>,
        label: AnyElement,
        // The label's widest; past it, it ends in an ellipsis.
        max_label: f32,
        open: Option<Picker>,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_open = open == Some(picker);
        let menu = self
            .menu_phase(picker.menu_id(), is_open, cx)
            .map(|leaving| self.render_picker_menu(picker, leaving, true, t, cx));
        div().relative().children(menu).child(
            chip(id, t, is_open)
                .when_some(icon, |d, icon| d.child(icon.el(zpx(13.))))
                .child(
                    div()
                        .max_w(zpx(max_label))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(label),
                )
                .child(
                    div()
                        .text_color(c(t.text_faint))
                        .child(Icon::ChevronDown.el(zpx(12.))),
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.open_picker_with_search(picker, window, cx)
                })),
        )
    }

    /// The input in a box whose height eases to the text's as lines come and
    /// go (typing, a paste, a send clearing it), retargeting mid-move. The
    /// text keeps its own height inside, pinned to the top: lines already
    /// there hold still while new ones are revealed below (pinned to the
    /// bottom, a new line pushed them up at once, then they slid back down).
    /// Measured a frame late; a hero ↔ dock switch snaps (the drop has its
    /// own morph), as does reduced motion.
    fn ease_input_height(
        &mut self,
        input: gpui::Stateful<gpui::Div>,
        hero: bool,
        window: &gpui::Window,
    ) -> gpui::Div {
        let now = std::time::Instant::now();
        let natural = self.composer_natural.clone();
        if self.composer_h_hero != hero {
            self.composer_h_hero = hero;
            self.composer_h = None;
            self.composer_line_h = 0.;
            natural.set(0.);
        }
        let measured = natural.get();
        if measured > 0. && (self.composer_line_h == 0. || measured < self.composer_line_h) {
            self.composer_line_h = measured;
        }
        let reduced = self.reduced();
        if measured > 0. {
            match &mut self.composer_h {
                None => self.composer_h = Some(crate::motion::WidthTween::settled(measured)),
                Some(h) => {
                    h.retarget(measured, now, !reduced);
                }
            }
        }
        let measure = gpui::canvas(
            move |bounds, window, _| {
                let h = f32::from(bounds.size.height);
                if (natural.replace(h) - h).abs() >= 0.5 {
                    window.request_animation_frame();
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0();
        let input = input.flex_none().child(measure);
        // Never taller than the editor's own most rows: a long suggestion (a
        // plain text layer under the empty editor) once grew past it.
        let cap = self.composer_line_h * COMPOSER_MAX_ROWS;
        let cap = if cap > 0. { cap } else { f32::INFINITY };
        match self.composer_h {
            // Held at the eased height even when settled: new text is measured
            // a frame late, and an unheld box showed it at full height for that
            // frame, then snapped back to ease open (the jiggle).
            Some(h) => {
                if h.animating(now) {
                    window.request_animation_frame();
                }
                div()
                    .h(px(h.value_at(now).min(cap)))
                    .overflow_hidden()
                    .child(input)
            }
            None => div()
                .when(cap > 0., |d| d.max_h(px(cap)).overflow_hidden())
                .child(input),
        }
    }

    /// The branch chip's label, sized to its text (up to [`BRANCH_LABEL_MAX`],
    /// then an ellipsis). When the label changes its box eases from the old
    /// width to the new one (so the content-sized tab resizes smoothly) while
    /// the old label rolls out and the new one rolls in, like the model chip.
    /// Rapid changes retarget from the current width; reduced motion snaps.
    fn branch_chip_label(&mut self, label: String, window: &gpui::Window) -> AnyElement {
        use gpui::AnimationExt as _;
        let now = std::time::Instant::now();
        let reduced = self.reduced();
        let target = measure_small_label(window, &label).min(f32::from(zpx(BRANCH_LABEL_MAX)));
        let changed = label != self.branch_label.now;
        match &mut self.branch_chip_w {
            // First render: rest at the label's width, no growing from 0.
            None => self.branch_chip_w = Some(crate::motion::WidthTween::settled(target)),
            // Same text re-measured (zoom, font load): snap, don't animate.
            Some(w) => {
                w.retarget(target, now, changed && !reduced);
            }
        }
        if self.branch_label.commit(&label, now, !reduced) {
            self.branch_prev_w = self.branch_label_w;
        }
        self.branch_label_w = target;
        let width = self.branch_chip_w.expect("set above");

        let roll = self.branch_label.roll as usize;
        let rolling = roll > 0 && !reduced;
        let anim =
            || gpui::Animation::new(crate::motion::SLOW).with_easing(crate::motion::ease_out);
        // The width eases on an empty spacer; the labels are absolute
        // siblings over it. They must not be children of the width
        // animation: gpui keys an animation's children under its id, so a
        // new width generation, or the wrapper going away when the tween
        // ends, would reset the roll and play it again.
        let spacer = div().h(zpx(ROLL_H));
        let spacer = if width.animating(now) && !reduced {
            let (from, to) = (width.from, width.to);
            spacer
                .with_animation(
                    ("branch-chip-w", width.generation as usize),
                    anim(),
                    move |el, d| el.w(px(from + (to - from) * d)),
                )
                .into_any_element()
        } else {
            spacer.w(px(width.to)).into_any_element()
        };
        let line = |text: String, w: f32| {
            div()
                .absolute()
                .left_0()
                .top_0()
                .h_full()
                .w(px(w))
                .flex()
                .items_center()
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis()
                .child(text)
        };
        let frame = div()
            .relative()
            .flex_none()
            .h(zpx(ROLL_H))
            .overflow_hidden()
            .child(spacer);
        if !rolling {
            return frame.child(line(label, target)).into_any_element();
        }
        frame
            .child(line(label, target).with_animation(
                ("branch-chip-roll", roll * 2 + 1),
                anim(),
                |el, d| el.top(zpx(ROLL_TRAVEL * (1. - d))).opacity(d),
            ))
            .child(
                line(self.branch_label.prev.clone(), self.branch_prev_w).with_animation(
                    ("branch-chip-roll", roll * 2),
                    anim(),
                    |el, d| {
                        // Gone by two thirds of the way, so the two labels never pile up.
                        el.top(zpx(-ROLL_TRAVEL * d))
                            .opacity((1. - d * 1.5).max(0.))
                    },
                ),
            )
            .into_any_element()
    }

    /// The effort levels the slider snaps to: the harness default first,
    /// then the model's levels, lowest to highest.
    pub(super) fn effort_stops(&self) -> Vec<String> {
        std::iter::once(String::new())
            .chain(self.effort_levels())
            .collect()
    }

    /// Where the selected level sits on the slider, 0..1.
    pub(super) fn effort_fraction(&self) -> f32 {
        let stops = self.effort_stops();
        stop_fraction(
            stops
                .iter()
                .position(|s| *s == self.selected_effort)
                .unwrap_or(0),
            stops.len(),
        )
    }

    /// The thinking-effort dropdown: one row per level, highest on top and
    /// the current one checked. Enters and leaves as one surface, exactly
    /// like the model picker (`floating_menu_at`'s motion): no row cascade.
    fn render_effort_menu(
        &mut self,
        leaving: Option<u64>,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let reduced = self.reduced();
        let stops = self.effort_stops();
        let mut items = vec![menu_label("Thinking effort", t).into_any_element()];
        // `i` is the level's rank, 0 the lowest.
        items.extend(stops.into_iter().enumerate().rev().map(|(i, level)| {
            menu_item(
                format!("effort-{i}"),
                t,
                true,
                level == self.selected_effort,
            )
            .child(effort_label(&level))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.select_effort(level.clone(), cx);
                this.close_effort_menu(cx);
            }))
            .into_any_element()
        }));
        let dismiss = cx.listener(|this, _: &gpui::MouseDownEvent, _, cx| {
            this.note_menu_dismissed(EFFORT_MENU_ID);
            this.close_effort_menu(cx);
        });
        let (anchor, at) = match self.effort_chip_bounds.get() {
            Some(b) => (
                gpui::Anchor::BottomLeft,
                Some(gpui::point(b.left(), b.top())),
            ),
            None => (gpui::Anchor::BottomLeft, None),
        };
        super::floating_menu_positioned(
            EFFORT_MENU_ID,
            t,
            180.,
            anchor,
            at,
            true,
            leaving,
            reduced,
            dismiss,
            items,
        )
    }

    fn render_permission_menu(
        &mut self,
        leaving: Option<u64>,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let reduced = self.reduced();
        let current = self.effective_permission();
        let planning = self.plan_on();
        let mut items = vec![menu_label("Permissions", t).into_any_element()];
        items.push(
            menu_item("permission-plan", t, true, planning)
                .child(Icon::ListChecks.el(zpx(PERMISSION_ICON)))
                .child(div().flex_1().child("Plan"))
                .child(
                    div()
                        .flex_none()
                        .text_color(c(t.text_faint))
                        .child("Research, then one plan"),
                )
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.select_plan(cx);
                    this.close_permission_menu(cx);
                }))
                .into_any_element(),
        );
        items.extend(self.permission_modes().to_vec().into_iter().map(|mode| {
            menu_item(
                format!("permission-{}", mode.as_str()),
                t,
                true,
                !planning && mode == current,
            )
            .child(permission_icon(mode).el(zpx(PERMISSION_ICON)))
            .child(div().flex_1().child(permission_label(mode)))
            .child(
                div()
                    .flex_none()
                    .text_color(c(t.text_faint))
                    .child(permission_detail(mode)),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.select_permission(mode, cx);
                this.close_permission_menu(cx);
            }))
            .into_any_element()
        }));
        let dismiss = cx.listener(|this, _: &gpui::MouseDownEvent, _, cx| {
            this.note_menu_dismissed(PERMISSION_MENU_ID);
            this.close_permission_menu(cx);
        });
        let (anchor, at) = match self.permission_chip_bounds.get() {
            Some(b) => (
                gpui::Anchor::BottomLeft,
                Some(gpui::point(b.left(), b.top())),
            ),
            None => (gpui::Anchor::BottomLeft, None),
        };
        super::floating_menu_positioned(
            PERMISSION_MENU_ID,
            t,
            300.,
            anchor,
            at,
            true,
            leaving,
            reduced,
            dismiss,
            items,
        )
    }

    /// The selected level's name, rolling when it changes: the old name slides
    /// out and fades as the new one slides in, upward for a higher level,
    /// downward for a lower one. Same clock as the knob's glide.
    fn effort_roll(&self, id: &'static str, right: bool) -> AnyElement {
        use gpui::AnimationExt as _;
        let current = effort_label(&self.selected_effort);
        let line = |text: String| {
            let d = div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis()
                .child(text);
            if right { d.justify_end() } else { d }
        };
        let frame = div().relative().w_full().h(zpx(ROLL_H)).overflow_hidden();
        if self.effort_glide == 0 || self.reduced() {
            return frame.child(line(current)).into_any_element();
        }
        let up = self.effort_fraction() >= self.effort_from;
        // Up: new comes from below (+), old leaves upward (-). Down: mirrored.
        let travel = if up { ROLL_TRAVEL } else { -ROLL_TRAVEL };
        let n = self.effort_glide as usize;
        let roll =
            || gpui::Animation::new(crate::motion::SLOW).with_easing(crate::motion::ease_out);
        frame
            .child(line(effort_label(&self.effort_prev)).with_animation(
                (id, n * 2),
                roll(),
                move |el, d| {
                    // Gone by two thirds of the way, so the two names never pile up.
                    el.top(zpx(-travel * d)).opacity((1. - d * 1.5).max(0.))
                },
            ))
            .child(
                line(current).with_animation((id, n * 2 + 1), roll(), move |el, d| {
                    el.top(zpx(travel * (1. - d))).opacity(d)
                }),
            )
            .into_any_element()
    }

    /// The model chip's label, rolling when the model changes, like the
    /// effort name: the old label slides out and fades as the new one slides
    /// in, upward for a model later in the list, downward for an earlier one.
    /// The chip's width eases to the new label's full width (see
    /// [`ChipWidth`]); only a row out of room squeezes it.
    fn model_roll_label(
        &mut self,
        id: &'static str,
        window: &gpui::Window,
        cap: Option<f32>,
    ) -> AnyElement {
        let current = self.model_label();
        let target = measure_label(window, &current, gpui::FontWeight::MEDIUM);
        let target = cap.map(|cap| target.min(cap)).unwrap_or(target);
        let prev = self.model_prev.clone();
        let (roll, up) = (self.model_roll, self.model_roll_up);
        let line = move |text: String| {
            div().h_full().flex().items_center().child(
                div()
                    .flex_1()
                    .min_w_0()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(text),
            )
        };
        let key = current.clone();
        self.chip_label(
            ChipLabel::Model,
            id,
            &key,
            target,
            roll,
            up,
            line(current.clone()),
            line(prev),
        )
    }

    /// The permission chip's icon and name, rolling on a change like the
    /// model chip: upward toward Full access, downward toward Ask.
    fn permission_roll_label(
        &mut self,
        id: &'static str,
        window: &gpui::Window,
        icon_only: bool,
    ) -> AnyElement {
        let row = |mode: PermissionMode| {
            div()
                .flex()
                .items_center()
                .whitespace_nowrap()
                .child(permission_icon(mode).el(zpx(PERMISSION_ICON)))
                .when(!icon_only, |d| {
                    d.child(div().ml(zpx(PERMISSION_GAP)).child(permission_label(mode)))
                })
        };
        let current = self.effective_permission();
        let prev = self.permission_prev;
        let target = if icon_only {
            f32::from(zpx(PERMISSION_ICON))
        } else {
            f32::from(zpx(PERMISSION_ICON + PERMISSION_GAP))
                + measure_label(window, permission_label(current), gpui::FontWeight::NORMAL)
        };
        let rank = |m: PermissionMode| PermissionMode::ALL.iter().position(|x| *x == m);
        let up = rank(current) >= rank(prev);
        let roll = self.permission_roll;
        let key = if icon_only {
            format!("icon:{}", current.as_str())
        } else {
            current.as_str().to_string()
        };
        self.chip_label(
            ChipLabel::Permission,
            id,
            &key,
            target,
            roll,
            up,
            row(current),
            row(prev),
        )
    }

    /// A chip label whose width eases on an empty spacer while the labels
    /// roll over it as absolute layers, like the branch chip: gpui keys an
    /// animation's children under its id, so labels inside the width
    /// animation would replay their roll on every width generation.
    #[allow(clippy::too_many_arguments)]
    fn chip_label(
        &mut self,
        which: ChipLabel,
        id: &'static str,
        text: &str,
        target: f32,
        roll: u64,
        up: bool,
        now_el: gpui::Div,
        old_el: gpui::Div,
    ) -> AnyElement {
        use gpui::AnimationExt as _;
        let reduced = self.reduced();
        let now = std::time::Instant::now();
        let slot = match which {
            ChipLabel::Model => &mut self.model_chip_w,
            ChipLabel::Permission => &mut self.permission_chip_w,
        };
        slot.update(text, target, now, !reduced);
        let (width, prev_w) = (slot.tween.expect("set by update"), slot.prev_w);
        let anim =
            || gpui::Animation::new(crate::motion::SLOW).with_easing(crate::motion::ease_out);
        let moving = width.animating(now) && !reduced;
        let spacer = div().h(zpx(ROLL_H));
        let spacer = if moving {
            let (from, to) = (width.from, width.to);
            spacer
                .with_animation(
                    (which.width_id(), width.generation as usize),
                    anim(),
                    move |el, d| el.w(px(from + (to - from) * d)),
                )
                .into_any_element()
        } else {
            spacer.w(px(width.to)).into_any_element()
        };
        let layer = |el: gpui::Div, w: f32| {
            el.absolute()
                .left_0()
                .top_0()
                .h_full()
                .w(px(w))
                .overflow_hidden()
        };
        // At rest the label is held to the frame, so a frame squeezed by its
        // row ends the text in an ellipsis; while the width eases, the label
        // keeps its own width and the frame reveals it.
        let now_layer = |el: gpui::Div| layer(el, target).when(!moving, |d| d.max_w_full());
        // Shrinks only when its row lets it (the model chip, out of room).
        let frame = div()
            .relative()
            .flex_shrink(1.)
            .min_w_0()
            .h(zpx(ROLL_H))
            .overflow_hidden()
            .child(spacer);
        if roll == 0 || reduced {
            return frame.child(now_layer(now_el)).into_any_element();
        }
        let travel = if up { ROLL_TRAVEL } else { -ROLL_TRAVEL };
        let n = roll as usize;
        frame
            .child(
                now_layer(now_el).with_animation((id, n * 2 + 1), anim(), move |el, d| {
                    el.top(zpx(travel * (1. - d))).opacity(d)
                }),
            )
            .child(
                layer(old_el, prev_w).with_animation((id, n * 2), anim(), move |el, d| {
                    // Gone by two thirds of the way, so the two labels never pile up.
                    el.top(zpx(-travel * d)).opacity((1. - d * 1.5).max(0.))
                }),
            )
            .into_any_element()
    }

    /// The search box at the top of a hero picker's menu.
    pub(super) fn picker_search_box(&self, icon: Icon, t: Tokens) -> AnyElement {
        // Borderless, on the rows' inset, with a hairline under it: the
        // field reads as the menu's first row, not a box inside it. flex_none
        // so the scrolling panel never squeezes it below its height.
        div()
            .flex_none()
            .h(zpx(32.))
            .mb_1()
            .px_2()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(c(t.border))
            .t_small()
            .child(
                div()
                    .w(zpx(14.))
                    .flex_none()
                    .flex()
                    .justify_center()
                    .text_color(c(t.text_faint))
                    .child(icon.el(zpx(14.))),
            )
            .child(div().flex_1().min_w_0().child(self.picker_filter.clone()))
            .into_any_element()
    }

    /// A picker's menu; `leaving` renders its exit.
    /// `up`: opens above its anchor (the composer's pickers); the session
    /// card's branch picker opens below.
    pub(super) fn render_picker_menu(
        &mut self,
        picker: Picker,
        leaving: Option<u64>,
        up: bool,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut items: Vec<AnyElement> = Vec::new();
        let id = picker.menu_id();
        let width = match picker {
            Picker::Folder => {
                let query = self.picker_query.clone();
                items.push(self.picker_search_box(Icon::Folder, t));
                items.extend(self.project_picker_rows(t, cx));
                let current = self.state.target_space_for_new_session();
                let is_no_project = |s: &crate::state::SpaceItem| self.state.is_scratch_space(s);
                let none_current =
                    self.state.new_chat_no_project || current.as_ref().is_some_and(&is_no_project);
                let current = current.map(|s| s.id);
                let mut spaces: Vec<_> = self
                    .state
                    .spaces
                    .values()
                    .filter(|s| {
                        !s.tombstoned
                            && s.device_id == self.state.local_device_id
                            && !is_no_project(s)
                    })
                    .cloned()
                    .collect();
                spaces.sort_by_key(|s| std::cmp::Reverse(s.created_at));
                let limit = if query.is_empty() {
                    MENU_ROWS
                } else {
                    MENU_SEARCH_ROWS
                };
                let (rows, more) = filter_rows(&spaces, &query, limit, |s| {
                    [s.name.as_str(), s.path.as_str()]
                });
                self.prefetch_branches(rows.iter().map(|s| s.path.clone()).collect(), cx);
                items.push(
                    menu_label(
                        if query.is_empty() {
                            "Recent folders"
                        } else {
                            "Matches"
                        },
                        t,
                    )
                    .into_any_element(),
                );
                if rows.is_empty() {
                    items.push(if spaces.is_empty() && query.is_empty() {
                        super::menu_empty_row(Icon::History, "No recent folders", t)
                            .into_any_element()
                    } else {
                        menu_label(
                            if spaces.is_empty() {
                                "No folders yet"
                            } else {
                                "No folders match"
                            },
                            t,
                        )
                        .into_any_element()
                    });
                }
                for space in rows {
                    let id = space.id.clone();
                    let row = menu_item(
                        format!("folder-{}", space.id),
                        t,
                        true,
                        current.as_deref() == Some(&space.id),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(c(t.text))
                            .child(space.name.clone()),
                    )
                    .child(faint_path(space.path.clone(), t))
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.state.new_chat_no_project = false;
                            this.state.new_chat_space = Some(id.clone());
                            this.projects.picked = None;
                            this.state.reset_branch_for_folder();
                            this.create_error = None;
                            this.set_picker(None, cx);
                        },
                    ));
                    items.push(row.into_any_element());
                }
                if more > 0 {
                    items.push(menu_label(more_hint(more, &query), t).into_any_element());
                }
                items.push(menu_separator(t).into_any_element());
                items.push(
                    menu_item("folder-none", t, true, none_current)
                        .child("No project")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.set_picker(None, cx);
                            this.use_no_project_for_new_chat(cx);
                        }))
                        .into_any_element(),
                );
                items.push(
                    menu_item("folder-choose", t, true, false)
                        .child("Choose folder")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.set_picker(None, cx);
                            this.pick_folder_for_new_chat(cx);
                        }))
                        .into_any_element(),
                );
                320.
            }
            Picker::Repo => {
                let reduced = self.reduced();
                items.push(self.picker_search_box(Icon::GitLab, t));
                let current = self
                    .issue_repo
                    .as_ref()
                    .map(|(host, p)| (host.clone(), p.id));
                items.push(
                    menu_item("repo-none", t, true, current.is_none())
                        .child("None")
                        .child(
                            div()
                                .t_small()
                                .text_color(c(t.text_faint))
                                .child("issue repo is optional"),
                        )
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.set_picker(None, cx);
                            this.select_issue_repo(None, cx);
                        }))
                        .into_any_element(),
                );
                match self.repo_projects.clone() {
                    None => items.extend(skeleton_rows("repo", 4, t, reduced)),
                    Some(Err(err)) => items.push(
                        menu_label(format!("Couldn't load projects: {err}"), t).into_any_element(),
                    ),
                    Some(Ok(projects)) if projects.is_empty() => items.push(
                        menu_label("Connect GitLab in Settings to pick a repo", t)
                            .into_any_element(),
                    ),
                    Some(Ok(projects)) => {
                        let query = self.picker_query.clone();
                        let limit = if query.is_empty() {
                            MENU_ROWS
                        } else {
                            MENU_SEARCH_ROWS
                        };
                        let (rows, more) = super::gitlab::filter_projects(&projects, &query, limit);
                        items.push(
                            menu_label(
                                if query.is_empty() {
                                    "Recent"
                                } else {
                                    "Matches"
                                },
                                t,
                            )
                            .into_any_element(),
                        );
                        if rows.is_empty() {
                            items.push(menu_label("No repos match", t).into_any_element());
                        }
                        for (host, project) in rows {
                            let selected = current.as_ref() == Some(&(host.clone(), project.id));
                            let pick = (host.clone(), project.clone());
                            let row =
                                menu_item(format!("repo-{host}-{}", project.id), t, true, selected)
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_color(c(t.text))
                                            .child(project.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .text_ellipsis()
                                            .t_small()
                                            .text_color(c(t.text_faint))
                                            .child(project.path.clone()),
                                    )
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.set_picker(None, cx);
                                        this.select_issue_repo(Some(pick.clone()), cx);
                                    }));
                            items.push(row.into_any_element());
                        }
                        if more > 0 {
                            items.push(menu_label(more_hint(more, &query), t).into_any_element());
                        }
                    }
                }
                320.
            }
            Picker::Model => {
                let locked = self.current_chat.is_some();
                // Keel is the one harness, so the section only shows if the
                // engine ever lists more (the test mock).
                let choose = self.harnesses.len() > 1;
                if choose {
                    items.push(
                        menu_label(
                            if locked {
                                "Harness · fixed for this chat"
                            } else {
                                "Harness · new chats"
                            },
                            t,
                        )
                        .into_any_element(),
                    );
                }
                for h in self.harnesses.clone().into_iter().filter(|_| choose) {
                    let id = h.id.clone();
                    let label = if h.installed {
                        h.display_name.clone()
                    } else {
                        format!("{} (not installed)", h.display_name)
                    };
                    let pickable = h.installed && !locked;
                    let mut row = menu_item(
                        format!("harness-{}", h.id),
                        t,
                        pickable,
                        h.id == self.selected_harness,
                    )
                    .child(Icon::harness(&h.id).el(zpx(MENU_LOGO)))
                    .child(label);
                    if pickable {
                        row = row.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.select_harness(id.clone(), cx)
                        }));
                    }
                    items.push(row.into_any_element());
                }
                if choose {
                    items.push(menu_separator(t).into_any_element());
                }
                let (all, width) = self.render_model_menu(std::mem::take(&mut items), t, cx);
                items = all;
                width
            }
            Picker::Branch => {
                let (reduced, query) = (self.reduced(), self.picker_query.clone());
                // Opened from the session card: this chat's own checkout,
                // and a pick switches it. Otherwise the new-chat folder's.
                let card = self.card_branch_menu();
                let from_card = card.is_some();
                let (selected, current, branches, loading, detached) = match card {
                    Some(list) => (
                        list.current.clone(),
                        list.current,
                        list.branches,
                        list.loading,
                        list.detached,
                    ),
                    None => (
                        self.state.selected_branch.clone(),
                        self.state.current_branch.clone(),
                        self.state.available_branches.clone(),
                        self.branches_loading,
                        self.state.detached_head.clone(),
                    ),
                };
                items.push(self.picker_search_box(Icon::Branch, t));
                if loading && branches.is_empty() {
                    items.extend(skeleton_rows("branch", 4, t, reduced));
                } else if branches.is_empty() {
                    items.push(menu_label("No branches (not a git repo?)", t).into_any_element());
                } else {
                    let limit = if query.is_empty() {
                        MENU_ROWS
                    } else {
                        MENU_SEARCH_ROWS
                    };
                    let (rows, more) = filter_rows(&branches, &query, limit, |b| [b.as_str()]);
                    if let Some(sha) = detached.filter(|_| query.is_empty()) {
                        items.push(
                            menu_label(format!("HEAD detached at {sha}"), t).into_any_element(),
                        );
                    }
                    items.push(
                        menu_label(
                            if query.is_empty() {
                                "Branches"
                            } else {
                                "Matches"
                            },
                            t,
                        )
                        .into_any_element(),
                    );
                    if rows.is_empty() {
                        items.push(menu_label("No branches match", t).into_any_element());
                    }
                    for name in rows {
                        let pick = name.clone();
                        let is_current = current.as_deref() == Some(name.as_str());
                        let row = menu_item(
                            format!("branch-option-{name}"),
                            t,
                            true,
                            selected.as_deref() == Some(name.as_str()),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(name.clone()),
                        )
                        // The checked-out branch: listed first, tagged.
                        .when(is_current, |d| {
                            d.child(
                                div()
                                    .flex_none()
                                    .px_1p5()
                                    .rounded(crate::style::RADIUS_SM)
                                    .bg(c(t.hover))
                                    .t_caption()
                                    .text_color(c(t.text_faint))
                                    .child("current"),
                            )
                        })
                        .on_click(cx.listener(
                            move |this, _: &ClickEvent, _, cx| {
                                if from_card {
                                    this.card_pick_branch(pick.clone(), cx);
                                    return;
                                }
                                this.state.set_branch(Some(pick.clone()));
                                this.create_error = None;
                                this.set_picker(None, cx);
                            },
                        ));
                        items.push(row.into_any_element());
                    }
                    if more > 0 {
                        items.push(menu_label(more_hint(more, &query), t).into_any_element());
                    }
                }
                300.
            }
        };
        let dismiss = cx.listener(move |this, _: &gpui::MouseDownEvent, _, cx| {
            this.note_menu_dismissed(picker.menu_id());
            this.set_picker(None, cx);
        });
        if picker == Picker::Model {
            let (anchor, at) = match self.model_chip_bounds.get() {
                Some(b) => (
                    gpui::Anchor::BottomLeft,
                    Some(gpui::point(b.left(), b.top())),
                ),
                None => (
                    if up {
                        gpui::Anchor::BottomLeft
                    } else {
                        gpui::Anchor::TopLeft
                    },
                    None,
                ),
            };
            return super::model_picker::model_menu_shell(
                id,
                t,
                width,
                anchor,
                at,
                up,
                leaving,
                self.reduced(),
                dismiss,
                items,
            );
        }
        if picker == Picker::Branch && self.session_card.picking && self.current_chat.is_some() {
            let compact = self.session_card.compact;
            let (anchor, at, w) = match self.card_branch_bounds.get() {
                Some(b) => {
                    let w = if compact {
                        260.
                    } else {
                        (f32::from(b.size.width) / crate::style::zoom()).max(240.)
                    };
                    let anchor = if compact {
                        gpui::Anchor::TopRight
                    } else {
                        gpui::Anchor::TopLeft
                    };
                    let at = if compact {
                        gpui::point(b.right(), b.bottom())
                    } else {
                        gpui::point(b.left(), b.bottom())
                    };
                    (anchor, Some(at), w)
                }
                None => (gpui::Anchor::TopLeft, None, 252.),
            };
            return super::floating_menu_positioned(
                id,
                t,
                w,
                anchor,
                at,
                false,
                leaving,
                self.reduced(),
                dismiss,
                items,
            );
        }
        floating_menu_at(id, t, width, up, leaving, self.reduced(), dismiss, items)
    }
}

/// How far past its own height the docked panel still reaches, `elapsed`
/// into the hero-to-dock morph: `extra` at the start, gliding to exactly 0
/// on the last frame, so the final animated frame is the settled layout.
fn morph_extra(extra: f32, elapsed: std::time::Duration, duration: std::time::Duration) -> f32 {
    let t = (elapsed.as_secs_f32() / duration.as_secs_f32().max(1e-3)).clamp(0., 1.);
    if t >= 1. {
        return 0.;
    }
    (extra * (1. - crate::motion::glide_curve(t))).max(0.)
}

/// Space between the hero composer's rows (chips, input, controls).
const HERO_GAP: f32 = 10.;
/// The hero panel's side inset (about its corner radius) and its padding
/// above the text and below the controls.
const HERO_INSET: f32 = 16.;
const HERO_PAD_Y: f32 = 14.;
/// The attach button's padding around its glyph: (30 − 16) / 2.
const ATTACH_GLYPH_PAD: f32 = 7.;

/// Space between reference cards (and what a folding card gives back).
const CHIP_GAP: f32 = 6.;

/// Rows a picker menu (folder, branch, repo) lists before a search, and at
/// most during one.
const MENU_ROWS: usize = 6;
const MENU_SEARCH_ROWS: usize = 8;

const EFFORT_MENU_ID: &str = "effort-picker-panel";
const PERMISSION_MENU_ID: &str = "permission-picker-panel";
const PERMISSION_ICON: f32 = 13.;
const PERMISSION_GAP: f32 = 4.;

#[derive(Clone, Copy)]
enum ChipLabel {
    Model,
    Permission,
}

impl ChipLabel {
    fn width_id(self) -> &'static str {
        match self {
            ChipLabel::Model => "model-chip-w",
            ChipLabel::Permission => "permission-chip-w",
        }
    }
}

/// A chip label's eased width, the text it was measured for, and the width
/// of the label before (for the outgoing roll layer).
#[derive(Default)]
pub(super) struct ChipWidth {
    tween: Option<crate::motion::WidthTween>,
    text: String,
    w: f32,
    prev_w: f32,
}

impl ChipWidth {
    /// The same text re-measured (zoom, font load) snaps; new text eases.
    fn update(&mut self, text: &str, target: f32, now: std::time::Instant, animate: bool) {
        let changed = text != self.text;
        match &mut self.tween {
            // First render rests at the label's width, no growing from 0.
            None => self.tween = Some(crate::motion::WidthTween::settled(target)),
            Some(tween) => {
                tween.retarget(target, now, changed && animate);
            }
        }
        if changed {
            self.prev_w = self.w;
            self.text = text.to_string();
        }
        self.w = target;
    }
}

/// The points pill: chips plus a pad on each side, minus the 1 px border.
const POINTS_PILL_PAD: f32 = 3.;
const POINTS_MENU_GAP: f32 = 8.;
/// The jump list's width: room for most of a prompt's first line.
const POINTS_MENU_W: f32 = 340.;
const POINTS_CHIP: f32 = 28.;
/// The suggestion pill's height, inside the reading line (22 px).
const SUGGESTION_PILL_H: f32 = 18.;
/// The pill's fill: the composer surface nudged a few percent toward the
/// text color. Solid, so text under it is hidden, in either scheme.
const PILL_TINT: f32 = 0.05;
/// The pill rises this far as it comes in, and sinks it as it goes.
const PILL_RISE: f32 = 2.;
const PILL_ENTER: std::time::Duration = std::time::Duration::from_millis(160);
/// Exits are quicker than entries.
const PILL_EXIT: std::time::Duration = std::time::Duration::from_millis(110);

/// `a` mixed toward `b` by `k`, opaque. Colors are `0xRRGGBBAA`.
fn mix_rgba(a: u32, b: u32, k: f32) -> u32 {
    let ch = |v: u32, s: u32| ((v >> s) & 0xFF) as f32;
    let m = |s: u32| ((ch(a, s) + (ch(b, s) - ch(a, s)) * k).round() as u32) << s;
    m(24) | m(16) | m(8) | 0xFF
}

/// The suggestion pill's entry and exit, stepped once per frame.
#[derive(Default, Clone, Copy)]
pub(super) struct PillMotion {
    shown: bool,
    enter: Option<std::time::Instant>,
    leave: Option<std::time::Instant>,
}

impl PillMotion {
    /// This frame's `(opacity, rise offset)`, or `None` when nothing draws.
    /// The entry plays only if the suggestion `arrived` within its length
    /// (one that comes back as the input empties appears at once); `exit`
    /// says whether a hide plays out or cuts.
    fn step(
        &mut self,
        visible: bool,
        arrived: Option<std::time::Instant>,
        exit: bool,
        reduced: bool,
        now: std::time::Instant,
    ) -> Option<(f32, f32)> {
        let progress = |since: std::time::Instant, len: std::time::Duration| {
            crate::motion::ease_out(
                (now.duration_since(since).as_secs_f32() / len.as_secs_f32()).min(1.),
            )
        };
        if visible {
            if !self.shown {
                self.shown = true;
                self.leave = None;
                self.enter = arrived.filter(|at| !reduced && now.duration_since(*at) < PILL_ENTER);
            }
            let p = self.enter.map_or(1., |at| progress(at, PILL_ENTER));
            if p >= 1. {
                self.enter = None;
            }
            return Some((p, PILL_RISE * (1. - p)));
        }
        if self.shown {
            self.shown = false;
            self.enter = None;
            self.leave = (exit && !reduced).then_some(now);
        }
        let p = progress(self.leave?, PILL_EXIT);
        if p >= 1. {
            self.leave = None;
            return None;
        }
        Some((1. - p, PILL_RISE * p))
    }

    /// Whether another frame is needed.
    fn animating(&self) -> bool {
        self.enter.is_some() || self.leave.is_some()
    }
}

const POINTS_PILL_H: f32 = POINTS_CHIP + 2. * POINTS_PILL_PAD + 2.;

/// One half of the points pill.
fn points_chip(id: &'static str, icon: Icon, active: bool, t: Tokens) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .size(zpx(POINTS_CHIP))
        .flex()
        .items_center()
        .justify_center()
        .rounded(RADIUS_LG)
        .text_color(c(if active { t.text } else { t.text_muted }))
        .cursor_pointer()
        .when(active, |s| s.bg(c(t.selected)))
        .hover(|s| s.text_color(c(t.text)))
        .hover_fade(c(t.hover), None)
        .child(icon.el(zpx(15.)))
}

fn permission_label(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Ask => "Ask",
        PermissionMode::AutoEdit => "Auto-edit",
        PermissionMode::FullAccess => "Full access",
    }
}

fn permission_detail(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Ask => "Asks before acting",
        PermissionMode::AutoEdit => "Edits run, commands ask",
        PermissionMode::FullAccess => "Nothing asks",
    }
}

fn permission_icon(mode: PermissionMode) -> Icon {
    match mode {
        PermissionMode::Ask => Icon::Comment,
        PermissionMode::AutoEdit => Icon::FilePen,
        PermissionMode::FullAccess => Icon::Alert,
    }
}
/// The branch chip label's widest: the chip then stays under ~320 px
/// (padding, icon, gaps and chevron take the rest).
const BRANCH_LABEL_MAX: f32 = 271.;

/// Width of `text` in the chips' small type, at the current zoom.
fn measure_small_label(window: &gpui::Window, text: &str) -> f32 {
    measure_label(window, text, gpui::FontWeight::NORMAL)
}

/// A chip label's width at the small size and `weight`: the line shaped
/// as it will be drawn. Summing each glyph's raw advance came out short of
/// the drawn line (CoreText tracks and kerns the system font at small
/// sizes), so labels that fitted ended in an ellipsis.
fn measure_label(window: &gpui::Window, text: &str, weight: gpui::FontWeight) -> f32 {
    if text.is_empty() {
        return 0.;
    }
    let run = gpui::TextRun {
        len: text.len(),
        font: gpui::Font {
            weight,
            ..gpui::font(crate::style::SANS)
        },
        color: gpui::black(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let size = zpx(crate::style::SMALL.size);
    let width = f32::from(
        window
            .text_system()
            .shape_line(
                gpui::SharedString::from(text.to_string()),
                size,
                &[run],
                None,
            )
            .width,
    );
    // A pixel of slack so rounding never ellipsizes a label that fits.
    width.ceil() + 1.
}

/// The effort name's roll: the line box it rolls in, and how far each name
/// travels (less than a full line: a nudge, not a slot machine).
const ROLL_H: f32 = 16.;
const ROLL_TRAVEL: f32 = 9.;

/// The effort chip's label width: fits "Default", "Minimal", "Medium".
const EFFORT_CHIP_LABEL_W: f32 = 50.;

/// An effort level as shown: "" is the harness default.
fn effort_label(level: &str) -> String {
    if level.is_empty() {
        "Default".into()
    } else {
        capitalize(level)
    }
}

/// Stop `i` of `n` along a 0..1 track; a lone stop sits at the start.
fn stop_fraction(i: usize, n: usize) -> f32 {
    if n < 2 { 0. } else { i as f32 / (n - 1) as f32 }
}

/// Record the composer slot's inner width (inside its px_4) from the slot's
/// outer `bounds`. A refresh asked for mid-draw is dropped, so a change
/// redraws on the next frame.
pub(super) fn record_slot(
    record: &std::cell::Cell<f32>,
    bounds: gpui::Bounds<gpui::Pixels>,
    window: &mut gpui::Window,
) {
    let inner =
        f32::from(bounds.size.width) - 2. * f32::from(gpui::rems(1.).to_pixels(window.rem_size()));
    if (record.get() - inner).abs() > 0.5 {
        record.set(inner);
        window.request_animation_frame();
    }
}

/// A composer row (chips, thumbnails) opening or closing at `p` (0 = gone,
/// 1 = full): one `line` of it, the 4 px padding above it (a box can't shrink
/// below its padding), and the hero panel's gap below it (cancelled by a
/// negative margin) all move together, so the input resizes smoothly.
/// Pixel-exact on purpose: the hero composer is centered, so it moves by
/// half of every height change. The total changes in even steps of real
/// (zoomed) pixels, so the panel and its text always land on whole pixels;
/// fractional steps made the placeholder wobble as it settled.
fn reveal_row(el: gpui::Div, p: f32, line: f32, hero: bool) -> gpui::Div {
    let (pad, below) = (4., if hero { HERO_GAP } else { 0. });
    let z = crate::style::zoom();
    let total = ((line + below) * z * p / 2.).round() * 2.;
    if total <= 0. {
        // Fully closed: out of the flow entirely. In it, the margin that
        // cancels the gap leaves a negative outer height, which the layout
        // floors at zero when sizing the panel: 12 px too tall for the
        // first and last frame, and the text jumped.
        return el
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h_0()
            .overflow_hidden();
    }
    let height = (line * z * p).round().min(total);
    el.overflow_hidden()
        .pt(px((pad * z * p).round().min(height)))
        .max_h(px(height))
        .mb(px(total - height) - zpx(below))
}

/// Placeholder rows in the shape of a name-and-path row, shimmering in a
/// ripple, while a picker's list loads.
fn skeleton_rows(prefix: &'static str, count: usize, t: Tokens, reduced: bool) -> Vec<AnyElement> {
    (0..count)
        .map(|i| {
            let bar = |w: f32| {
                div()
                    .h(zpx(10.))
                    .w(zpx(w))
                    .rounded(zpx(3.))
                    .bg(c(t.surface))
            };
            let phase = i as f32 * 0.12;
            div()
                .h(zpx(28.))
                .px_2()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(div().w(zpx(14.)).flex_none())
                .child(crate::motion::shimmer(
                    bar(70. + 18. * (i % 3) as f32),
                    (prefix, i * 2),
                    phase,
                    c(t.hover),
                    reduced,
                ))
                .child(crate::motion::shimmer(
                    bar(110. - 14. * (i % 2) as f32),
                    (prefix, i * 2 + 1),
                    phase + 0.06,
                    c(t.hover),
                    reduced,
                ))
                .into_any_element()
        })
        .collect()
}

/// Model rows' height: `menu_item`'s 4 px padding around an 18 px line.
const MODEL_ROW_H: f32 = 26.;
const SKELETON_ROWS: usize = 4;

/// Placeholder model rows while a harness lists its models. Tinted like
/// the menu's own hover (a few percent of the text colour over the menu),
/// never an opaque fill: a soft pill where the label sits and a shorter one
/// where the id caption sits, and one faint band sweeping each row on the
/// shared clock (the trace lines' sweep, no per-row animation).
pub(super) fn model_skeleton(t: Tokens, clock_ms: f32, reduced: bool) -> Vec<AnyElement> {
    // (label width, id width): varied like real names and ids.
    const WIDTHS: [(f32, f32); SKELETON_ROWS] = [(92., 52.), (128., 70.), (76., 40.), (112., 60.)];
    (0..SKELETON_ROWS)
        .map(|i| {
            let (label_w, id_w) = WIDTHS[i];
            let pill = |w: f32, h: f32, a: f32| {
                div()
                    .flex_none()
                    .h(zpx(h))
                    .w(zpx(w))
                    .rounded_full()
                    .bg(c(t.text).opacity(a))
            };
            let band = (!reduced)
                .then(|| crate::trace_view::shimmer_left(clock_ms, i as f32 * 0.06))
                .flatten()
                .map(|left| {
                    let hl = c(t.text).opacity(0.05);
                    let clear = gpui::Hsla { a: 0., ..hl };
                    let half = |a: gpui::Hsla, b: gpui::Hsla| {
                        div().flex_1().h_full().bg(gpui::linear_gradient(
                            90.,
                            gpui::linear_color_stop(a, 0.),
                            gpui::linear_color_stop(b, 1.),
                        ))
                    };
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(gpui::relative(left))
                        .w(gpui::relative(0.6))
                        .flex()
                        .flex_row()
                        .child(half(clear, hl))
                        .child(half(hl, clear))
                });
            div()
                .relative()
                .overflow_hidden()
                .h(zpx(MODEL_ROW_H))
                .px_2()
                .rounded(crate::style::RADIUS_SM)
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                // The check column, empty as on unselected rows.
                .child(div().w(zpx(14.)).flex_none())
                .child(pill(label_w, 8., 0.08))
                .child(div().flex_1())
                .child(pill(id_w, 6., 0.05))
                .children(band)
                .into_any_element()
        })
        .collect()
}

/// A row's secondary text: a path, faint and cut short with an ellipsis.
fn faint_path(path: String, t: Tokens) -> impl IntoElement {
    div()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .t_small()
        .text_color(c(t.text_faint))
        .child(path)
}

/// Under a capped list: how many more matched, and how to reach them.
fn more_hint(more: usize, query: &str) -> String {
    if query.is_empty() {
        format!("{more} more — search to find them")
    } else {
        format!("{more} more — keep typing")
    }
}

/// How long a picker search waits after the last keystroke.
const PICKER_SEARCH_DELAY: std::time::Duration = std::time::Duration::from_millis(180);

/// A picker menu's rows: with no query the first `limit` (in the order
/// given), else up to `limit` whose `text` fields contain it, ignoring case.
/// Also how many more matched but aren't shown.
pub(super) fn filter_rows<'a, T, const N: usize>(
    rows: &'a [T],
    query: &str,
    limit: usize,
    text: impl Fn(&'a T) -> [&'a str; N],
) -> (Vec<&'a T>, usize) {
    let query = query.to_lowercase();
    let matching: Vec<&T> = rows
        .iter()
        .filter(|row| {
            query.is_empty()
                || text(row)
                    .iter()
                    .any(|field| field.to_lowercase().contains(&query))
        })
        .collect();
    let more = matching.len().saturating_sub(limit);
    (matching.into_iter().take(limit).collect(), more)
}

/// Height of the folder / branch / repo tab above the hero composer.
const TAB_HEIGHT: f32 = 34.;

/// Hero composer width: narrower than the transcript, like a search field.
pub(super) const HERO_COMPOSER_WIDTH: gpui::Pixels = px(640.);

/// "high" → "High".
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// A round icon-only control (attach).
/// The editor's most rows (`auto_grow(1, 8)` in the view); the eased box
/// holds to that many lines.
const COMPOSER_MAX_ROWS: f32 = 8.;
/// Below this text width, inline, the composer stacks.
const FIT_MIN_TEXT: f32 = 260.;
/// Room for a layout to come back must beat the line by this much, so
/// resize jitter doesn't flip it.
const FIT_HYST: f32 = 16.;
/// Panel width that's neither text nor chips: border, padding, attach, gaps.
const FIT_FIXED: f32 = 66.;
/// Inline ⇄ stacked glide.
const FIT_MS: f32 = 200.;

/// How the docked composer lays out at its width.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct ComposerFit {
    /// Text on top at full width, controls in a row below.
    pub stacked: bool,
    /// 0 full chips; 1 the permission chip is just its icon; 2 the model
    /// name may also shrink, ellipsized, to what's left of the row.
    pub compact: u8,
}

/// Width of the left-hand chips (permission, project scope) at each
/// compaction level. Level 0 is the full labels, 1 drops the permission
/// name and shortens the scope chip, 2 keeps both as icons.
pub(super) fn composer_left_widths(permission: bool, scope: bool) -> [f32; 3] {
    let perm = |icon: bool| {
        if !permission {
            0.
        } else if icon {
            48.
        } else {
            130.
        }
    };
    let scope = |level: u8| {
        if !scope {
            0.
        } else {
            match level {
                0 => 180.,
                1 => 96.,
                _ => 44.,
            }
        }
    };
    [
        perm(false) + scope(0),
        perm(true) + scope(1),
        perm(true) + scope(2),
    ]
}

/// The layout for a panel `panel` wide, given the fixed room and the
/// controls' widths at each compaction level. `left` is the permission and
/// scope chips, which sit outside the measured model cluster. Inline only
/// at full chips, when the text keeps `min_text`; stacked otherwise, as
/// little compacted as fits. A layout comes back only with `hyst` to spare.
pub(super) fn fit_composer(
    panel: f32,
    fixed: f32,
    left: [f32; 3],
    ctrl: [f32; 3],
    prev: ComposerFit,
    min_text: f32,
    hyst: f32,
) -> ComposerFit {
    let text = panel - fixed - left[0] - ctrl[0];
    let need = if prev.stacked {
        min_text + hyst
    } else {
        min_text
    };
    if text >= need {
        return ComposerFit::default();
    }
    let room = panel - fixed;
    let was = if prev.stacked { prev.compact } else { 0 };
    let compact = (0..3u8)
        .find(|&k| left[k as usize] + ctrl[k as usize] + if k < was { hyst } else { 0. } <= room)
        .unwrap_or(2);
    ComposerFit {
        stacked: true,
        compact,
    }
}

impl KeelWindow {
    /// Decide the docked layout from last frame's measurements.
    fn update_composer_fit(&mut self) -> ComposerFit {
        let z = |v: f32| f32::from(zpx(v));
        // Mid-drop from the hero the panel is squeezed by the drop's widening:
        // decide from the slot it lands in instead, so the layout neither
        // flips nor glides during the handoff.
        let dropping = self.hero_exiting();
        let (slot, max) = (self.composer_slot.get(), self.composer_max.get());
        // Mid-slide the dock already has its landing width (`column_slide`):
        // decide from that, not last frame's box, so it doesn't lag a frame.
        let landing = self
            .column_slide
            .map(|s| s.to.w)
            .filter(|_| !dropping && max > 0.);
        let panel = if let Some(w) = landing {
            max.min(w - z(32.))
        } else if dropping && slot > 0. && max > 0. {
            max.min(slot - z(32.))
        } else {
            self.composer_panel_w.get()
        };
        if panel <= 0. {
            return self.composer_fit;
        }
        let got = *self.composer_ctrl_w.borrow();
        // Levels not seen yet: guessed from the full width (slimmer now that
        // permission is in the top-left accessory).
        let full = if got[0] > 0. { got[0] } else { z(200.) };
        let guess = [full, full - z(40.), full - z(80.)];
        let ctrl = [0, 1, 2].map(|k| if got[k] > 0. { got[k] } else { guess[k] });
        let permission = self.permission_modes().len() > 1;
        let scope = self
            .current_chat
            .as_deref()
            .is_some_and(|id| self.projects.chat_project_id(id).is_some());
        let left = composer_left_widths(permission, scope).map(z);
        let next = fit_composer(
            panel,
            z(FIT_FIXED),
            left,
            ctrl,
            self.composer_fit,
            z(FIT_MIN_TEXT),
            z(FIT_HYST),
        );
        // Mid-slide the text reflows once, at the start; so does this, with
        // no glide racing the slide (it would blank the controls mid-move).
        if next.stacked != self.composer_fit.stacked
            && self.composer_fit_known
            && !dropping
            && landing.is_none()
            && !self.reduced()
        {
            self.composer_fit_at = Some(std::time::Instant::now());
        }
        self.composer_fit = next;
        self.composer_fit_known = true;
        next
    }

    /// Eased progress (0→1) of the inline ⇄ stacked glide; `None` at rest.
    fn composer_fit_progress(&mut self, window: &gpui::Window) -> Option<f32> {
        let at = self.composer_fit_at?;
        let x = at.elapsed().as_secs_f32() * 1000. / FIT_MS;
        if x >= 1. || self.reduced() {
            self.composer_fit_at = None;
            return None;
        }
        window.request_animation_frame();
        Some(crate::motion::ease_out(x))
    }
}

fn icon_chip(id: &'static str, icon: Icon, t: Tokens) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .size(zpx(30.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .text_color(c(t.text_muted))
        .cursor_pointer()
        .hover(|s| s.text_color(c(t.text)))
        .hover_fade(c(t.hover), None)
        .child(icon.el(zpx(16.)))
}

/// Borderless control in the composer's picker row.
/// Harness logos sit 1 px under the text beside them (menu rows are `t_body`,
/// 13 px), centered on it. The logo SVGs are padded to one optical size, so a
/// solid mark (Pi) doesn't read bigger than line art.
const MENU_LOGO: f32 = 12.;

impl KeelWindow {
    /// Plan mode, beside the access and model controls. Off until this chat
    /// has a saved value. A new chat uses the hero's unsaved toggle.
    fn plan_on(&self) -> bool {
        if let Some(id) = &self.current_chat
            && let Some(chat) = self.state.chats.get(id)
        {
            return chat.plan_mode;
        }
        self.plan_mode_draft
    }

    fn plan_toggle(&mut self, t: Tokens, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let on = self.plan_on();
        chip("plan-mode-btn", t, on)
            .child("Plan")
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.toggle_plan_mode(cx);
            }))
    }
}

fn chip(id: &'static str, t: Tokens, active: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(zpx(26.))
        .px_2()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .rounded(RADIUS_MD)
        .t_small()
        .text_color(c(if active { t.text } else { t.text_muted }))
        .cursor_pointer()
        .when(active, |s| s.bg(c(t.selected)))
        .hover(|s| s.text_color(c(t.text)))
        .hover_fade(c(t.hover), None)
}

#[cfg(test)]
mod slider_tests {
    use super::*;

    #[test]
    fn the_suggestion_pill_rises_in_and_sinks_out_faster() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut m = PillMotion::default();
        // A fresh suggestion rises in, under 300 ms, then holds.
        assert_eq!(
            m.step(true, Some(t0), true, false, t0),
            Some((0., PILL_RISE))
        );
        let (o, dy) = m.step(true, Some(t0), true, false, ms(80)).unwrap();
        assert!(o > 0.5 && o < 1. && dy > 0. && dy < PILL_RISE, "{o} {dy}");
        assert!(m.animating());
        assert_eq!(m.step(true, Some(t0), true, false, ms(160)), Some((1., 0.)));
        assert!(!m.animating());
        // Cleared (or clicked): it sinks out, quicker than it came in.
        assert_eq!(
            m.step(false, Some(t0), true, false, ms(1000)),
            Some((1., 0.))
        );
        assert!(m.step(false, Some(t0), true, false, ms(1060)).unwrap().0 < 0.5);
        assert_eq!(m.step(false, Some(t0), true, false, ms(1110)), None);
        assert!(!m.animating());
        assert!(PILL_EXIT < PILL_ENTER && PILL_ENTER < Duration::from_millis(300));
    }

    #[test]
    fn keyboard_changes_and_reduced_motion_show_the_pill_at_once() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let later = t0 + Duration::from_secs(5);
        let mut m = PillMotion::default();
        // Back after the input emptied: no entry.
        assert_eq!(m.step(true, Some(t0), false, false, later), Some((1., 0.)));
        // Typing hides it with no exit.
        assert_eq!(m.step(false, Some(t0), false, false, later), None);
        assert!(!m.animating());
        // Reduced motion: neither plays.
        let mut r = PillMotion::default();
        assert_eq!(r.step(true, Some(later), true, true, later), Some((1., 0.)));
        assert_eq!(r.step(false, Some(later), true, true, later), None);
        assert!(SUGGESTION_PILL_H <= crate::style::READING.line);
    }

    #[test]
    fn the_pill_fill_is_solid_and_just_off_the_surface() {
        assert_eq!(mix_rgba(0x00000000, 0xFFFFFFFF, 0.5), 0x808080FF);
        let fill = mix_rgba(0xEEEAE4FF, 0x1E1D1BFF, PILL_TINT);
        assert_eq!(fill & 0xFF, 0xFF);
        let red = (fill >> 24) as i32;
        assert!((0xEE - red) > 0 && (0xEE - red) < 16, "{fill:08X}");
    }

    #[test]
    fn a_disabled_send_is_the_same_disc_only_dimmed() {
        for dark in [false, true] {
            let t = keel_theme::resolve_theme(None, dark);
            let (on_bg, on_fg, on) = send_look(true, t);
            let (off_bg, off_fg, off) = send_look(false, t);
            assert_eq!((on_bg, on_fg), (off_bg, off_fg));
            assert_eq!(on, 1.);
            assert!(off > 0.3 && off < 0.6, "{off}");
        }
    }

    #[test]
    fn composer_stacks_when_the_text_would_be_squeezed_with_hysteresis() {
        let ctrl = [330., 260., 220.];
        let fit = |panel: f32, prev: ComposerFit| {
            fit_composer(panel, 66., [0., 0., 0.], ctrl, prev, 260., 16.)
        };
        let inline = ComposerFit::default();
        // Wide: inline. The line is 66 + 330 + 260 = 656.
        assert_eq!(fit(720., inline), inline);
        assert_eq!(fit(656., inline), inline);
        let stacked = fit(655., inline);
        assert!(stacked.stacked);
        assert_eq!(stacked.compact, 0);
        // Back to inline only past the line plus 16 px: no flip on jitter.
        assert!(fit(660., stacked).stacked);
        assert!(fit(671., stacked).stacked);
        assert_eq!(fit(672., stacked), inline);
        // Narrower: the permission chip goes to its icon, then the model
        // name shortens; never anything wider than the room.
        assert_eq!(fit(380., stacked).compact, 1);
        let tight = fit(300., stacked);
        assert_eq!(
            tight,
            ComposerFit {
                stacked: true,
                compact: 2
            }
        );
        assert_eq!(
            fit(200., stacked).compact,
            2,
            "the most compact, even if over"
        );
        // Growing back needs the 16 px too.
        assert_eq!(fit(330., tight).compact, 2);
        assert_eq!(fit(342., tight).compact, 1);
        let one = fit(342., tight);
        assert_eq!(fit(400., one).compact, 1);
        assert_eq!(fit(412., one).compact, 0);
        // The screenshot's ~350 px column stacks.
        assert!(fit(350., inline).stacked);
    }

    #[test]
    fn project_scope_forces_a_tighter_composer_before_the_row_overflows() {
        let ctrl = [160., 140., 100.];
        let left = composer_left_widths(true, true);
        let stacked = ComposerFit {
            stacked: true,
            compact: 0,
        };
        let wide = fit_composer(560., 66., left, ctrl, ComposerFit::default(), 260., 16.);
        assert!(wide.stacked);
        assert_eq!(wide.compact, 0);
        let mid = fit_composer(400., 66., left, ctrl, stacked, 260., 16.);
        assert_eq!(mid.compact, 1, "permission icon and a short scope label");
        let tight = fit_composer(280., 66., left, ctrl, mid, 260., 16.);
        assert_eq!(tight.compact, 2);
    }

    #[test]
    fn hero_morph_lands_on_the_settled_layout() {
        use std::time::Duration;
        let d = Duration::from_millis(380);
        assert!((morph_extra(120., Duration::ZERO, d) - 120.).abs() < 0.1);
        let mid = morph_extra(120., Duration::from_millis(190), d);
        assert!(mid > 0. && mid < 120.);
        // The last frame and after: no extra height left to snap away.
        assert_eq!(morph_extra(120., d, d), 0.);
        assert_eq!(morph_extra(120., Duration::from_secs(2), d), 0.);
        // Shrinks every frame, never back up.
        let steps: Vec<f32> = (0..=38)
            .map(|i| morph_extra(120., Duration::from_millis(i * 10), d))
            .collect();
        assert!(steps.windows(2).all(|w| w[1] <= w[0]));
    }

    #[test]
    fn effort_stops_and_labels() {
        assert_eq!(
            (
                stop_fraction(0, 4),
                stop_fraction(3, 4),
                stop_fraction(0, 1)
            ),
            (0., 1., 0.)
        );
        assert_eq!(effort_label(""), "Default");
    }
}

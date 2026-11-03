//! Window-level keyboard actions and the hold-⌘ key-cap hints.

use gpui::{Context, Div, prelude::*};
use keel_proto::ChatStatus;

use super::KeelWindow;
use crate::keys::{self, *};

impl KeelWindow {
    /// Attach every window action to the root element.
    pub(super) fn bind_actions(&mut self, root: Div, cx: &mut Context<Self>) -> Div {
        root.key_context(keys::CONTEXT)
            // Before a focused input's own Cmd-C, which swallows it even when
            // empty: selected transcript text wins.
            .capture_action(|_: &gpui_base::input::Copy, window, cx| {
                let text = gpui_base::TextSelection::selected_text(window, cx);
                if !text.trim().is_empty() {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                    cx.stop_propagation();
                }
            })
            .on_action(cx.listener(|this, _: &NewChat, _, cx| this.create_new_session(None, cx)))
            .on_action(cx.listener(|this, _: &OpenSettings, _, cx| this.toggle_settings(cx)))
            .on_action(cx.listener(|this, _: &ZoomIn, _, cx| {
                this.set_zoom(crate::style::zoom_step(crate::style::zoom(), true), cx)
            }))
            .on_action(cx.listener(|this, _: &ZoomOut, _, cx| {
                this.set_zoom(crate::style::zoom_step(crate::style::zoom(), false), cx)
            }))
            .on_action(cx.listener(|this, _: &ZoomReset, _, cx| this.set_zoom(1.0, cx)))
            .on_action(cx.listener(|this, _: &FocusComposer, window, cx| {
                this.composer.update(cx, |s, cx| s.focus(window, cx));
            }))
            .on_action(cx.listener(|this, _: &AcceptPromptSuggestion, window, cx| {
                this.accept_suggestion(window, cx)
            }))
            .on_action(cx.listener(|this, _: &ExpandSteps, _, cx| this.toggle_expand_steps(cx)))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.toggle_sidebar(cx);
            }))
            .on_action(
                cx.listener(|this, _: &ToggleTerminal, window, cx| {
                    this.toggle_terminal(window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &ToggleBrowser, window, cx| this.toggle_browser(window, cx)),
            )
            .on_action(cx.listener(|this, _: &BrowserAddress, window, cx| {
                this.focus_browser_address(window, cx)
            }))
            .on_action(cx.listener(|this, _: &BrowserReload, _, cx| this.browser_reload(cx)))
            .on_action(cx.listener(|this, _: &BrowserBack, _, _| this.browser_history(false)))
            .on_action(cx.listener(|this, _: &BrowserForward, _, _| this.browser_history(true)))
            .on_action(cx.listener(|this, _: &BrowserExpand, _, cx| this.toggle_browser_expand(cx)))
            .on_action(cx.listener(|this, _: &BrowserEndSession, window, cx| {
                this.end_browser_session(window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &BrowserNewTab, window, cx| this.browser_new_tab(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ToggleFiles, window, cx| this.toggle_files(window, cx)),
            )
            .on_action(cx.listener(|this, _: &FilesSave, _, cx| this.files_save(false, cx)))
            .on_action(cx.listener(|this, _: &BrowserCloseTab, window, cx| {
                this.browser_close_active_tab(window, cx)
            }))
            .on_action(cx.listener(|this, _: &TimerStartPause, _, cx| this.timer_start_pause(cx)))
            .on_action(cx.listener(|this, _: &TimerReset, _, cx| this.timer_reset(cx)))
            .on_action(cx.listener(|this, _: &ToggleTimer, _, cx| {
                this.timer_set_hidden(!this.timer.hidden, cx)
            }))
            .on_action(cx.listener(|this, _: &TrackMergeRequest, window, cx| {
                this.mr_open_prompt(window, cx)
            }))
            .on_action(cx.listener(|this, _: &NextChat, _, cx| this.step_chat(1, cx)))
            .on_action(cx.listener(|this, _: &PrevChat, _, cx| this.step_chat(-1, cx)))
            .on_action(cx.listener(|this, _: &CloseTab, _, cx| {
                if let Some(id) = this.state.active_tab.clone() {
                    this.close_menus(cx);
                    this.state.close_tab(&id);
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &ArchiveChat, _, cx| {
                if let Some(id) = this.state.active_tab.clone() {
                    this.archive_chat(id, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &StopRun, _, cx| {
                let busy = matches!(
                    this.active_status(),
                    Some(ChatStatus::Running | ChatStatus::AwaitingInput)
                );
                if busy {
                    this.handle_interrupt(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &Dismiss, _, cx| {
                // Closes menus first; with none open, Escape goes on to the
                // question panel and text fields.
                if !this.close_menus(cx) {
                    if this.browser.expanded {
                        this.toggle_browser_expand(cx);
                    } else if this.settings_open || this.gitlab_open {
                        this.close_pages();
                        this.finish_settings_exit(cx);
                        cx.notify();
                    } else {
                        cx.propagate();
                    }
                }
            }))
            .on_action(cx.listener(|this, _: &JumpTab1, _, cx| this.jump_tab(1, cx)))
            .on_action(cx.listener(|this, _: &JumpTab2, _, cx| this.jump_tab(2, cx)))
            .on_action(cx.listener(|this, _: &JumpTab3, _, cx| this.jump_tab(3, cx)))
            .on_action(cx.listener(|this, _: &JumpTab4, _, cx| this.jump_tab(4, cx)))
            .on_action(cx.listener(|this, _: &JumpTab5, _, cx| this.jump_tab(5, cx)))
            .on_action(cx.listener(|this, _: &JumpTab6, _, cx| this.jump_tab(6, cx)))
            .on_action(cx.listener(|this, _: &JumpTab7, _, cx| this.jump_tab(7, cx)))
            .on_action(cx.listener(|this, _: &JumpTab8, _, cx| this.jump_tab(8, cx)))
            .on_action(cx.listener(|this, _: &JumpTab9, _, cx| this.jump_tab(9, cx)))
            // Text fields bind Escape themselves; an open menu gets it first.
            .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                let key = event.keystroke.key.as_str();
                let m = event.keystroke.modifiers;
                if !this.lightbox_fx.shown() && this.dotify_key(event, window, cx) {
                    cx.stop_propagation();
                } else if let Some(super::Lightbox::Diagram { source, view, .. }) =
                    this.lightbox.as_ref().filter(|_| this.lightbox_fx.shown())
                    && !m.secondary()
                    && matches!(key, "=" | "+" | "-" | "0")
                {
                    let natural = crate::mermaid::size(source).unwrap_or((1., 1.));
                    match key {
                        "0" => view.borrow_mut().reset(),
                        "-" => view.borrow_mut().zoom_center(1. / 1.25, natural),
                        _ => view.borrow_mut().zoom_center(1.25, natural),
                    }
                    cx.stop_propagation();
                    cx.notify();
                } else if key == "escape" && this.close_lightbox(cx) {
                    cx.stop_propagation();
                } else if key == "escape" && this.close_menus(cx) {
                    cx.stop_propagation();
                } else if key == "v" && m.secondary() && !m.shift && !m.alt && this.paste_images(cx)
                {
                    // An image paste attaches; text pastes reach the field as usual.
                    cx.stop_propagation();
                }
            }))
    }

    fn step_chat(&mut self, delta: isize, cx: &mut Context<Self>) {
        let rows = self.sidebar_row_ids();
        if rows.is_empty() {
            return;
        }
        let len = rows.len() as isize;
        let next = match self
            .state
            .active_tab
            .as_ref()
            .and_then(|id| rows.iter().position(|r| r == id))
        {
            Some(ix) => (ix as isize + delta).rem_euclid(len),
            None if delta < 0 => len - 1,
            None => 0,
        };
        if let Some(id) = rows.get(next as usize).cloned() {
            self.close_menus(cx);
            self.projects.close_home();
            self.state.open_or_focus_tab(&id);
            cx.notify();
        }
    }

    fn jump_tab(&mut self, n: usize, cx: &mut Context<Self>) {
        if let Some(id) = self.state.tab_at(n) {
            self.close_menus(cx);
            self.projects.close_home();
            self.state.active_tab = Some(id);
            cx.notify();
        }
    }

    /// Close every menu and picker. True when one was open.
    pub(super) fn close_menus(&mut self, cx: &mut Context<Self>) -> bool {
        if self.skills.modal_skill.is_some() {
            self.dismiss_skill_modal(cx);
            return true;
        }
        let timer_menu = self.timer_close_menu();
        let mr_menu = self.mr_close_menus();
        let scope_menu = self.projects.close_scope();
        let project_sheet = self.projects.close_sheet();
        let open = timer_menu
            || mr_menu
            || scope_menu
            || project_sheet
            || self.state.space_menu.is_some()
            || self.state.show_overflow_menu
            || self.show_harness_picker
            || self.show_model_picker
            || self.show_effort_picker
            || self.show_effort_menu
            || self.show_permission_menu
            || self.show_subagent_menu
            || self.show_points_menu
            || self.show_sound_menu
            || self.session_card.picking
            || self.state.show_branch_picker
            || self.confirming_delete_chats.is_some()
            || self.delete_confirm_leaving.is_some()
            || self.select_mode
            || !self.selected_chats.is_empty();
        // Escape backs out of the delete modal (with its exit), then selection.
        if self.confirming_delete_chats.is_some() {
            self.dismiss_delete_confirm(cx);
        } else {
            self.clear_selection(cx);
        }
        self.select_mode = false;
        self.state.space_menu = None;
        self.state.show_overflow_menu = false;
        self.show_harness_picker = false;
        self.show_model_picker = false;
        self.show_effort_picker = false;
        self.show_effort_menu = false;
        self.show_permission_menu = false;
        self.show_subagent_menu = false;
        self.show_points_menu = false;
        self.show_sound_menu = false;
        self.state.show_branch_picker = false;
        self.card_end_picking();
        if open {
            cx.notify();
        }
        open
    }
}

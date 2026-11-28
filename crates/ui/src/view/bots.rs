//! Keel Bots UI: a messaging inbox (pinned lab chat, then coworker threads), the DM
//! header and empty thread, the built-in Dr. Keelee creator, and the split-studio Bot
//! Customizer modal (name, color, attire, eyes, hair, mustache, size).

use std::path::{Path, PathBuf};

use gpui::{
    AnimationExt as _, AnyElement, ClickEvent, Context, ElementId, Entity, IntoElement,
    ScrollHandle, Window, div, prelude::*, px, relative,
};
use gpui_base::input::InputState;
use keel_proto::DisplayStatus;
use keel_proto::bots::{
    self, Bot, BotAppearance, BotAttire, BotColor, BotEyes, BotHair, BotMustache, BotSize,
    BotsStore, DR_KEELEE_ID,
};
use keel_theme::Tokens;

use super::sidebar::{filled_button, text_button};
use super::{KeelWindow, drag_region, icon_button, menu_action, menu_separator, sidebar_menu_at};
use crate::creature::{BotMood, random_appearance, render_creature_avatar, render_creature_stage};
use crate::hover_fade::{HoverFadeExt, reveal_on_hover};
use crate::icons::Icon;
use crate::style::{
    COLUMN_PAD, HEADER_HEIGHT, INPUT_H, KeelStyled, MONO, RADIUS_LG, RADIUS_MD, RADIUS_SM,
    RADIUS_XL, TRANSCRIPT_MAX_WIDTH, c, r_md, zpx,
};

/// Filter tabs on the Bots directory page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum BotStatusFilter {
    #[default]
    All,
    Active,
    Ready,
}

/// Active state for the Bot Customization modal.
pub(super) struct CustomizeDraft {
    pub(super) bot_id: String,
    pub(super) builtin: bool,
    pub(super) tagline: String,
    pub(super) name_input: Entity<InputState>,
    pub(super) appearance: BotAppearance,
    pub(super) pop_at: Option<std::time::Instant>,
    pub(super) rand_seed: u64,
}

/// Window-local state for Keel Bots.
pub(super) struct BotsUi {
    data_dir: PathBuf,
    pub(super) store: BotsStore,
    pub(super) page_open: bool,
    pub(super) page_scroll: ScrollHandle,
    pub(super) filter: BotStatusFilter,
    pub(super) filter_gen: u64,
    pub(super) filter_at: Option<std::time::Instant>,
    pub(super) card_menu_bot: Option<String>,
    pub(super) card_menu_owner: Option<String>,
    pub(super) customize: Option<CustomizeDraft>,
    pub(super) customize_open: bool,
    pub(super) customize_leaving: Option<u64>,
    pub(super) customize_gen: u64,
    pub(super) show_header_menu: bool,
    pub(super) confirming_delete: Option<String>,
    pub(super) delete_gen: u64,
    pub(super) delete_leaving: Option<(u64, String)>,
    pub(super) info_bot: Option<String>,
    pub(super) info_gen: u64,
    pub(super) info_leaving: Option<(u64, String)>,
    pub(super) info_scroll: ScrollHandle,
    pub(super) info_copied_at: Option<std::time::Instant>,
}

impl BotsUi {
    pub(super) fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(bots::FILE);
        let store = std::fs::read_to_string(&path)
            .map(|text| BotsStore::parse(&text))
            .unwrap_or_default();
        Self {
            data_dir: data_dir.to_path_buf(),
            store,
            page_open: false,
            page_scroll: ScrollHandle::new(),
            filter: BotStatusFilter::All,
            filter_gen: 0,
            filter_at: None,
            card_menu_bot: None,
            card_menu_owner: None,
            customize: None,
            customize_open: false,
            customize_leaving: None,
            customize_gen: 0,
            show_header_menu: false,
            confirming_delete: None,
            delete_gen: 0,
            delete_leaving: None,
            info_bot: None,
            info_gen: 0,
            info_leaving: None,
            info_scroll: ScrollHandle::new(),
            info_copied_at: None,
        }
    }

    pub(super) fn update_from_view(&mut self, view: &serde_json::Value) {
        if let Ok(mut parsed) = serde_json::from_value::<BotsStore>(view.clone()) {
            parsed.ensure_dr_keelee();
            self.store = parsed;
        }
    }

    pub(super) fn save_local_fallback(&self) {
        let path = self.data_dir.join(bots::FILE);
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, self.store.to_json())
            .and_then(|()| std::fs::rename(&tmp, &path))
            .is_err()
        {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

fn input_value(input: &Entity<InputState>, cx: &Context<KeelWindow>) -> String {
    input.read(cx).value().to_string()
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

fn format_bot_created_at(created_at: u64, builtin: bool) -> String {
    if builtin || created_at == 0 {
        return "Built-in".to_string();
    }
    let secs = if created_at > 10_000_000_000 {
        (created_at / 1000) as libc::time_t
    } else {
        created_at as libc::time_t
    };
    #[cfg(unix)]
    {
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let res = unsafe { libc::localtime_r(&secs, &mut tm) };
        if !res.is_null() {
            return format!(
                "{} {}, {} · {}",
                MONTHS.get(tm.tm_mon as usize).copied().unwrap_or("Jan"),
                tm.tm_mday,
                1900 + tm.tm_year,
                clock_label(&tm)
            );
        }
    }
    format!("{secs}")
}

/// Compact messaging-app timestamp: `now`, `12m`, `3h`, `Yesterday`,
/// `Mon`, or a short date once the message is more than a week old.
fn format_bot_room_time(last_message_at: i64) -> String {
    if last_message_at <= 0 {
        return String::new();
    }
    let now_ms = crate::state::AppState::now_ms();
    let then_ms = if last_message_at > 10_000_000_000 {
        last_message_at
    } else {
        last_message_at.saturating_mul(1000)
    };
    let delta = now_ms.saturating_sub(then_ms);
    if delta < 60_000 {
        return "now".to_string();
    }
    if delta < 3_600_000 {
        return format!("{}m", delta / 60_000);
    }
    if delta < 86_400_000 {
        return format!("{}h", delta / 3_600_000);
    }
    #[cfg(unix)]
    {
        let secs = (then_ms / 1000) as libc::time_t;
        let now_secs = (now_ms / 1000) as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let mut now_tm: libc::tm = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            !libc::localtime_r(&secs, &mut tm).is_null()
                && !libc::localtime_r(&now_secs, &mut now_tm).is_null()
        };
        if ok {
            let month = MONTHS.get(tm.tm_mon as usize).copied().unwrap_or("Jan");
            if tm.tm_year == now_tm.tm_year && tm.tm_yday == now_tm.tm_yday {
                return clock_label(&tm);
            }
            let days = now_tm.tm_yday - tm.tm_yday;
            if tm.tm_year == now_tm.tm_year {
                if days == 1 {
                    return "Yesterday".to_string();
                }
                if (2..7).contains(&days) {
                    return WEEKDAYS
                        .get(tm.tm_wday as usize)
                        .copied()
                        .unwrap_or("")
                        .to_string();
                }
                return format!("{month} {}", tm.tm_mday);
            }
            return format!("{month} {}, {}", tm.tm_mday, 1900 + tm.tm_year);
        }
    }
    format!("{}d", delta / 86_400_000)
}

#[cfg(unix)]
fn clock_label(tm: &libc::tm) -> String {
    let h24 = tm.tm_hour;
    let ampm = if h24 >= 12 { "PM" } else { "AM" };
    let h12 = match h24 % 12 {
        0 => 12,
        h => h,
    };
    format!("{h12}:{:02} {ampm}", tm.tm_min)
}

/// First non-empty line of a message, capped so a pasted wall of text cannot
/// bloat the room row's text layout.
fn bot_preview_line(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let mut out = String::new();
    let mut chars = 0usize;
    for ch in line.chars() {
        if chars >= 120 {
            out.push('…');
            break;
        }
        out.push(ch);
        chars += 1;
    }
    // The engine clips the registry preview at 80 chars without an ellipsis.
    if chars >= 80 && !out.ends_with('…') {
        out.push('…');
    }
    out
}

fn bot_room_rank(status: DisplayStatus) -> u8 {
    match status {
        DisplayStatus::AwaitingInput => 0,
        DisplayStatus::Running => 1,
        DisplayStatus::Aborted => 2,
        _ => 3,
    }
}

impl KeelWindow {
    /// The bot owning the currently open chat, if the active chat is a bot DM.
    pub(super) fn active_bot(&self) -> Option<&Bot> {
        let chat_id = self.current_chat.as_deref()?;
        self.bots.store.by_chat_id(chat_id)
    }

    /// Look up a bot's live display status from its 1:1 DM chat in `self.state`.
    pub(super) fn bot_display_status(&self, bot: &Bot) -> DisplayStatus {
        let Some(cid) = bot.chat_id.as_deref() else {
            return DisplayStatus::Idle;
        };
        let Some(chat) = self.state.chats.get(cid) else {
            return DisplayStatus::Idle;
        };
        if chat.tombstoned {
            return DisplayStatus::Idle;
        }
        keel_proto::display_status(chat.status, chat.last_seen_at, crate::state::AppState::now_ms())
    }

    /// The room row's second line: latest message, else the bot's promise.
    fn bot_room_preview(&self, bot: &Bot) -> String {
        if let Some(chat) = bot
            .chat_id
            .as_deref()
            .and_then(|id| self.state.chats.get(id))
        {
            let preview = bot_preview_line(&chat.last_message_preview);
            if !preview.is_empty() {
                return preview;
            }
        }
        if bot.is_creator() {
            return "Tell Dr. Keelee what coworker you need — role, voice, and guardrails."
                .to_string();
        }
        if !bot.tagline.trim().is_empty() {
            return bot.tagline.clone();
        }
        "No messages yet — say hello.".to_string()
    }

    /// Compact timestamp for the room's right rail.
    fn bot_room_time(&self, bot: &Bot) -> String {
        let at = bot
            .chat_id
            .as_deref()
            .and_then(|id| self.state.chats.get(id))
            .map(|chat| chat.last_message_at)
            .unwrap_or(0);
        if at > 0 {
            format_bot_room_time(at)
        } else if bot.is_creator() {
            "Pinned".to_string()
        } else {
            "New".to_string()
        }
    }

    /// Unread alerts for the bot's DM, matching the sidebar's badge setting.
    fn bot_unread_mark(&self, bot: &Bot, t: Tokens) -> Option<AnyElement> {
        use crate::notify::{NotifyEvent, RowBadge};
        let cid = bot.chat_id.as_deref()?;
        let badge = self
            .unread
            .row_badge(cid, self.state.ui_settings.notifications.badge)?;
        let color = match self.unread.latest(cid) {
            Some(NotifyEvent::NeedsInput) => t.warning,
            Some(NotifyEvent::Error) => t.danger,
            _ => t.accent,
        };
        Some(match badge {
            RowBadge::Dot => div()
                .size(zpx(7.))
                .flex_none()
                .rounded_full()
                .bg(c(color))
                .into_any_element(),
            RowBadge::Count(n) => div()
                .min_w(zpx(17.))
                .h(zpx(17.))
                .px(zpx(4.))
                .flex_none()
                .rounded_full()
                .bg(c(color))
                .flex()
                .items_center()
                .justify_center()
                .font_family(MONO)
                .text_size(zpx(10.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(c(t.text_on_accent))
                .child(n.to_string())
                .into_any_element(),
        })
    }

    /// Open the full-pane Bots directory page.
    pub(super) fn open_bots_page(&mut self, cx: &mut Context<Self>) {
        self.close_menus(cx);
        self.close_pages();
        self.finish_settings_exit(cx);
        self.bots.page_open = true;
        self.bots.card_menu_bot = None;
        self.bots.card_menu_owner = None;
        self.bots
            .page_scroll
            .set_offset(gpui::point(gpui::px(0.), gpui::px(0.)));
        cx.notify();
    }

    /// Open a bot's DM and prefill + focus the composer with a starter prompt.
    pub(super) fn open_bot_with_prompt(
        &mut self,
        bot_id: &str,
        prompt: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_bot(bot_id, cx);
        self.composer.update(cx, |s, cx| {
            s.set_value(&prompt, window, cx);
            let end = s.value().len();
            s.set_selected_range(end..end, cx);
            s.focus(window, cx);
        });
    }

    /// Open a bot's continuous 1:1 DM thread (creating its backing scratch chat lazily if needed).
    pub(super) fn open_bot(&mut self, bot_id: &str, cx: &mut Context<Self>) {
        self.close_menus(cx);
        self.close_pages();
        self.finish_settings_exit(cx);

        if let Some(bot) = self.bots.store.by_id(bot_id).cloned()
            && let Some(cid) = bot.chat_id
            && self
                .state
                .chats
                .get(&cid)
                .is_some_and(|c| !c.tombstoned && !c.archived)
        {
            self.state.open_or_focus_tab(&cid);
            cx.notify();
            return;
        }

        let Some(client) = self.client.clone() else {
            return;
        };
        let id = bot_id.to_string();
        let harness = self.active_harness_id().to_string();
        let model = self.active_model_id().to_string();
        cx.spawn(async move |this, cx| {
            let res = client
                .call(
                    "bots_ensure_chat",
                    serde_json::json!({
                        "id": id,
                        "harness": harness,
                        "model": model,
                        "reset": false,
                    }),
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Ok(val) = res {
                    if let Ok(bot) = serde_json::from_value::<Bot>(val["bot"].clone()) {
                        if let Some(slot) = this.bots.store.by_id_mut(&bot.id) {
                            *slot = bot;
                        }
                    }
                    if let Some(cid) = val["chat_id"].as_str() {
                        this.state.open_or_focus_tab(cid);
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Clear/reset a bot's 1:1 DM thread to a fresh conversation.
    pub(super) fn reset_bot_chat(&mut self, bot_id: &str, cx: &mut Context<Self>) {
        self.bots.show_header_menu = false;
        let Some(client) = self.client.clone() else {
            return;
        };
        let id = bot_id.to_string();
        let harness = self.active_harness_id().to_string();
        let model = self.active_model_id().to_string();
        cx.spawn(async move |this, cx| {
            let res = client
                .call(
                    "bots_ensure_chat",
                    serde_json::json!({
                        "id": id,
                        "harness": harness,
                        "model": model,
                        "reset": true,
                    }),
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Ok(val) = res {
                    if let Ok(bot) = serde_json::from_value::<Bot>(val["bot"].clone()) {
                        if let Some(slot) = this.bots.store.by_id_mut(&bot.id) {
                            *slot = bot;
                        }
                    }
                    if let Some(cid) = val["chat_id"].as_str() {
                        this.state.open_or_focus_tab(cid);
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Open the Bot Customizer modal for `bot_id` (disabled for built-in Dr. Keelee).
    pub(super) fn open_bot_customizer(
        &mut self,
        bot_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(bot) = self.bots.store.by_id(bot_id).cloned() else {
            return;
        };
        if bot.is_creator() {
            return;
        }
        self.close_menus(cx);
        let name_val = bot.name.clone();
        let name_input = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("Bot name");
            input.set_value(name_val, window, cx);
            input
        });
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(42);
        self.bots.customize = Some(CustomizeDraft {
            bot_id: bot.id,
            builtin: bot.builtin,
            tagline: bot.tagline,
            name_input,
            appearance: bot.appearance,
            pop_at: Some(std::time::Instant::now()),
            rand_seed: seed,
        });
        self.bots.customize_gen = self.bots.customize_gen.wrapping_add(1);
        self.bots.customize_open = true;
        self.bots.customize_leaving = None;
        cx.notify();
    }

    /// Close the Bot Customizer modal with snappy exit motion.
    pub(super) fn dismiss_bot_customizer(&mut self, cx: &mut Context<Self>) {
        if self.bots.customize_open {
            self.bots.customize_open = false;
            let m_gen = self.bots.customize_gen;
            self.bots.customize_leaving = Some(m_gen);
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(crate::motion::FAST + std::time::Duration::from_millis(30))
                    .await;
                let _ = this.update(cx, |this, cx| {
                    if this.bots.customize_leaving == Some(m_gen) {
                        this.bots.customize_leaving = None;
                        this.bots.customize = None;
                        cx.notify();
                    }
                });
            })
            .detach();
            cx.notify();
        }
    }

    /// Persist edits from the Bot Customizer modal.
    pub(super) fn save_bot_customizer(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = &self.bots.customize else {
            return;
        };
        let bot_id = draft.bot_id.clone();
        let raw_name = input_value(&draft.name_input, cx);
        let name = raw_name.trim().to_string();
        if name.is_empty() {
            return;
        }
        let appearance = draft.appearance;

        // Optimistic update so the sidebar, header, and chat bubbles reflect changes immediately.
        if let Some(bot) = self.bots.store.by_id_mut(&bot_id) {
            bot.name = name.clone();
            bot.appearance = appearance;
        }
        self.bots.save_local_fallback();

        if let Some(client) = self.client.clone() {
            let params = serde_json::json!({
                "id": bot_id,
                "name": name,
                "appearance": appearance,
            });
            cx.spawn(async move |this, cx| {
                let res = client.call("bots_update", params).await;
                let _ = this.update(cx, |this, cx| {
                    if let Ok(val) = res
                        && let Ok(bot) = serde_json::from_value::<Bot>(val["bot"].clone())
                        && let Some(slot) = this.bots.store.by_id_mut(&bot.id)
                    {
                        *slot = bot;
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        self.dismiss_bot_customizer(cx);
    }

    pub(super) fn ask_delete_bot(&mut self, bot_id: String, cx: &mut Context<Self>) {
        if bot_id == DR_KEELEE_ID {
            return;
        }
        self.bots.show_header_menu = false;
        self.bots.delete_gen = self.bots.delete_gen.wrapping_add(1);
        self.bots.confirming_delete = Some(bot_id);
        self.bots.delete_leaving = None;
        cx.notify();
    }

    pub(super) fn dismiss_delete_bot(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.bots.confirming_delete.take() {
            let del_gen = self.bots.delete_gen;
            self.bots.delete_leaving = Some((del_gen, id));
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(crate::motion::FAST + std::time::Duration::from_millis(30))
                    .await;
                let _ = this.update(cx, |this, cx| {
                    if this
                        .bots
                        .delete_leaving
                        .as_ref()
                        .is_some_and(|(g, _)| *g == del_gen)
                    {
                        this.bots.delete_leaving = None;
                        cx.notify();
                    }
                });
            })
            .detach();
            cx.notify();
        }
    }

    fn commit_delete_bot(&mut self, bot_id: String, cx: &mut Context<Self>) {
        if bot_id == DR_KEELEE_ID {
            return;
        }
        self.dismiss_delete_bot(cx);
        let removed_chat = self
            .bots
            .store
            .by_id(&bot_id)
            .and_then(|b| b.chat_id.clone());
        self.bots.store.bots.retain(|b| b.id != bot_id);
        if let Some(cid) = &removed_chat
            && self.current_chat.as_deref() == Some(cid.as_str())
        {
            self.state.close_tab(cid);
            self.current_chat = None;
        }
        if let Some(client) = self.client.clone() {
            cx.spawn(async move |_this, _cx| {
                let _ = client
                    .call("bots_delete", serde_json::json!({ "id": bot_id }))
                    .await;
            })
            .detach();
        }
        cx.notify();
    }

    /// Send a starter message directly into the currently active bot DM.
    pub(super) fn send_bot_starter(&mut self, text: String, cx: &mut Context<Self>) {
        if let Some(chat_id) = self.current_chat.clone() {
            self.send_text(&chat_id, text, false, true, cx);
            cx.notify();
        }
    }

    /// Messaging-app contact header for an active bot DM (replaces regular workspace header).
    pub(super) fn render_bot_header(
        &mut self,
        bot: Bot,
        t: Tokens,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = if self.sidebar_toggling() {
            self.sidebar_open_now()
        } else if self.state.ui_settings.sidebar_collapsed {
            0.0
        } else {
            1.0
        };
        let collapsed_pl = f32::from(self.traffic_lights_inset() + zpx(38.));
        let expanded_pl = f32::from(zpx(14.));
        let left_pad = (1.0 - open) * collapsed_pl + open * expanded_pl;

        // Right sidebar and its toggle trigger are hidden on Bot DMs.
        let right_pad = f32::from(zpx(14.));

        // Align the left edge of the bot creature avatar with `col_left` and the right edge
        // of the `...` button with `col_right` (symmetrically matching the centered composer
        // and user/bot chat bubbles), while leaving room on the left for the "← Bots" back pill.
        let pane_w = self.pane_width(window).unwrap_or(1100.);
        let col_w = f32::from(self.column_max()).min(pane_w);
        let col_inset = (pane_w - col_w) * 0.5 + f32::from(zpx(16.));
        let gap_px = f32::from(zpx(10.)); // `gap_2p5` between spacer and adjacent child
        let back_reserve = f32::from(zpx(28.)) + gap_px;
        let bar_spacer = (col_inset - left_pad - back_reserve - gap_px).max(0.);
        let bar_right_spacer = (col_inset - right_pad - gap_px).max(0.);

        let status = self.bot_display_status(&bot);
        let working = status == DisplayStatus::Running;
        let awaiting = status == DisplayStatus::AwaitingInput;
        let reduced = self.reduced();
        let accent = c(bot.appearance.color.accent_rgba());

        let status_text: String = if working {
            "Working…".to_string()
        } else if awaiting {
            "Waiting for your reply".to_string()
        } else if !bot.tagline.is_empty() {
            bot.tagline.clone()
        } else {
            "Online".to_string()
        };

        let bot_id_info = bot.id.clone();
        let bot_id_cust = bot.id.clone();
        let bot_id_reset = bot.id.clone();
        let bot_id_del = bot.id.clone();
        let is_creator = bot.is_creator();
        let menu_open = self.bots.show_header_menu;

        let more_menu = self
            .menu_phase("bot-header-menu", menu_open, cx)
            .map(|leaving| {
                let mut items: Vec<AnyElement> = vec![
                    menu_action("bot-menu-info", Icon::Info.el(zpx(14.)), false, t)
                        .child("Info")
                        .on_click(cx.listener({
                            let id = bot_id_info.clone();
                            move |this, _: &ClickEvent, _, cx| {
                                this.bots.show_header_menu = false;
                                this.open_bot_info(&id, cx);
                            }
                        }))
                        .into_any_element(),
                ];
                if !is_creator {
                    items.push(
                        menu_action(
                            "bot-menu-customize",
                            Icon::PenLine.el(zpx(14.)),
                            false,
                            t,
                        )
                        .child("Customize")
                        .on_click(cx.listener({
                            let id = bot_id_cust.clone();
                            move |this, _: &ClickEvent, window, cx| {
                                this.bots.show_header_menu = false;
                                this.open_bot_customizer(&id, window, cx);
                            }
                        }))
                        .into_any_element(),
                    );
                }
                items.push(
                    menu_action(
                        "bot-menu-reset",
                        Icon::Reset.el(zpx(14.)),
                        false,
                        t,
                    )
                    .child("Clear conversation")
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.reset_bot_chat(&bot_id_reset, cx);
                    }))
                    .into_any_element(),
                );
                if !is_creator {
                    items.push(menu_separator(t).into_any_element());
                    items.push(
                        menu_action("bot-menu-delete", Icon::Trash.el(zpx(14.)), true, t)
                            .child("Retire bot")
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.ask_delete_bot(bot_id_del.clone(), cx);
                            }))
                            .into_any_element(),
                    );
                }
                let dismiss = cx.listener(|this, _: &gpui::MouseDownEvent, _, cx| {
                    this.note_menu_dismissed("bot-header-menu");
                    this.bots.show_header_menu = false;
                    cx.notify();
                });
                sidebar_menu_at(
                    "bot-header-menu",
                    t,
                    188.,
                    -164.,
                    32.,
                    leaving,
                    reduced,
                    dismiss,
                    items,
                )
            });

        div()
            .id("bot-dm-header")
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(crate::style::HEADER_HEIGHT)
            .occlude()
            .flex()
            .flex_row()
            .items_center()
            .pl(px(left_pad))
            .pr(px(right_pad))
            .gap_2p5()
            .bg(c(t.floating))
            .border_b_1()
            .border_color(c(t.border).opacity(0.72))
            .child(
                icon_button("bot-header-back-btn", Icon::Back, t)
                    .tooltip(move |_, cx| {
                        cx.new(|_| crate::view::dotify_ui::Tip::new("Back to Bots", t))
                            .into()
                    })
                    .tooltip_show_delay(crate::view::tooltip::delay())
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.open_bots_page(cx);
                    })),
            )
            .when(bar_spacer > 1., |d| {
                d.child(
                    drag_region("bot-header-drag-left")
                        .flex_none()
                        .w(px(bar_spacer)),
                )
            })
            .child(
                // Avatar with presence indicator dot.
                div()
                    .id("bot-header-avatar-btn")
                    .relative()
                    .flex_none()
                    .when(!is_creator, |d| {
                        d.cursor_pointer()
                            .hover(|s| s.opacity(0.92))
                            .active(|s| s.opacity(0.8))
                            .on_click(cx.listener({
                                let id = bot.id.clone();
                                move |this, _: &ClickEvent, window, cx| {
                                    this.open_bot_customizer(&id, window, cx);
                                }
                            }))
                    })
                    .child(render_creature_avatar(
                        &bot.id,
                        bot.appearance,
                        is_creator,
                        if working {
                            BotMood::Working
                        } else if awaiting {
                            BotMood::Awaiting
                        } else {
                            BotMood::Idle
                        },
                        44.,
                        self.clock_epoch,
                        t,
                        reduced,
                    )),
            )
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(zpx(1.))
                    .child(
                        div()
                            .text_size(zpx(13.5))
                            .line_height(zpx(17.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(c(t.text))
                            .whitespace_nowrap()
                            .child(bot.name.clone()),
                    )
                    .child(
                        div()
                            .t_caption()
                            .text_color(if working {
                                accent
                            } else if awaiting {
                                c(t.warning)
                            } else {
                                c(t.text_muted)
                            })
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(status_text),
                    ),
            )
            .child(drag_region("bot-header-drag"))
            .child(
                div()
                    .relative()
                    .flex_none()
                    .child(
                        icon_button("bot-header-more-btn", Icon::Ellipsis, t)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                if this.menu_just_dismissed("bot-header-menu") {
                                    return;
                                }
                                this.bots.show_header_menu = !this.bots.show_header_menu;
                                cx.notify();
                            })),
                    )
                    .children(more_menu),
            )
            .when(bar_right_spacer > 1., |d| {
                d.child(
                    drag_region("bot-header-drag-right")
                        .flex_none()
                        .w(px(bar_right_spacer)),
                )
            })
            .into_any_element()
    }

    /// Messaging-app empty DM welcome view shown when a bot conversation has no messages yet.
    pub(super) fn render_bot_welcome(
        &mut self,
        bot: Bot,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let accent = c(bot.appearance.color.accent_rgba());
        let reduced = self.reduced();
        let is_creator = bot.is_creator();
        let is_naked = bot.appearance == BotAppearance::naked_default();
        let bot_id = bot.id.clone();

        let starters: Vec<(&'static str, &'static str)> = if is_creator {
            vec![
                (
                    "Build a PR reviewer bot",
                    "make me a sharp PR reviewer bot that catches bugs, edge cases, and over-engineering — terse voice, never bikesheds formatting",
                ),
                (
                    "Make a daily briefing bot",
                    "hatch a morning briefing bot that summarizes my tasks and open threads in 5 tight bullets, zero fluff",
                ),
                (
                    "Audit my bot fleet",
                    "audit my current bots and tell me what needs tightening or retiring",
                ),
            ]
        } else {
            vec![
                ("Say hi & check scope", "hey! introduce yourself in one sentence and tell me what you're built for"),
                ("Start a quick task", "ready to work — ask me what we're tackling first"),
            ]
        };

        let card = div()
            .w(zpx(460.))
            .max_w(relative(1.0))
            .flex()
            .flex_col()
            .items_center()
            .gap_4()
                    // Frameless 3D creature portrait (no circular background).
                    .child(
                        div()
                            .id("bot-welcome-portrait")
                            .size(zpx(148.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(!is_creator, |d| {
                                d.cursor_pointer()
                                    .hover(|s| s.opacity(0.92))
                                    .active(|s| s.opacity(0.84))
                                    .on_click(cx.listener({
                                        let id = bot_id.clone();
                                        move |this, _: &ClickEvent, window, cx| {
                                            this.open_bot_customizer(&id, window, cx);
                                        }
                                    }))
                            })
                            .child(render_creature_stage(
                                &bot.id,
                                bot.appearance,
                                is_creator,
                                BotMood::Idle,
                                144.,
                                self.clock_epoch,
                                None,
                                t,
                                reduced,
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(zpx(19.))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(c(t.text))
                                    .child(bot.name.clone()),
                            )
                            .when(!bot.tagline.is_empty(), |d| {
                                d.child(
                                    div()
                                        .t_body()
                                        .text_color(c(t.text_muted))
                                        .text_center()
                                        .child(bot.tagline.clone()),
                                )
                            })
                            .when(is_creator, |d| {
                                d.child(
                                    div()
                                        .pt_0p5()
                                        .t_small()
                                        .text_color(c(t.text_faint))
                                        .text_center()
                                        .child("Tell Dr. Keelee what coworker you need — all new bots hatch here."),
                                )
                            }),
                    )
                    // Naked creature nudge banner for freshly hatched bots.
                    .when(!is_creator && is_naked, |d| {
                        d.child(
                            div()
                                .id("bot-welcome-dress-btn")
                                .px_3()
                                .py_1p5()
                                .rounded_full()
                                .bg(accent.opacity(0.14))
                                .border_1()
                                .border_color(accent.opacity(0.32))
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_1p5()
                                .cursor_pointer()
                                .t_small()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(accent)
                                .hover(|s| s.bg(accent.opacity(0.2)))
                                .active(|s| s.opacity(0.8))
                                .child(Icon::SparkleFilled.el(zpx(12.)))
                                .child("Freshly hatched · Dress up this creature")
                                .on_click(cx.listener({
                                    let id = bot_id.clone();
                                    move |this, _: &ClickEvent, window, cx| {
                                        this.open_bot_customizer(&id, window, cx);
                                    }
                                })),
                        )
                    })
                    // Anti-jobs chips for created bots.
                    .when(!is_creator && !bot.anti_jobs.is_empty(), |d| {
                        d.child(
                            div()
                                .w_full()
                                .p_3()
                                .rounded(RADIUS_LG)
                                .bg(c(t.surface))
                                .border_1()
                                .border_color(c(t.border).opacity(0.75))
                                .flex()
                                .flex_col()
                                .gap_1p5()
                                .child(
                                    div()
                                        .t_caption()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(c(t.text_muted))
                                        .child("GUARDRAILS (ANTI-JOBS)"),
                                )
                                .children(bot.anti_jobs.iter().take(4).map(|rule| {
                                    div()
                                        .flex()
                                        .flex_row()
                                        .items_start()
                                        .gap_1p5()
                                        .t_small()
                                        .text_color(c(t.text_muted))
                                        .child(
                                            div()
                                                .text_color(c(t.text_faint))
                                                .child("•"),
                                        )
                                        .child(rule.clone())
                                })),
                        )
                    })
                    // Starter message pills.
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .justify_center()
                            .gap_2()
                            .pt_1()
                            .children(starters.into_iter().enumerate().map(|(ix, (label, prompt))| {
                                let prompt = prompt.to_string();
                                div()
                                    .id(ElementId::Name(format!("bot-starter-{ix}").into()))
                                    .px_3()
                                    .py_1p5()
                                    .rounded_full()
                                    .bg(c(t.surface))
                                    .border_1()
                                    .border_color(c(t.border))
                                    .cursor_pointer()
                                    .t_small()
                                    .text_color(c(t.text))
                                    .hover_fade(c(t.hover), Some(c(t.selected)))
                                    .active(|s| s.opacity(0.8))
                                    .child(label)
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.send_bot_starter(prompt.clone(), cx);
                                    }))
                            })),
                    );

        div()
            .id("bot-welcome-stage")
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .pb(zpx(120.))
            .pt(zpx(64.))
            .px_6()
            .child(crate::motion::appear(
                card,
                ElementId::Name(format!("bot-welcome-in-{}", bot.id).into()),
                crate::motion::BASE,
                6.,
                reduced,
            ))
            .into_any_element()
    }

    /// Split-studio Bot Customizer modal: live creature stage on the left, trait pickers on the right.
    pub(super) fn render_bot_customize_modal(
        &mut self,
        t: Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (leaving, modal_gen) = match (self.bots.customize_open, self.bots.customize_leaving) {
            (true, _) => (false, self.bots.customize_gen),
            (false, Some(n)) => (true, n),
            _ => return None,
        };
        let draft = self.bots.customize.as_ref()?;
        let bot_id_stage = draft.bot_id.clone();
        let name_input = draft.name_input.clone();
        let appearance = draft.appearance;
        let builtin = draft.builtin;
        let tagline = draft.tagline.clone();
        let pop_at = draft.pop_at;

        name_input.update(cx, |s, _cx| {
            s.set_editor_style(super::editor_style(t));
        });

        let reduced = self.reduced();
        let live_name = input_value(&name_input, cx);
        let display_name = if live_name.trim().is_empty() {
            "Unnamed Bot".to_string()
        } else {
            live_name.trim().to_string()
        };
        let can_save = !live_name.trim().is_empty();
        let accent = c(appearance.color.accent_rgba());

        // Helper for tactile option chips.
        let chip = |id: String, label: &'static str, selected: bool| {
            div()
                .id(ElementId::Name(id.into()))
                .h(zpx(28.))
                .px_2p5()
                .rounded(RADIUS_MD)
                .border_1()
                .border_color(if selected { accent } else { c(t.border) })
                .bg(if selected {
                    accent.opacity(0.16)
                } else {
                    c(t.surface)
                })
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .t_small()
                .font_weight(if selected {
                    gpui::FontWeight::SEMIBOLD
                } else {
                    gpui::FontWeight::NORMAL
                })
                .text_color(if selected { accent } else { c(t.text) })
                .when(!selected, |d| {
                    d.hover(|s| s.border_color(c(t.text_faint)))
                        .hover_fade(c(t.hover), None)
                })
                .active(|s| s.opacity(0.78))
                .child(label)
        };

        let section_label = |label: &'static str| {
            div()
                .t_caption()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(c(t.text_muted))
                .child(label)
        };

        // Left: Live Creature Studio Stage.
        let left_stage = div()
            .w(zpx(236.))
            .flex_none()
            .p_4()
            .rounded(RADIUS_LG)
            .bg(accent.opacity(0.08))
            .border_1()
            .border_color(accent.opacity(0.24))
            .flex()
            .flex_col()
            .items_center()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .px_2()
                            .py(zpx(2.))
                            .rounded_full()
                            .bg(c(t.floating).opacity(0.85))
                            .border_1()
                            .border_color(c(t.border).opacity(0.6))
                            .t_caption()
                            .text_color(c(t.text_muted))
                            .child(appearance.size.label()),
                    )
                    .child(
                        div()
                            .px_2()
                            .py(zpx(2.))
                            .rounded_full()
                            .bg(accent.opacity(0.18))
                            .t_caption()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(accent)
                            .child(appearance.color.label()),
                    ),
            )
            .child(
                div()
                    .py_2()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(render_creature_stage(
                        &bot_id_stage,
                        appearance,
                        builtin,
                        BotMood::Idle,
                        196.,
                        self.clock_epoch,
                        pop_at,
                        t,
                        reduced,
                    )),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_0p5()
                    .child(
                        div()
                            .text_size(zpx(15.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(c(t.text))
                            .text_center()
                            .child(display_name),
                    )
                    .when(!tagline.is_empty(), |d| {
                        d.child(
                            div()
                                .t_caption()
                                .text_color(c(t.text_muted))
                                .text_center()
                                .child(tagline),
                        )
                    }),
            )
            .child(
                div()
                    .w_full()
                    .pt_1()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_center()
                    .gap_1p5()
                    .child(
                        div()
                            .id("bot-cust-randomize")
                            .flex_1()
                            .h(zpx(28.))
                            .rounded(RADIUS_MD)
                            .bg(c(t.floating))
                            .border_1()
                            .border_color(c(t.border))
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_center()
                            .gap_1p5()
                            .cursor_pointer()
                            .t_caption()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(c(t.text))
                            .hover_fade(c(t.hover), Some(c(t.selected)))
                            .active(|s| s.opacity(0.78))
                            .child(Icon::Shuffle.el(zpx(12.)))
                            .child("Randomize")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                if let Some(d) = &mut this.bots.customize {
                                    d.rand_seed = d.rand_seed.wrapping_add(1);
                                    d.appearance = random_appearance(d.rand_seed);
                                    d.pop_at = Some(std::time::Instant::now());
                                    cx.notify();
                                }
                            })),
                    )
                    .child(
                        div()
                            .id("bot-cust-reset")
                            .flex_1()
                            .h(zpx(28.))
                            .rounded(RADIUS_MD)
                            .bg(c(t.floating))
                            .border_1()
                            .border_color(c(t.border))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .t_caption()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(c(t.text_muted))
                            .hover(|s| s.text_color(c(t.text)))
                            .hover_fade(c(t.hover), Some(c(t.selected)))
                            .active(|s| s.opacity(0.78))
                            .child(if builtin { "Lab default" } else { "Naked" })
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                if let Some(d) = &mut this.bots.customize {
                                    d.appearance = if d.builtin {
                                        BotAppearance::dr_keelee_default()
                                    } else {
                                        BotAppearance::naked_default()
                                    };
                                    d.pop_at = Some(std::time::Instant::now());
                                    cx.notify();
                                }
                            })),
                    ),
            );

        // Right: Name + Swatches + Size + Attire + Eyes + Hair + Mustache.
        let name_focused =
            gpui::Focusable::focus_handle(name_input.read(cx), cx).is_focused(window);
        let name_section = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(section_label("NAME"))
            .child(
                div()
                    .h(zpx(32.))
                    .px_2p5()
                    .rounded(RADIUS_MD)
                    .border_1()
                    .border_color(c(if name_focused { t.accent } else { t.border }))
                    .bg(c(t.surface))
                    .flex()
                    .items_center()
                    .child(div().w_full().child(name_input.clone())),
            );

        let color_section = div()
            .flex()
            .flex_col()
            .gap_1p5()
            .child(section_label("COLOR"))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap_2()
                    .children(BotColor::ALL.iter().copied().map(|col| {
                        let selected = appearance.color == col;
                        let col_accent = c(col.accent_rgba());
                        div()
                            .id(ElementId::Name(format!("bot-col-{:?}", col).into()))
                            .size(zpx(28.))
                            .rounded_full()
                            .p(zpx(2.))
                            .border_2()
                            .border_color(if selected {
                                col_accent
                            } else {
                                gpui::transparent_black()
                            })
                            .cursor_pointer()
                            .flex()
                            .items_center()
                            .justify_center()
                            .active(|s| s.opacity(0.78))
                            .child(
                                div()
                                    .size_full()
                                    .rounded_full()
                                    .bg(col_accent)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .when(selected, |d| {
                                        d.text_color(gpui::white())
                                            .child(Icon::Check.el(zpx(12.)))
                                    }),
                            )
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                if let Some(d) = &mut this.bots.customize {
                                    d.appearance.color = col;
                                    d.pop_at = Some(std::time::Instant::now());
                                    cx.notify();
                                }
                            }))
                    })),
            );

        let size_section = div()
            .flex()
            .flex_col()
            .gap_1p5()
            .child(section_label("SIZE"))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_1p5()
                    .children(BotSize::ALL.iter().copied().map(|sz| {
                        chip(format!("bot-sz-{:?}", sz), sz.label(), appearance.size == sz)
                            .flex_1()
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                if let Some(d) = &mut this.bots.customize {
                                    d.appearance.size = sz;
                                    d.pop_at = Some(std::time::Instant::now());
                                    cx.notify();
                                }
                            }))
                    })),
            );

        let attire_section = div()
            .flex()
            .flex_col()
            .gap_1p5()
            .child(section_label("ATTIRE"))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap_1p5()
                    .children(BotAttire::ALL.iter().copied().map(|att| {
                        chip(
                            format!("bot-att-{:?}", att),
                            att.label(),
                            appearance.attire == att,
                        )
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            if let Some(d) = &mut this.bots.customize {
                                d.appearance.attire = att;
                                d.pop_at = Some(std::time::Instant::now());
                                cx.notify();
                            }
                        }))
                    })),
            );

        let eyes_section = div()
            .flex()
            .flex_col()
            .gap_1p5()
            .child(section_label("EYES"))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap_1p5()
                    .children(BotEyes::ALL.iter().copied().map(|ey| {
                        chip(format!("bot-eye-{:?}", ey), ey.label(), appearance.eyes == ey)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                if let Some(d) = &mut this.bots.customize {
                                    d.appearance.eyes = ey;
                                    d.pop_at = Some(std::time::Instant::now());
                                    cx.notify();
                                }
                            }))
                    })),
            );

        let hair_section = div()
            .flex()
            .flex_col()
            .gap_1p5()
            .child(section_label("HAIR"))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap_1p5()
                    .children(BotHair::ALL.iter().copied().map(|hr| {
                        chip(format!("bot-hair-{:?}", hr), hr.label(), appearance.hair == hr)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                if let Some(d) = &mut this.bots.customize {
                                    d.appearance.hair = hr;
                                    d.pop_at = Some(std::time::Instant::now());
                                    cx.notify();
                                }
                            }))
                    })),
            );

        let mustache_section = div()
            .flex()
            .flex_col()
            .gap_1p5()
            .child(section_label("MUSTACHE"))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap_1p5()
                    .children(BotMustache::ALL.iter().copied().map(|ms| {
                        chip(
                            format!("bot-stache-{:?}", ms),
                            ms.label(),
                            appearance.mustache == ms,
                        )
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            if let Some(d) = &mut this.bots.customize {
                                d.appearance.mustache = ms;
                                d.pop_at = Some(std::time::Instant::now());
                                cx.notify();
                            }
                        }))
                    })),
            );

        let right_controls = div()
            .id("bot-cust-scroll")
            .flex_1()
            .min_w_0()
            .max_h(zpx(420.))
            .overflow_y_scroll()
            .pr_1()
            .flex()
            .flex_col()
            .gap_3p5()
            .child(name_section)
            .child(color_section)
            .child(size_section)
            .child(attire_section)
            .child(eyes_section)
            .child(hair_section)
            .child(mustache_section);

        let header = div()
            .flex()
            .flex_row()
            .items_start()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_size(zpx(15.5))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(c(t.text))
                            .child("Customize Creature"),
                    )
                    .child(
                        div()
                            .t_small()
                            .text_color(c(t.text_muted))
                            .child("Tune name, colors, attire, eyes, hair, mustache, and size."),
                    ),
            )
            .child(
                div()
                    .id("bot-cust-close-x")
                    .size(zpx(24.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(RADIUS_SM)
                    .text_color(c(t.text_faint))
                    .cursor_pointer()
                    .hover(|s| s.text_color(c(t.text)))
                    .hover_fade(c(t.hover), Some(c(t.selected)))
                    .active(|s| s.opacity(0.7))
                    .child(Icon::Close.el(zpx(12.)))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.dismiss_bot_customizer(cx);
                    })),
            );

        let cancel_btn = text_button("bot-cust-cancel", "Cancel", t)
            .h(zpx(30.))
            .px_3()
            .py_0()
            .flex()
            .items_center()
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.dismiss_bot_customizer(cx);
            }));

        let save_base = div()
            .id("bot-cust-save")
            .h(zpx(30.))
            .px_3p5()
            .rounded(RADIUS_MD)
            .flex()
            .items_center()
            .justify_center()
            .t_small()
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .child("Save changes");

        let save_btn = if can_save {
            save_base
                .bg(accent)
                .text_color(gpui::white())
                .cursor_pointer()
                .hover(|s| s.opacity(0.92))
                .active(|s| s.opacity(0.78))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.save_bot_customizer(cx);
                }))
        } else {
            save_base.bg(c(t.hover)).text_color(c(t.text_faint))
        };

        let card = div()
            .id(("bot-cust-modal-card", modal_gen as usize))
            .w(zpx(690.))
            .max_w(relative(0.94))
            .p_5()
            .rounded(RADIUS_XL)
            .bg(c(t.floating))
            .border_1()
            .border_color(c(t.border))
            .floating_surface(t)
            .shadow_2xl()
            .flex()
            .flex_col()
            .gap_4()
            .cursor_default()
            .on_click(|_: &ClickEvent, _, cx| cx.stop_propagation())
            .child(header)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_4()
                    .child(left_stage)
                    .child(right_controls),
            )
            .child(
                div()
                    .pt_2()
                    .border_t_1()
                    .border_color(c(t.border).opacity(0.7))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .t_caption()
                            .text_color(c(t.text_faint))
                            .child("Esc to cancel"),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(cancel_btn)
                            .child(save_btn),
                    ),
            );

        let card_motion = if leaving {
            crate::motion::dismiss(
                card,
                ("bot-cust-card-out", modal_gen as usize),
                reduced,
            )
        } else {
            crate::motion::appear(
                card,
                ("bot-cust-card-in", modal_gen as usize),
                crate::motion::BASE,
                14.,
                reduced,
            )
            .into_any_element()
        };

        let scrim = div()
            .id(("bot-cust-scrim", modal_gen as usize))
            .absolute()
            .inset_0()
            .occlude()
            .bg(gpui::rgba(0x00000088))
            .flex()
            .items_center()
            .justify_center()
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.dismiss_bot_customizer(cx);
            }));

        let scrim = if leaving {
            if reduced {
                scrim.child(card_motion).into_any_element()
            } else {
                scrim
                    .child(card_motion)
                    .with_animation(
                        ("bot-cust-scrim-out", modal_gen as usize),
                        gpui::Animation::new(crate::motion::FAST)
                            .with_easing(crate::motion::ease_out),
                        |el, delta| el.opacity(1.0 - delta),
                    )
                    .into_any_element()
            }
        } else if reduced {
            scrim.child(card_motion).into_any_element()
        } else {
            scrim
                .child(card_motion)
                .with_animation(
                    ("bot-cust-scrim-in", modal_gen as usize),
                    gpui::Animation::new(crate::motion::FAST)
                        .with_easing(crate::motion::ease_out),
                    |el, delta| el.opacity(delta),
                )
                .into_any_element()
        };

        Some(scrim)
    }

    /// Delete confirmation modal for retiring a non-builtin bot.
    pub(super) fn render_delete_bot_modal(
        &mut self,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (leaving, del_gen, bot_id) = match (&self.bots.confirming_delete, &self.bots.delete_leaving)
        {
            (Some(id), _) => (false, self.bots.delete_gen, id.clone()),
            (None, Some((g, id))) => (true, *g, id.clone()),
            _ => return None,
        };
        let bot_name = self
            .bots
            .store
            .by_id(&bot_id)
            .map(|b| b.name.clone())
            .unwrap_or_else(|| bot_id.clone());
        let reduced = self.reduced();

        let card = div()
            .id(("bot-delete-card", del_gen as usize))
            .w(zpx(380.))
            .max_w(relative(0.9))
            .p_4()
            .rounded(RADIUS_LG)
            .bg(c(t.floating))
            .border_1()
            .border_color(c(t.border))
            .floating_surface(t)
            .shadow_2xl()
            .flex()
            .flex_col()
            .gap_3()
            .on_click(|_: &ClickEvent, _, cx| cx.stop_propagation())
            .child(
                div()
                    .text_size(zpx(15.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(c(t.text))
                    .child(format!("Retire {bot_name}?")),
            )
            .child(
                div()
                    .t_small()
                    .text_color(c(t.text_muted))
                    .child("This removes the bot and its DM history. You can always ask Dr. Keelee to hatch a new one."),
            )
            .child(
                div()
                    .pt_1()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap_2()
                    .child(
                        text_button("bot-del-cancel", "Cancel", t).on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| {
                                this.dismiss_delete_bot(cx);
                            },
                        )),
                    )
                    .child(
                        filled_button("bot-del-confirm", "Retire bot", t.danger, t).on_click(
                            cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.commit_delete_bot(bot_id.clone(), cx);
                            }),
                        ),
                    ),
            );

        let card_motion = if leaving {
            crate::motion::dismiss(card, ("bot-del-out", del_gen as usize), reduced)
        } else {
            crate::motion::appear(card, ("bot-del-in", del_gen as usize), crate::motion::BASE, 12., reduced)
                .into_any_element()
        };

        Some(
            div()
                .id(("bot-del-scrim", del_gen as usize))
                .absolute()
                .inset_0()
                .occlude()
                .bg(gpui::rgba(0x00000088))
                .flex()
                .items_center()
                .justify_center()
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.dismiss_delete_bot(cx);
                }))
                .child(card_motion)
                .into_any_element(),
        )
    }

    /// Active harness id fallback used when opening a bot's DM chat.
    fn active_harness_id(&self) -> &str {
        if self.selected_harness.is_empty() {
            "keel"
        } else {
            &self.selected_harness
        }
    }

    /// Active model id fallback used when opening a bot's DM chat.
    fn active_model_id(&self) -> &str {
        &self.selected_model
    }

    /// Summary counts across all bots: `(total_coworkers, running_count, awaiting_count)`.
    pub(super) fn bots_fleet_counts(&self) -> (usize, usize, usize) {
        let mut coworkers = 0usize;
        let mut running = 0usize;
        let mut awaiting = 0usize;
        for bot in &self.bots.store.bots {
            if !bot.is_creator() {
                coworkers += 1;
            }
            match self.bot_display_status(bot) {
                DisplayStatus::Running => running += 1,
                DisplayStatus::AwaitingInput => awaiting += 1,
                _ => {}
            }
        }
        (coworkers, running, awaiting)
    }

    fn recent_bot_filter_nav(&self) -> bool {
        !self.reduced()
            && self
                .bots
                .filter_at
                .is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(180))
    }

    fn set_bot_filter(&mut self, next: BotStatusFilter, cx: &mut Context<Self>) {
        if self.bots.filter == next {
            return;
        }
        self.bots.filter = next;
        self.bots.card_menu_bot = None;
        self.bots.filter_gen = self.bots.filter_gen.wrapping_add(1);
        self.bots.filter_at = Some(std::time::Instant::now());
        self.bots
            .page_scroll
            .set_offset(gpui::point(gpui::px(0.), gpui::px(0.)));
        cx.notify();
    }

    /// Full-pane Bots directory page, opened from the top sidebar menu.
    pub(super) fn render_bots_page(&mut self, t: Tokens, cx: &mut Context<Self>) -> AnyElement {
        let (coworkers, running, awaiting) = self.bots_fleet_counts();
        let active_total = running + awaiting;
        let ready_count = coworkers.saturating_sub(active_total);
        let creator = self
            .bots
            .store
            .bots
            .iter()
            .find(|b| b.is_creator())
            .cloned()
            .unwrap_or_else(Bot::dr_keelee);
        let hatched: Vec<Bot> = self
            .bots
            .store
            .bots
            .iter()
            .filter(|b| !b.is_creator())
            .cloned()
            .collect();

        let summary_text = if coworkers == 0 {
            "No coworkers yet".to_string()
        } else if active_total > 0 {
            format!("{active_total} active")
        } else {
            format!(
                "{coworkers} {}",
                if coworkers == 1 {
                    "coworker"
                } else {
                    "coworkers"
                }
            )
        };

        let search = div()
            .w(zpx(220.))
            .h(zpx(INPUT_H))
            .px(zpx(12.))
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(8.))
            .rounded_full()
            .bg(c(t.input))
            .t_small()
            .text_color(c(t.text_muted))
            .child(Icon::Search.el(zpx(13.)).text_color(c(t.text_faint)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(self.bots_filter.clone().into_any_element()),
            );

        let filter = self.bots.filter;
        let show_filters = active_total > 0 || filter != BotStatusFilter::All;
        let make_filter =
            |id: &'static str, label: &'static str, count: usize, target: BotStatusFilter| {
                let selected = filter == target;
                div()
                    .id(id)
                    .h(zpx(24.))
                    .px(zpx(10.))
                    .rounded_full()
                    .when(selected, |d| d.bg(c(t.surface)))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(6.))
                    .cursor_pointer()
                    .t_caption()
                    .font_weight(if selected {
                        gpui::FontWeight::SEMIBOLD
                    } else {
                        gpui::FontWeight::MEDIUM
                    })
                    .text_color(c(if selected { t.text } else { t.text_muted }))
                    .when(!selected, |d| d.hover(|s| s.text_color(c(t.text))))
                    .active(|s| s.opacity(0.72))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.set_bot_filter(target, cx);
                    }))
                    .child(label)
                    .child(
                        div()
                            .t_caption()
                            .text_color(c(if selected { t.text_muted } else { t.text_faint }))
                            .child(count.to_string()),
                    )
            };
        let filter_bar = show_filters.then(|| {
            div()
                .p(zpx(2.))
                .rounded_full()
                .bg(c(t.input))
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(2.))
                .child(make_filter(
                    "bots-filter-all",
                    "All",
                    coworkers,
                    BotStatusFilter::All,
                ))
                .child(make_filter(
                    "bots-filter-active",
                    "Active",
                    active_total,
                    BotStatusFilter::Active,
                ))
                .child(make_filter(
                    "bots-filter-ready",
                    "Ready",
                    ready_count,
                    BotStatusFilter::Ready,
                ))
        });

        let top_bar = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(zpx(8.))
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap(zpx(12.))
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .flex_row()
                            .items_baseline()
                            .gap(zpx(8.))
                            .child(
                                div()
                                    .text_size(zpx(17.))
                                    .line_height(zpx(22.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(c(t.text))
                                    .child("Bots"),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .t_caption()
                                    .text_color(c(t.text_faint))
                                    .child(summary_text),
                            ),
                    )
                    .child(search),
            )
            .children(filter_bar);

        let creator_room = self.render_bot_room(creator, true, t, cx);
        let quick = (coworkers > 0).then(|| self.render_bot_quick_strip(t, cx));
        let directory = self.render_bots_directory(&hatched, t, cx);
        let hairline = || div().h(px(1.)).ml(zpx(64.)).bg(c(t.border).opacity(0.55));

        div()
            .flex_1()
            .min_h_0()
            .w_full()
            .bg(c(t.window))
            .flex()
            .flex_col()
            .items_center()
            .child(
                div()
                    .w_full()
                    .max_w(TRANSCRIPT_MAX_WIDTH)
                    .flex_1()
                    .min_h_0()
                    .px(zpx(COLUMN_PAD))
                    .pt(HEADER_HEIGHT + zpx(2.))
                    .pb(zpx(8.))
                    .flex()
                    .flex_col()
                    .child(top_bar)
                    .child(
                        div()
                            .id("bots-page-scroll")
                            .track_scroll(&self.bots.page_scroll)
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .pt(zpx(12.))
                            .pb(zpx(24.))
                            .child(creator_room)
                            .children(quick)
                            .when(coworkers > 0, |d| {
                                d.child(div().pt(zpx(16.)).child(hairline()))
                            })
                            .child(div().pt(zpx(8.)).child(directory)),
                    ),
            )
            .into_any_element()
    }

    /// One conversation row. `pinned` is Dr. Keelee, kept at the top of the inbox.
    fn render_bot_room(
        &mut self,
        bot: Bot,
        pinned: bool,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let reduced = self.reduced();
        let status = self.bot_display_status(&bot);
        let working = status == DisplayStatus::Running;
        let awaiting = status == DisplayStatus::AwaitingInput;
        let aborted = status == DisplayStatus::Aborted;
        let accent = c(bot.appearance.color.accent_rgba());
        let has_thread = bot.chat_id.is_some();
        let active = has_thread && bot.chat_id.as_deref() == self.current_chat.as_deref();
        let preview = self.bot_room_preview(&bot);
        let time = self.bot_room_time(&bot);
        let unread = self.bot_unread_mark(&bot, t);

        let open_id = bot.id.clone();
        let menu_toggle_id = bot.id.clone();

        let menu_is_open = self.bots.card_menu_bot.as_deref() == Some(bot.id.as_str());
        let is_menu_owner = self.bots.card_menu_owner.as_deref() == Some(bot.id.as_str());
        let card_menu = if pinned {
            None
        } else if menu_is_open || is_menu_owner {
            self.menu_phase("bot-card-menu", menu_is_open, cx)
                .map(|leaving| {
                    let id_info = bot.id.clone();
                    let id_cust = bot.id.clone();
                    let id_reset = bot.id.clone();
                    let id_del = bot.id.clone();
                    let mut items: Vec<AnyElement> = vec![
                        menu_action("bot-card-menu-info", Icon::Info.el(zpx(14.)), false, t)
                            .child("Info")
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.bots.card_menu_bot = None;
                                this.open_bot_info(&id_info, cx);
                            }))
                            .into_any_element(),
                        menu_action("bot-card-menu-cust", Icon::PenLine.el(zpx(14.)), false, t)
                            .child("Customize")
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.bots.card_menu_bot = None;
                                this.open_bot_customizer(&id_cust, window, cx);
                            }))
                            .into_any_element(),
                    ];
                    if has_thread {
                        items.push(
                            menu_action("bot-card-menu-reset", Icon::Reset.el(zpx(14.)), false, t)
                                .child("Clear conversation")
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.bots.card_menu_bot = None;
                                    this.reset_bot_chat(&id_reset, cx);
                                }))
                                .into_any_element(),
                        );
                    }
                    items.push(menu_separator(t).into_any_element());
                    items.push(
                        menu_action("bot-card-menu-retire", Icon::Trash.el(zpx(14.)), true, t)
                            .child("Retire bot")
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.bots.card_menu_bot = None;
                                this.ask_delete_bot(id_del.clone(), cx);
                            }))
                            .into_any_element(),
                    );
                    let dismiss = cx.listener(|this, _: &gpui::MouseDownEvent, _, cx| {
                        this.note_menu_dismissed("bot-card-menu");
                        this.bots.card_menu_bot = None;
                        cx.notify();
                    });
                    sidebar_menu_at(
                        "bot-card-menu",
                        t,
                        184.,
                        -160.,
                        28.,
                        leaving,
                        reduced,
                        dismiss,
                        items,
                    )
                })
        } else {
            None
        };

        let has_unread = unread.is_some();
        let avatar_size = 40.;
        let group = format!("bot-room-{}", bot.id);
        let room_id = format!("bot-room-row-{}", bot.id);
        let presence_color = if working {
            t.accent
        } else if awaiting {
            t.warning
        } else {
            t.danger
        };
        let show_presence = working || awaiting || aborted;

        let row = div()
            .id(ElementId::Name(room_id.into()))
            .group(group.clone())
            .hover_probe(group.clone())
            .relative()
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(12.))
            .px(zpx(12.))
            .py(zpx(10.))
            .min_h(zpx(64.))
            .cursor_pointer()
            .rounded(zpx(8.))
            .when(active, |d| d.bg(c(t.selected)))
            .hover_fade(c(t.hover), None)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.open_bot(&open_id, cx);
            }))
            .child(
                div()
                    .relative()
                    .flex_none()
                    .size(zpx(avatar_size))
                    .rounded_full()
                    .bg(accent.opacity(0.12))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(render_creature_avatar(
                        &bot.id,
                        bot.appearance,
                        bot.is_creator(),
                        if working {
                            BotMood::Working
                        } else if awaiting {
                            BotMood::Awaiting
                        } else {
                            BotMood::Idle
                        },
                        34.,
                        self.clock_epoch,
                        t,
                        reduced,
                    ))
                    .when(show_presence, |d| {
                        d.child(
                            div()
                                .absolute()
                                .bottom(zpx(-1.))
                                .right(zpx(-1.))
                                .size(zpx(12.))
                                .rounded_full()
                                .bg(c(if active { t.selected } else { t.window }))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(div().size(zpx(7.)).rounded_full().bg(c(presence_color))),
                        )
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(zpx(4.))
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
                                    .truncate()
                                    .text_size(zpx(14.5))
                                    .line_height(zpx(20.))
                                    .font_weight(if has_unread {
                                        gpui::FontWeight::SEMIBOLD
                                    } else {
                                        gpui::FontWeight::MEDIUM
                                    })
                                    .text_color(c(t.text))
                                    .child(bot.name.clone()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .t_caption()
                                    .text_color(c(if has_unread { t.accent } else { t.text_faint }))
                                    .child(time),
                            ),
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
                                    .t_small()
                                    .text_color(c(if has_unread {
                                        t.text
                                    } else if has_thread {
                                        t.text_muted
                                    } else {
                                        t.text_faint
                                    }))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(preview),
                            )
                            .children(unread),
                    ),
            )
            .when(!pinned, |d| {
                d.child(
                    div()
                        .absolute()
                        .right(zpx(12.))
                        .bottom(zpx(8.))
                        .child(
                            reveal_on_hover(
                                ElementId::Name(format!("bot-room-more-{}", bot.id).into()),
                                group.clone(),
                                0.,
                                1.,
                                super::settings::icon_button(
                                    format!("bot-room-more-btn-{}", bot.id),
                                    Icon::Ellipsis,
                                    t,
                                )
                                .on_click(cx.listener(
                                    move |this, _: &ClickEvent, _, cx| {
                                        cx.stop_propagation();
                                        if this.menu_just_dismissed("bot-card-menu") {
                                            return;
                                        }
                                        if this.bots.card_menu_bot.as_deref()
                                            == Some(menu_toggle_id.as_str())
                                        {
                                            this.bots.card_menu_bot = None;
                                        } else {
                                            this.bots.card_menu_owner =
                                                Some(menu_toggle_id.clone());
                                            this.bots.card_menu_bot = Some(menu_toggle_id.clone());
                                        }
                                        cx.notify();
                                    },
                                )),
                            )
                            .with_offset(4., 0.),
                        )
                        .children(card_menu),
                )
            });

        row.into_any_element()
    }

    /// One line of hatch prompts under the pinned lab chat. The row itself stays
    /// a conversation; these are the occasional actions, not part of the avatar.
    fn render_bot_quick_strip(&mut self, t: Tokens, cx: &mut Context<Self>) -> AnyElement {
        let recipes: [(&'static str, &'static str); 3] = [
            (
                "Review a PR",
                "Make me a sharp PR reviewer bot that catches bugs, edge cases, and over-engineering — terse voice, never bikesheds formatting.",
            ),
            (
                "Write a briefing",
                "Create a morning briefing bot that summarizes my tasks and open threads in 5 tight bullets, zero fluff.",
            ),
            (
                "Audit the fleet",
                "Audit my current bots and tell me what needs tightening or retiring.",
            ),
        ];
        div()
            .w_full()
            .pl(zpx(64.))
            .pr(zpx(12.))
            .pt(zpx(2.))
            .pb(zpx(4.))
            .flex()
            .flex_row()
            .items_center()
            .flex_wrap()
            .gap(zpx(8.))
            .children(
                recipes
                    .into_iter()
                    .enumerate()
                    .map(|(ix, (label, prompt))| {
                        let prompt = prompt.to_string();
                        div()
                            .id(ElementId::Name(format!("bots-room-recipe-{ix}").into()))
                            .h(zpx(28.))
                            .px(zpx(12.))
                            .rounded_full()
                            .bg(c(t.surface))
                            .flex()
                            .flex_row()
                            .items_center()
                            .cursor_pointer()
                            .t_caption()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(c(t.text_muted))
                            .hover(|s| s.text_color(c(t.text)))
                            .hover_fade(c(t.hover), None)
                            .active(|s| s.opacity(0.72))
                            .child(label)
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.open_bot_with_prompt(DR_KEELEE_ID, prompt.clone(), window, cx);
                            }))
                    }),
            )
            .into_any_element()
    }

    fn render_bots_directory(
        &mut self,
        hatched: &[Bot],
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let query = self.bots_filter.read(cx).value().to_string();
        let needle = query.trim().to_lowercase();
        let filter = self.bots.filter;

        let mut rooms: Vec<(Bot, DisplayStatus, i64)> = hatched
            .iter()
            .filter(|bot| {
                let status = self.bot_display_status(bot);
                let is_active = matches!(
                    status,
                    DisplayStatus::Running | DisplayStatus::AwaitingInput
                );
                match filter {
                    BotStatusFilter::All => {}
                    BotStatusFilter::Active if !is_active => return false,
                    BotStatusFilter::Ready if is_active => return false,
                    _ => {}
                }
                if needle.is_empty() {
                    return true;
                }
                let in_name = bot.name.to_lowercase().contains(&needle);
                let in_tag = bot.tagline.to_lowercase().contains(&needle);
                let in_rules = bot
                    .anti_jobs
                    .iter()
                    .any(|r| r.to_lowercase().contains(&needle));
                let in_preview = bot
                    .chat_id
                    .as_deref()
                    .and_then(|id| self.state.chats.get(id))
                    .is_some_and(|chat| chat.last_message_preview.to_lowercase().contains(&needle));
                in_name || in_tag || in_rules || in_preview
            })
            .map(|bot| {
                let status = self.bot_display_status(bot);
                let at = bot
                    .chat_id
                    .as_deref()
                    .and_then(|id| self.state.chats.get(id))
                    .map(|chat| chat.last_message_at)
                    .unwrap_or(0);
                (bot.clone(), status, at)
            })
            .collect();

        rooms.sort_by(|a, b| {
            bot_room_rank(a.1)
                .cmp(&bot_room_rank(b.1))
                .then_with(|| b.2.cmp(&a.2))
                .then_with(|| a.0.name.to_lowercase().cmp(&b.0.name.to_lowercase()))
        });

        let content: AnyElement = if hatched.is_empty() {
            self.render_bots_empty(t, cx)
        } else if rooms.is_empty() {
            div()
                .w_full()
                .py(zpx(28.))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(zpx(8.))
                .child(
                    div()
                        .t_small()
                        .text_color(c(t.text_muted))
                        .child("No conversations match."),
                )
                .child(
                    div()
                        .id("bots-reset-filter-btn")
                        .h(zpx(26.))
                        .px(zpx(10.))
                        .rounded_full()
                        .bg(c(t.surface))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .t_caption()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(c(t.text))
                        .hover_fade(c(t.hover), None)
                        .active(|s| s.opacity(0.72))
                        .child("Clear search")
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.bots.filter = BotStatusFilter::All;
                            this.bots_filter
                                .update(cx, |s, cx| s.set_value("", window, cx));
                            this.bots.filter_gen = this.bots.filter_gen.wrapping_add(1);
                            this.bots.filter_at = Some(std::time::Instant::now());
                            cx.notify();
                        })),
                )
                .into_any_element()
        } else {
            let mut list = div().w_full().flex().flex_col().gap(zpx(2.));
            for (bot, _, _) in &rooms {
                list = list.child(self.render_bot_room(bot.clone(), false, t, cx));
            }
            let filter_gen = self.bots.filter_gen as usize;
            let filter_anim = self.recent_bot_filter_nav();
            list.with_animation(
                ("bots-grid-filter", filter_gen),
                gpui::Animation::new(std::time::Duration::from_millis(160))
                    .with_easing(crate::motion::ease_out_strong),
                move |el, delta| {
                    if filter_anim { el.opacity(delta) } else { el }
                },
            )
            .into_any_element()
        };

        content
    }

    fn render_bots_empty(&mut self, t: Tokens, cx: &mut Context<Self>) -> AnyElement {
        let starters: [(&'static str, &'static str, &'static str); 3] = [
            (
                "Code Reviewer",
                "Catches bugs and edge cases. Skips formatting debates.",
                "Make me a sharp PR reviewer bot that catches bugs, edge cases, and over-engineering — terse voice, never bikesheds formatting.",
            ),
            (
                "Writing Editor",
                "Tightens docs and changelogs. Keeps your voice.",
                "Create a sharp technical writing editor bot that tightens prose and cuts filler while keeping my voice.",
            ),
            (
                "Release Buddy",
                "Checks notes, migrations, and the ship list.",
                "Build me a release checklist bot that verifies diffs, migrations, and ship readiness with zero fluff.",
            ),
        ];

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(zpx(2.))
            .children(
                starters
                    .into_iter()
                    .enumerate()
                    .map(|(ix, (title, desc, prompt))| {
                        let prompt = prompt.to_string();
                        div()
                            .w_full()
                            .child(
                                div()
                                    .id(ElementId::Name(format!("bots-empty-starter-{ix}").into()))
                                    .w_full()
                                    .min_h(zpx(64.))
                                    .px(zpx(12.))
                                    .py(zpx(10.))
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(zpx(12.))
                                    .cursor_pointer()
                                    .rounded(zpx(8.))
                                    .hover_fade(c(t.hover), None)
                                    .child(
                                        div()
                                            .size(zpx(40.))
                                            .flex_none()
                                            .rounded_full()
                                            .bg(c(t.surface))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .text_color(c(t.text_faint))
                                            .child(Icon::Plus.el(zpx(14.))),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .flex()
                                            .flex_col()
                                            .gap(zpx(4.))
                                            .child(
                                                div()
                                                    .text_size(zpx(14.5))
                                                    .line_height(zpx(20.))
                                                    .font_weight(gpui::FontWeight::MEDIUM)
                                                    .text_color(c(t.text))
                                                    .child(title),
                                            )
                                            .child(
                                                div()
                                                    .t_small()
                                                    .text_color(c(t.text_muted))
                                                    .truncate()
                                                    .child(desc),
                                            ),
                                    )
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, window, cx| {
                                            this.open_bot_with_prompt(
                                                DR_KEELEE_ID,
                                                prompt.clone(),
                                                window,
                                                cx,
                                            );
                                        },
                                    )),
                            )
                    }),
            )
            .into_any_element()
    }

    /// Open the Bot Info modal showing created timestamp, system prompt, and guardrails.
    pub(super) fn open_bot_info(&mut self, bot_id: &str, cx: &mut Context<Self>) {
        if self.bots.store.by_id(bot_id).is_none() {
            return;
        }
        self.close_menus(cx);
        self.bots.info_gen = self.bots.info_gen.wrapping_add(1);
        self.bots.info_bot = Some(bot_id.to_string());
        self.bots.info_leaving = None;
        self.bots.info_copied_at = None;
        self.bots
            .info_scroll
            .set_offset(gpui::point(gpui::px(0.), gpui::px(0.)));
        cx.notify();
    }

    pub(super) fn dismiss_bot_info(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.bots.info_bot.take() {
            let info_gen = self.bots.info_gen;
            self.bots.info_leaving = Some((info_gen, id));
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(crate::motion::FAST + std::time::Duration::from_millis(30))
                    .await;
                let _ = this.update(cx, |this, cx| {
                    if this
                        .bots
                        .info_leaving
                        .as_ref()
                        .is_some_and(|(g, _)| *g == info_gen)
                    {
                        this.bots.info_leaving = None;
                        cx.notify();
                    }
                });
            })
            .detach();
            cx.notify();
        }
    }

    /// Modal displaying a bot's details, creation timestamp, system prompt, and guardrails.
    pub(super) fn render_bot_info_modal(
        &mut self,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (leaving, info_gen, bot_id) = match (&self.bots.info_bot, &self.bots.info_leaving) {
            (Some(id), _) => (false, self.bots.info_gen, id.clone()),
            (None, Some((g, id))) => (true, *g, id.clone()),
            _ => return None,
        };
        let bot = self.bots.store.by_id(&bot_id).cloned()?;
        let reduced = self.reduced();
        let accent = c(bot.appearance.color.accent_rgba());
        let is_creator = bot.is_creator();
        let created_text = format_bot_created_at(bot.created_at, bot.builtin);
        let thread_label = if bot.chat_id.is_some() {
            "Active DM"
        } else {
            "Not started"
        };
        let role_label = if bot.builtin { "Built-in" } else { "Coworker" };
        let prompt_text = if !bot.system_prompt.trim().is_empty() {
            bot.system_prompt.trim().to_string()
        } else if is_creator {
            "Built-in Dr. Keelee lab architect prompt".to_string()
        } else {
            "No custom system prompt.".to_string()
        };
        let has_prompt = !bot.system_prompt.trim().is_empty();
        let copied_recently = self
            .bots
            .info_copied_at
            .is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(1600));
        let prompt_for_copy = bot.system_prompt.trim().to_string();

        let meta_cell = |label: &'static str, value: String, mono: bool| {
            div()
                .flex_1()
                .min_w_0()
                .px(zpx(10.))
                .py(zpx(8.))
                .rounded(r_md())
                .bg(c(t.surface))
                .border_1()
                .border_color(c(t.border).opacity(0.75))
                .flex()
                .flex_col()
                .gap(zpx(2.))
                .child(
                    div()
                        .font_family(MONO)
                        .t_caption()
                        .text_color(c(t.text_faint))
                        .child(label),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .when(mono, |d| d.font_family(MONO))
                        .t_small()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(c(t.text))
                        .child(value),
                )
        };

        let header = div()
            .flex()
            .flex_row()
            .items_start()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(12.))
                    .child(
                        div()
                            .size(zpx(52.))
                            .flex_none()
                            .rounded(r_md())
                            .bg(accent.opacity(0.10))
                            .border_1()
                            .border_color(accent.opacity(0.24))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(render_creature_avatar(
                                &bot.id,
                                bot.appearance,
                                is_creator,
                                BotMood::Idle,
                                44.,
                                self.clock_epoch,
                                t,
                                reduced,
                            )),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(zpx(2.))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(zpx(16.))
                                    .line_height(zpx(20.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(c(t.text))
                                    .child(bot.name.clone()),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .t_small()
                                    .text_color(c(t.text_muted))
                                    .child(if !bot.tagline.is_empty() {
                                        bot.tagline.clone()
                                    } else {
                                        "Specialized Keel coworker".to_string()
                                    }),
                            ),
                    ),
            )
            .child(
                super::settings::icon_button("bot-info-close-x", Icon::Close, t).on_click(
                    cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.dismiss_bot_info(cx);
                    }),
                ),
            );

        let body_scroll = div()
            .id("bot-info-scroll")
            .track_scroll(&self.bots.info_scroll)
            .w_full()
            .max_h(zpx(400.))
            .overflow_y_scroll()
            .pr(zpx(4.))
            .flex()
            .flex_col()
            .gap(zpx(14.))
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .gap(zpx(8.))
                    .child(meta_cell("CREATED", created_text, false))
                    .child(meta_cell("ID", bot.id.clone(), true))
                    .child(meta_cell("TYPE", format!("{role_label} · {thread_label}"), false)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(zpx(6.))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .font_family(MONO)
                                    .t_caption()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(c(t.text_muted))
                                    .child("SYSTEM PROMPT"),
                            )
                            .when(has_prompt, |d| {
                                d.child(
                                    div()
                                        .id("bot-info-copy-prompt")
                                        .h(zpx(22.))
                                        .px(zpx(7.))
                                        .rounded(RADIUS_SM)
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap(zpx(4.))
                                        .cursor_pointer()
                                        .t_caption()
                                        .text_color(c(if copied_recently {
                                            t.accent
                                        } else {
                                            t.text_muted
                                        }))
                                        .hover(|s| s.text_color(c(t.text)))
                                        .hover_fade(c(t.hover), None)
                                        .active(|s| s.opacity(0.75))
                                        .child(
                                            if copied_recently {
                                                Icon::Check
                                            } else {
                                                Icon::Copy
                                            }
                                            .el(zpx(11.)),
                                        )
                                        .child(if copied_recently { "Copied" } else { "Copy" })
                                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                                prompt_for_copy.clone(),
                                            ));
                                            this.bots.info_copied_at =
                                                Some(std::time::Instant::now());
                                            cx.notify();
                                        })),
                                )
                            }),
                    )
                    .child(
                        div()
                            .w_full()
                            .p(zpx(12.))
                            .rounded(r_md())
                            .bg(c(t.surface))
                            .border_1()
                            .border_color(c(t.border).opacity(0.8))
                            .t_small()
                            .text_color(c(if has_prompt { t.text } else { t.text_muted }))
                            .child(prompt_text),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(zpx(6.))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(zpx(6.))
                            .child(
                                div()
                                    .font_family(MONO)
                                    .t_caption()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(c(t.text_muted))
                                    .child("GUARDRAILS (ANTI-JOBS)"),
                            )
                            .when(!bot.anti_jobs.is_empty(), |d| {
                                d.child(
                                    div()
                                        .font_family(MONO)
                                        .t_caption()
                                        .text_color(c(t.text_faint))
                                        .child(bot.anti_jobs.len().to_string()),
                                )
                            }),
                    )
                    .child(if bot.anti_jobs.is_empty() {
                        div()
                            .w_full()
                            .p(zpx(12.))
                            .rounded(r_md())
                            .bg(c(t.surface))
                            .border_1()
                            .border_color(c(t.border).opacity(0.8))
                            .t_small()
                            .text_color(c(t.text_muted))
                            .child("No guardrails configured.")
                    } else {
                        div()
                            .w_full()
                            .p(zpx(12.))
                            .rounded(r_md())
                            .bg(c(t.surface))
                            .border_1()
                            .border_color(c(t.border).opacity(0.8))
                            .flex()
                            .flex_col()
                            .gap(zpx(6.))
                            .children(bot.anti_jobs.iter().map(|rule| {
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_start()
                                    .gap(zpx(7.))
                                    .t_small()
                                    .text_color(c(t.text))
                                    .child(div().text_color(c(t.text_faint)).child("•"))
                                    .child(div().flex_1().min_w_0().child(rule.clone()))
                            }))
                    }),
            );

        let footer = div()
            .pt_2()
            .border_t_1()
            .border_color(c(t.border).opacity(0.7))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .child(
                div()
                    .t_caption()
                    .text_color(c(t.text_faint))
                    .child("Esc to close"),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(8.))
                    .when(!is_creator, |d| {
                        let cust_id = bot.id.clone();
                        d.child(
                            div()
                                .id("bot-info-customize-btn")
                                .h(zpx(28.))
                                .px(zpx(10.))
                                .rounded(r_md())
                                .border_1()
                                .border_color(c(t.border))
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(zpx(5.))
                                .cursor_pointer()
                                .t_small()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(c(t.text))
                                .hover_fade(c(t.hover), None)
                                .active(|s| s.opacity(0.78))
                                .child(Icon::PenLine.el(zpx(12.)))
                                .child("Customize")
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    this.dismiss_bot_info(cx);
                                    this.open_bot_customizer(&cust_id, window, cx);
                                })),
                        )
                    })
                    .child(
                        text_button("bot-info-done-btn", "Done", t).on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| {
                                this.dismiss_bot_info(cx);
                            },
                        )),
                    ),
            );

        let card = div()
            .id(("bot-info-modal-card", info_gen as usize))
            .w(zpx(600.))
            .max_w(relative(0.92))
            .p_5()
            .rounded(RADIUS_XL)
            .bg(c(t.floating))
            .border_1()
            .border_color(c(t.border))
            .floating_surface(t)
            .shadow_2xl()
            .flex()
            .flex_col()
            .gap_4()
            .cursor_default()
            .on_click(|_: &ClickEvent, _, cx| cx.stop_propagation())
            .child(header)
            .child(body_scroll)
            .child(footer);

        let card_motion = if leaving {
            crate::motion::dismiss(
                card,
                ("bot-info-card-out", info_gen as usize),
                reduced,
            )
        } else {
            crate::motion::appear(
                card,
                ("bot-info-card-in", info_gen as usize),
                crate::motion::BASE,
                14.,
                reduced,
            )
            .into_any_element()
        };

        let scrim = div()
            .id(("bot-info-scrim", info_gen as usize))
            .absolute()
            .inset_0()
            .occlude()
            .bg(gpui::rgba(0x00000088))
            .flex()
            .items_center()
            .justify_center()
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.dismiss_bot_info(cx);
            }));

        Some(if leaving {
            if reduced {
                scrim.child(card_motion).into_any_element()
            } else {
                scrim
                    .child(card_motion)
                    .with_animation(
                        ("bot-info-scrim-out", info_gen as usize),
                        gpui::Animation::new(crate::motion::FAST)
                            .with_easing(crate::motion::ease_out),
                        |el, delta| el.opacity(1.0 - delta),
                    )
                    .into_any_element()
            }
        } else if reduced {
            scrim.child(card_motion).into_any_element()
        } else {
            scrim
                .child(card_motion)
                .with_animation(
                    ("bot-info-scrim-in", info_gen as usize),
                    gpui::Animation::new(crate::motion::FAST)
                        .with_easing(crate::motion::ease_out),
                    |el, delta| el.opacity(delta),
                )
                .into_any_element()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_takes_the_first_line_and_marks_engine_clipping() {
        assert_eq!(bot_preview_line("\n  hello\nworld"), "hello");
        assert_eq!(bot_preview_line("short"), "short");
        let clipped = "x".repeat(80);
        assert_eq!(bot_preview_line(&clipped), format!("{clipped}…"));
    }

    #[test]
    fn room_rank_floats_waiting_then_running() {
        assert!(
            bot_room_rank(DisplayStatus::AwaitingInput) < bot_room_rank(DisplayStatus::Running)
        );
        assert!(bot_room_rank(DisplayStatus::Running) < bot_room_rank(DisplayStatus::Idle));
    }
}

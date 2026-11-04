//! The session's merge request monitor: a chip in the header (status dot,
//! `!42`, short title; click opens the MR in the browser, hover for a card,
//! right-click for Refresh / Stop tracking), a status dot on the chat's
//! sidebar row, and one-line offers and notes under transcript turns.
//!
//! An MR gets here three ways: a tool in a turn created it (tracked at
//! once, with Undo), a link to one shows up in the chat (offered under that
//! turn: Track / Ignore), or "Track merge request…" (paste a link or `!42`).
//! One MR per session; a second one is offered as a replacement. The model
//! and its file are in [`crate::mr`].
//!
//! Polling: the chat on screen every minute while the window has focus (and
//! at once on focus or switch if the last look is 15 s old), backing off
//! after errors; other chats' MRs at most every five minutes, one per tick.

use crate::hover_fade::HoverFadeExt;
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnimationExt as _, AnyElement, App, Bounds, ClickEvent, Context, ElementId, MouseButton,
    MouseDownEvent, Pixels, Task, Window, div, prelude::*, px,
};
use keel_theme::Tokens;

use super::sidebar::{filled_button, text_button};
use super::{KeelWindow, menu_item, menu_motion, menu_panel, menu_separator};
use crate::icons::Icon;
use crate::motion;
use crate::mr::{
    Mention, MrRef, MrSnapshot, MrStatus, find_mr_urls, parse_input, parse_remote, status_color,
    truncate,
};
use crate::style::{HEADER_HEIGHT, KeelStyled, MONO, RADIUS_MD, RADIUS_SM, c, zpx};
use crate::timer::now_ms;

type BoundsCell = Rc<Cell<Option<Bounds<Pixels>>>>;

/// A line under a transcript turn.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum InlineMr {
    /// "!42 Title · Track / Ignore" (Track reads "Replace !41" when one is tracked).
    Offer {
        mr: MrRef,
        title: String,
        replace: Option<String>,
    },
    /// The agent's own MR was just tracked: Undo.
    Undo { mr: MrRef },
    /// "!42 merged · 3:12 PM".
    Note { text: String },
}

#[derive(Clone, Debug)]
pub(super) enum MrAction {
    Track(MrRef),
    Ignore,
    Undo,
}

pub(super) type OnMrAction = Rc<dyn Fn(MrAction, &mut App)>;

/// View-only state (the MRs themselves are `KeelWindow::mr`).
#[derive(Default)]
pub(super) struct MrUi {
    poll: Option<Task<()>>,
    /// Fetches under way (`chat|mr`), and offers whose title was asked for.
    in_flight: HashSet<String>,
    offer_tried: HashSet<String>,
    /// Connected GitLab hosts (`None` until listed): links elsewhere aren't offered.
    hosts: Option<Vec<String>>,
    window_active: bool,
    hover: bool,
    hover_gen: u64,
    card: bool,
    menu: bool,
    painted: BoundsCell,
    content_w: Rc<Cell<f32>>,
    /// What the header showed last frame (chat, MR key), to see an MR arrive.
    shown: Option<(String, Option<String>)>,
    enter: Option<(u64, Instant)>,
    enter_gen: u64,
    /// Status last drawn per chat, and the latest change: (number, old color, when, pulse).
    last_status: HashMap<String, (String, MrStatus)>,
    change: Option<(u64, u32, Instant, bool)>,
    change_gen: u64,
    /// Merged/closed on screen in this chat: the hold, then the fold.
    done_since: Option<String>,
    done_task: Option<Task<()>>,
    leaving: Option<(u64, Instant)>,
    leave_gen: u64,
    /// Just tracked on its own: (chat, MR, turn) for the inline Undo.
    undo: Option<(String, MrRef, String)>,
    prompt: bool,
    prompt_error: Option<String>,
    /// Rows carrying lines last frame, to remeasure when they change.
    inline_sig: Vec<(usize, String)>,
}

fn flight_key(chat: &str, mr: &MrRef) -> String {
    format!("{chat}|{}", mr.key())
}

const POLL_MS: i64 = 60_000;
const BACKGROUND_MS: i64 = 5 * 60_000;
const FRESH_MS: i64 = 15_000;
const TICK: Duration = Duration::from_secs(5);
const CARD_DELAY: Duration = Duration::from_millis(400);
/// Merged / Closed stays up this long once seen, then folds away.
const DONE_HOLD: Duration = Duration::from_secs(3);
const ENTER: Duration = Duration::from_millis(180);
const CONTENT_LAG: Duration = Duration::from_millis(60);
const LEAVE: Duration = Duration::from_millis(160);
const CROSSFADE: Duration = Duration::from_millis(200);
const RING: Duration = Duration::from_millis(600);
const TITLE_CHARS: usize = 28;

/// Filled, or a ring for a draft.
fn fill(d: gpui::Div, color: u32, hollow: bool) -> gpui::Div {
    if hollow {
        d.border_1().border_color(c(color))
    } else {
        d.bg(c(color))
    }
}

/// "3:12 PM", local time.
fn clock() -> Option<String> {
    let out = std::process::Command::new("date")
        .arg("+%-I:%M %p")
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn ago_ms(ms: i64) -> String {
    match (now_ms() - ms).max(0) / 1000 {
        s if s < 60 => "just now".into(),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86_400),
    }
}

/// "5m ago" for a GitLab ISO time (same shape as the GitLab page).
fn ago_iso(iso: &str) -> Option<String> {
    let num = |r: std::ops::Range<usize>| iso.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hh, mm, ss) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let secs = (era * 146_097 + doe - 719_468) * 86_400 + hh * 3600 + mm * 60 + ss;
    let age = (now_ms() / 1000 - secs).max(0);
    Some(match age {
        s if s < 60 => "just now".into(),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s if s < 86_400 * 30 => format!("{}d ago", s / 86_400),
        s => format!("{}mo ago", s / (86_400 * 30)),
    })
}

impl KeelWindow {
    // ── Behavior ────────────────────────────────────────────────────────

    /// At window open: learn the connected hosts and start the poll loop.
    pub(super) fn mr_start(&mut self, cx: &mut Context<Self>) {
        if let Some(client) = self.client.clone() {
            cx.spawn(async move |this, cx| {
                let v = client
                    .call("list_connections", serde_json::json!({}))
                    .await
                    .unwrap_or_default();
                let hosts: Vec<String> = v["connections"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|c| c["kind"] == "gitlab")
                    .filter_map(|c| c["host"].as_str().map(str::to_string))
                    .collect();
                let _ = this.update(cx, |this, cx| {
                    this.mr_ui.hosts = Some(hosts);
                    cx.notify();
                });
            })
            .detach();
        }
        self.mr_ui.poll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                if this.update(cx, |this, cx| this.mr_tick(cx)).is_err() {
                    break;
                }
            }
        }));
    }

    fn mr_host_ok(&self, host: &str) -> bool {
        self.connections
            .iter()
            .any(|c| c.kind == "gitlab" && c.host == host)
            || self
                .mr_ui
                .hosts
                .as_ref()
                .is_none_or(|hosts| hosts.iter().any(|h| h == host))
    }

    /// The chat whose header is on screen (not under Settings or GitLab).
    fn mr_visible_chat(&self) -> Option<String> {
        self.current_chat
            .clone()
            .filter(|_| !self.settings_open && !self.gitlab_open)
    }

    /// A turn's rows were published: MR links in its text are offered, or
    /// tracked when a tool in the turn created one.
    pub(super) fn mr_scan(&mut self, msg_id: &str, text: &str, created: bool) {
        let Some(chat) = self.current_chat.clone() else {
            return;
        };
        for mr in find_mr_urls(text) {
            if !self.mr_host_ok(&mr.host) {
                continue;
            }
            match self.mr.entry(&chat).mention(mr.clone(), msg_id, created) {
                Mention::Nothing => continue,
                Mention::Tracked => self.mr_ui.undo = Some((chat.clone(), mr, msg_id.to_string())),
                Mention::Offered => {}
            }
            self.mr.save(&self.state.data_dir);
        }
    }

    fn mr_tick(&mut self, cx: &mut Context<Self>) {
        let now = now_ms();
        let current = self.mr_visible_chat();
        if self.mr_ui.window_active
            && let Some(chat) = current.clone()
            && let Some(t) = self.mr.tracked(&chat)
            && !t.status().is_some_and(MrStatus::is_done)
            && t.due(now, POLL_MS)
        {
            let mr = t.mr.clone();
            self.mr_fetch(chat, mr, cx);
        }
        let background = self
            .mr
            .sessions
            .iter()
            .filter(|(id, _)| current.as_ref() != Some(*id))
            .filter(|(id, _)| {
                self.state
                    .chats
                    .get(*id)
                    .is_some_and(|c| !c.archived && !c.tombstoned)
            })
            .filter_map(|(id, s)| s.tracked.as_ref().map(|t| (id, t)))
            .find(|(id, t)| {
                !t.status().is_some_and(MrStatus::is_done)
                    && t.due(now, BACKGROUND_MS)
                    && !self.mr_ui.in_flight.contains(&flight_key(id, &t.mr))
            })
            .map(|(id, t)| (id.clone(), t.mr.clone()));
        if let Some((chat, mr)) = background {
            self.mr_fetch(chat, mr, cx);
        }
    }

    /// Window focus or a chat switch: look again if the last look is stale.
    pub(super) fn mr_poke(&mut self, cx: &mut Context<Self>) {
        let Some(chat) = self.mr_visible_chat() else {
            return;
        };
        if let Some(t) = self.mr.tracked(&chat)
            && !t.status().is_some_and(MrStatus::is_done)
            && now_ms() - t.tried_ms > FRESH_MS
        {
            let mr = t.mr.clone();
            self.mr_fetch(chat, mr, cx);
        }
    }

    pub(super) fn mr_note_window_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if active != self.mr_ui.window_active {
            self.mr_ui.window_active = active;
            if active {
                self.mr_poke(cx);
            }
        }
    }

    /// The active chat changed: per-chat view state starts over.
    pub(super) fn mr_on_switch(&mut self, cx: &mut Context<Self>) {
        let ui = &mut self.mr_ui;
        ui.undo = None;
        ui.hover = false;
        ui.card = false;
        ui.menu = false;
        ui.prompt = false;
        ui.change = None;
        ui.done_since = None;
        ui.done_task = None;
        ui.leaving = None;
        self.mr_poke(cx);
    }

    /// First look at a newly tracked MR, and an offer's title.
    fn mr_ensure(&mut self, chat: &str, cx: &mut Context<Self>) {
        if let Some(t) = self.mr.tracked(chat)
            && t.tried_ms == 0
        {
            let mr = t.mr.clone();
            self.mr_fetch(chat.to_string(), mr, cx);
        }
        if let Some(offer) = self.mr.get(chat).and_then(|s| s.offer.as_ref())
            && offer.snap.is_none()
            && self.mr_ui.offer_tried.insert(flight_key(chat, &offer.mr))
        {
            let mr = offer.mr.clone();
            self.mr_fetch(chat.to_string(), mr, cx);
        }
    }

    fn mr_fetch(&mut self, chat: String, mr: MrRef, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let key = flight_key(&chat, &mr);
        if !self.mr_ui.in_flight.insert(key.clone()) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let params =
                serde_json::json!({ "host": mr.host, "project": mr.project, "iid": mr.iid });
            let res = client.call("gitlab_mr", params).await;
            let _ = this.update(cx, |this, cx| {
                this.mr_ui.in_flight.remove(&key);
                this.mr_apply(&chat, &mr, res);
                cx.notify();
            });
        })
        .detach();
    }

    fn mr_apply(&mut self, chat: &str, mr: &MrRef, res: Result<serde_json::Value, String>) {
        let now = now_ms();
        let auto = self
            .mr_ui
            .undo
            .as_ref()
            .is_some_and(|(c, m, _)| c == chat && m == mr);
        let session = self.mr.entry(chat);
        let (mut changed, mut stop, mut drop_offer) = (false, false, false);
        if let Some(t) = session.tracked.as_mut().filter(|t| t.mr == *mr) {
            changed = true;
            t.tried_ms = now;
            match &res {
                Ok(v) => {
                    let snap = MrSnapshot::parse(v);
                    // Tracked on its own from an old turn, and long done: nothing to watch.
                    stop = auto && t.ok_ms == 0 && snap.status().is_done();
                    t.snap = Some(snap);
                    t.ok_ms = now;
                    t.errors = 0;
                    t.error = None;
                }
                Err(err) => {
                    t.errors += 1;
                    t.error = Some(err.clone());
                }
            }
        }
        if let Some(offer) = session.offer.as_mut().filter(|o| o.mr == *mr)
            && let Ok(v) = &res
        {
            changed = true;
            let snap = MrSnapshot::parse(v);
            // Already merged or closed: not worth offering.
            drop_offer = snap.status().is_done();
            offer.snap = Some(snap);
        }
        if stop {
            session.stop();
            self.mr_ui.undo = None;
        }
        if drop_offer {
            session.ignore_offer();
        }
        if changed {
            self.mr.save(&self.state.data_dir);
        }
    }

    pub(super) fn mr_track(&mut self, chat: String, mr: MrRef, cx: &mut Context<Self>) {
        self.mr.entry(&chat).track(mr.clone());
        self.mr_ui.undo = None;
        self.mr.save(&self.state.data_dir);
        self.mr_fetch(chat, mr, cx);
        cx.notify();
    }

    pub(super) fn mr_stop(&mut self, cx: &mut Context<Self>) {
        let ui = &mut self.mr_ui;
        (ui.menu, ui.hover, ui.card) = (false, false, false);
        if let Some(chat) = self.current_chat.clone() {
            self.mr.entry(&chat).stop();
            self.mr.save(&self.state.data_dir);
        }
        cx.notify();
    }

    fn mr_refresh(&mut self, cx: &mut Context<Self>) {
        self.mr_ui.menu = false;
        if let Some(chat) = self.current_chat.clone()
            && let Some(mr) = self.mr.tracked(&chat).map(|t| t.mr.clone())
        {
            self.mr_fetch(chat, mr, cx);
        }
        cx.notify();
    }

    pub(super) fn mr_inline_action(&mut self, action: MrAction, cx: &mut Context<Self>) {
        let Some(chat) = self.current_chat.clone() else {
            return;
        };
        match action {
            MrAction::Track(mr) => return self.mr_track(chat, mr, cx),
            MrAction::Ignore => self.mr.entry(&chat).ignore_offer(),
            MrAction::Undo => {
                if self.mr_ui.undo.take().is_some() {
                    self.mr.entry(&chat).stop();
                }
            }
        }
        self.mr.save(&self.state.data_dir);
        cx.notify();
    }

    fn mr_hover(&mut self, hovered: bool, cx: &mut Context<Self>) {
        if self.mr_ui.hover == hovered {
            return;
        }
        self.mr_ui.hover = hovered;
        self.mr_ui.hover_gen += 1;
        self.mr_ui.card = false;
        if hovered {
            let n = self.mr_ui.hover_gen;
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(CARD_DELAY).await;
                let _ = this.update(cx, |this, cx| {
                    if this.mr_ui.hover && this.mr_ui.hover_gen == n && !this.mr_ui.menu {
                        this.mr_ui.card = true;
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        cx.notify();
    }

    /// Merged or closed, on screen: hold ~3 s, fold the chip away, then
    /// drop it and leave a note in the transcript.
    fn mr_hold_done(&mut self, chat: &str, key: String, cx: &mut Context<Self>) {
        if self.mr_ui.done_since.as_deref() == Some(chat) {
            return;
        }
        self.mr_ui.done_since = Some(chat.to_string());
        self.mr_ui.leaving = None;
        let chat = chat.to_string();
        self.mr_ui.done_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DONE_HOLD).await;
            let here = this
                .update(cx, |this, cx| {
                    let here = this.mr_visible_chat().as_deref() == Some(chat.as_str())
                        && this.mr.tracked(&chat).is_some_and(|t| t.mr.key() == key);
                    if here {
                        this.mr_ui.leave_gen += 1;
                        this.mr_ui.leaving = Some((this.mr_ui.leave_gen, Instant::now()));
                        cx.notify();
                    } else {
                        this.mr_ui.done_since = None;
                    }
                    here
                })
                .unwrap_or(false);
            if here {
                cx.background_executor()
                    .timer(LEAVE + Duration::from_millis(20))
                    .await;
                let _ = this.update(cx, |this, cx| this.mr_finish(&chat, cx));
            }
        }));
    }

    fn mr_finish(&mut self, chat: &str, cx: &mut Context<Self>) {
        let ui = &mut self.mr_ui;
        (ui.done_since, ui.leaving, ui.hover, ui.card) = (None, None, false, false);
        if let Some(t) = self.mr.tracked(chat)
            && let Some(status) = t.status().filter(|s| s.is_done())
        {
            let verb = if status == MrStatus::Merged {
                "merged"
            } else {
                "closed"
            };
            let text = match clock() {
                Some(at) => format!("{} {verb} · {at}", t.mr.label()),
                None => format!("{} {verb}", t.mr.label()),
            };
            self.mr.entry(chat).finish(text);
            self.mr.save(&self.state.data_dir);
        }
        cx.notify();
    }

    /// Session menu / menu bar: "Track merge request…".
    pub(super) fn mr_open_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.current_chat.is_none() {
            return;
        }
        self.mr_ui.prompt = true;
        self.mr_ui.prompt_error = None;
        self.mr_input.update(cx, |s, cx| {
            s.set_value("", window, cx);
            s.focus(window, cx);
        });
        cx.notify();
    }

    pub(super) fn mr_submit_prompt(&mut self, cx: &mut Context<Self>) {
        let Some(chat) = self.current_chat.clone() else {
            return;
        };
        let text = self.mr_input.read(cx).value().to_string();
        let default = self.mr_default_project(&chat);
        match parse_input(
            &text,
            default.as_ref().map(|(h, p)| (h.as_str(), p.as_str())),
        ) {
            Ok(mr) => {
                self.mr_ui.prompt = false;
                self.mr_track(chat, mr, cx);
            }
            Err(err) => {
                self.mr_ui.prompt_error = Some(err);
                cx.notify();
            }
        }
    }

    /// Where `!42` points: the MR this chat already knows, else its
    /// folder's `origin` remote.
    fn mr_default_project(&self, chat: &str) -> Option<(String, String)> {
        let session = self.mr.get(chat);
        let known = session.and_then(|s| {
            s.tracked
                .as_ref()
                .map(|t| &t.mr)
                .or(s.offer.as_ref().map(|o| &o.mr))
        });
        if let Some(mr) = known {
            return Some((mr.host.clone(), mr.project.clone()));
        }
        let path = self
            .state
            .chats
            .get(chat)
            .and_then(|c| self.state.spaces.get(&c.space_id))
            .map(|s| s.path.clone())?;
        let out = std::process::Command::new("git")
            .args(["-C", &path, "remote", "get-url", "origin"])
            .output()
            .ok()?;
        parse_remote(&String::from_utf8_lossy(&out.stdout))
    }

    /// Escape and outside clicks. True when something was open.
    pub(super) fn mr_close_menus(&mut self) -> bool {
        let open = self.mr_ui.menu || self.mr_ui.prompt;
        self.mr_ui.menu = false;
        self.mr_ui.prompt = false;
        open
    }

    // ── Rendering ───────────────────────────────────────────────────────

    /// The header chip, after the chat title. `None`: nothing tracked.
    pub(super) fn render_mr_chip(
        &mut self,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let chat = self.current_chat.clone()?;
        self.mr_ensure(&chat, cx);
        let tracked = self.mr.tracked(&chat).cloned();
        let key = tracked.as_ref().map(|t| t.mr.key());
        // An MR arriving in the chat on screen expands in; a chat switch doesn't.
        if self.mr_ui.shown.as_ref().map(|(c, k)| (c, k)) != Some((&chat, &key)) {
            if key.is_some() && self.mr_ui.shown.as_ref().is_some_and(|(c, _)| *c == chat) {
                self.mr_ui.enter_gen += 1;
                self.mr_ui.enter = Some((self.mr_ui.enter_gen, Instant::now()));
            }
            self.mr_ui.shown = Some((chat.clone(), key));
        }
        let tracked = tracked?;
        let reduced = self.reduced();
        let status = tracked.status();
        let snap = tracked.snap.clone().unwrap_or_default();
        let color = status_color(status, t);
        if let Some(status) = status {
            let prev = self
                .mr_ui
                .last_status
                .insert(chat.clone(), (tracked.mr.key(), status));
            if let Some((prev_key, prev_status)) = prev
                && prev_key == tracked.mr.key()
                && prev_status != status
            {
                self.mr_ui.change_gen += 1;
                let old = status_color(Some(prev_status), t);
                self.mr_ui.change = Some((
                    self.mr_ui.change_gen,
                    old,
                    Instant::now(),
                    status.is_meaningful(),
                ));
            }
        }
        let done = status.is_some_and(MrStatus::is_done);
        if done {
            self.mr_hold_done(&chat, tracked.mr.key(), cx);
        }
        let hovered = self.mr_ui.hover;
        let url = if snap.web_url.is_empty() {
            tracked.mr.web_url()
        } else {
            snap.web_url.clone()
        };
        let dot = self.mr_dot(color, status, snap.draft, tracked.error.is_some(), reduced);
        let title = truncate(&snap.title, TITLE_CHARS);
        let close = div()
            .id("mr-chip-close")
            .size(zpx(20.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(RADIUS_SM)
            .text_color(c(t.text_muted))
            .hover(|s| s.text_color(c(t.text)))
            .hover_fade(c(t.hover), None)
            .child(Icon::Close.el(zpx(12.)))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                this.mr_stop(cx);
            }));
        let content = div()
            .h(zpx(26.))
            .pl(zpx(8.))
            .pr(zpx(if hovered { 3. } else { 10. }))
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(7.))
            .child(dot)
            .child(
                div()
                    .t_body()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(c(t.text))
                    .child(tracked.mr.label()),
            )
            .map(|d| {
                if done {
                    d.child(div().t_body().text_color(c(color)).child(
                        if status == Some(MrStatus::Merged) {
                            "Merged"
                        } else {
                            "Closed"
                        },
                    ))
                } else if title.is_empty() {
                    d
                } else {
                    d.child(
                        div()
                            .t_body()
                            .text_color(c(t.text_muted))
                            .when(snap.draft, |d| d.opacity(0.6))
                            .child(title),
                    )
                }
            })
            .when(hovered, |d| {
                d.child(
                    div()
                        .text_color(c(t.text_faint))
                        .child(Icon::External.el(zpx(12.))),
                )
                .child(close)
            });
        let record = self.mr_ui.content_w.clone();
        let measured = div()
            .relative()
            .flex()
            .flex_none()
            .whitespace_nowrap()
            .child(content)
            .child(
                gpui::canvas(
                    move |b, _, _| record.set(f32::from(b.size.width)),
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            );
        let painted = self.mr_ui.painted.clone();
        let pill = div()
            .id("mr-chip")
            .relative()
            .flex()
            .flex_none()
            .overflow_hidden()
            .p(zpx(3.))
            .rounded(zpx(10.))
            .bg(c(t.floating & 0xFFFF_FF00 | 0xB3))
            .border_1()
            .border_color(c(t.border))
            .cursor_pointer()
            .child(
                gpui::canvas(move |b, _, _| painted.set(Some(b)), |_, _, _, _| {})
                    .absolute()
                    .inset_0(),
            )
            .child(measured)
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| this.mr_hover(*hovered, cx)))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.mr_ui.card = false;
                cx.open_url(&url);
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.mr_ui.menu = true;
                    this.mr_ui.card = false;
                    cx.notify();
                }),
            );
        // Width moves, as the timer's does: content width + padding and border.
        let w = self.mr_ui.content_w.clone();
        let chrome = f32::from(zpx(8.));
        let pill: AnyElement = match (self.mr_ui.leaving, self.mr_ui.enter) {
            _ if reduced => pill.into_any_element(),
            (Some((n, _)), _) => {
                let from = w.get() + chrome;
                pill.with_animation(
                    ("mr-leave", n as usize),
                    gpui::Animation::new(LEAVE).with_easing(motion::ease_in),
                    move |el, d| el.w(px(from * (1. - d))).opacity(1. - d),
                )
                .into_any_element()
            }
            (None, Some((n, at))) if at.elapsed() < ENTER + CONTENT_LAG => {
                let total = (ENTER + CONTENT_LAG).as_secs_f32();
                let (grow_for, lag) = (ENTER.as_secs_f32(), CONTENT_LAG.as_secs_f32());
                pill.with_animation(
                    ("mr-enter", n as usize),
                    gpui::Animation::new(ENTER + CONTENT_LAG),
                    move |el, d| {
                        let s = d * total;
                        let grow = motion::ease_out((s / grow_for).min(1.));
                        let fade = motion::ease_out(((s - lag) / grow_for).clamp(0., 1.));
                        el.w(px((w.get() + chrome) * grow)).opacity(fade)
                    },
                )
                .into_any_element()
            }
            _ => pill.into_any_element(),
        };
        Some(div().flex_none().ml(zpx(8.)).child(pill).into_any_element())
    }

    /// The status dot: color crossfades on a change; one soft ring for a
    /// change that matters; a slow thin ring while a pipeline runs.
    fn mr_dot(
        &self,
        color: u32,
        status: Option<MrStatus>,
        draft: bool,
        stale: bool,
        reduced: bool,
    ) -> AnyElement {
        const BOX: f32 = 12.;
        const DOT: f32 = 7.;
        let at = (BOX - DOT) / 2.;
        let dot = move |color: u32| {
            fill(
                div()
                    .absolute()
                    .left(zpx(at))
                    .top(zpx(at))
                    .size(zpx(DOT))
                    .rounded_full(),
                color,
                draft,
            )
        };
        let mut layer = div()
            .relative()
            .flex_none()
            .size(zpx(BOX))
            .when(stale, |d| d.opacity(0.4));
        let change = self.mr_ui.change.filter(|_| !reduced);
        match change {
            Some((n, from, when, _)) if when.elapsed() < CROSSFADE => {
                layer = layer.child(dot(from)).child(dot(color).with_animation(
                    ("mr-dot-fade", n as usize),
                    gpui::Animation::new(CROSSFADE),
                    |el, d| el.opacity(d),
                ));
            }
            _ => layer = layer.child(dot(color)),
        }
        if status == Some(MrStatus::Running) {
            let ring = div()
                .absolute()
                .left_0()
                .top_0()
                .size(zpx(BOX))
                .rounded_full()
                .border_1()
                .border_color(c(color));
            layer = layer.child(motion::breathe(ring, "mr-run-ring", reduced));
        }
        if let Some((n, _, when, true)) = change
            && when.elapsed() < RING
        {
            let ring = div()
                .absolute()
                .rounded_full()
                .border_1()
                .border_color(c(color));
            layer = layer.child(ring.with_animation(
                ("mr-dot-ring", n as usize),
                gpui::Animation::new(RING).with_easing(motion::ease_out),
                move |el, d| {
                    let size = DOT + 12. * d;
                    let off = (BOX - size) / 2.;
                    el.left(zpx(off))
                        .top(zpx(off))
                        .size(zpx(size))
                        .opacity(0.7 * (1. - d))
                },
            ));
        }
        layer.into_any_element()
    }

    /// Glyph and dot on a chat's sidebar row while it tracks an MR.
    pub(super) fn mr_badge(&self, chat_id: &str, t: Tokens) -> Option<AnyElement> {
        let tracked = self.mr.tracked(chat_id)?;
        let draft = tracked.snap.as_ref().is_some_and(|s| s.draft);
        let color = status_color(tracked.status(), t);
        Some(
            div()
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(3.))
                .child(
                    div()
                        .text_color(c(t.text_faint))
                        .child(Icon::GitLab.el(zpx(10.))),
                )
                .child(
                    fill(div().size(zpx(6.)).rounded_full(), color, draft)
                        .when(tracked.error.is_some(), |d| d.opacity(0.4)),
                )
                .into_any_element(),
        )
    }

    /// Hover card under the chip: full title, status, branches, freshness.
    pub(super) fn render_mr_card(
        &mut self,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let leaving = self.menu_phase("mr-card", self.mr_ui.card && !self.mr_ui.menu, cx)?;
        let tracked = self.mr.tracked(self.current_chat.as_ref()?)?;
        let chip = self.mr_ui.painted.get()?;
        let snap = tracked.snap.clone().unwrap_or_default();
        let color = status_color(tracked.status(), t);
        let status = match (&tracked.error, tracked.snap.is_some()) {
            (Some(_), _) if tracked.ok_ms > 0 => {
                format!("Couldn't refresh · checked {}", ago_ms(tracked.ok_ms))
            }
            (Some(_), _) => "Couldn't load".to_string(),
            (None, true) => snap.status_text(),
            (None, false) => "Loading…".to_string(),
        };
        let title = if snap.title.is_empty() {
            tracked.mr.label()
        } else {
            snap.title.clone()
        };
        let branches = (!snap.source_branch.is_empty())
            .then(|| format!("{} → {}", snap.source_branch, snap.target_branch));
        let foot = match ago_iso(&snap.updated_at) {
            Some(age) => format!(
                "{}{} · updated {age}",
                tracked.mr.project,
                tracked.mr.label()
            ),
            None => format!("{}{}", tracked.mr.project, tracked.mr.label()),
        };
        let card = div()
            .w(zpx(300.))
            .mt_1()
            .p_3()
            .flex()
            .flex_col()
            .gap_1p5()
            .floating_surface(t)
            .child(
                div()
                    .t_body()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(title),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1p5()
                    .t_small()
                    .text_color(c(t.text_muted))
                    .child(fill(
                        div().size(zpx(7.)).flex_none().rounded_full(),
                        color,
                        snap.draft,
                    ))
                    .child(status),
            )
            .children(
                tracked
                    .error
                    .clone()
                    .map(|err| div().t_caption().text_color(c(t.text_faint)).child(err)),
            )
            .children(branches.map(|b| {
                div()
                    .t_caption()
                    .font_family(MONO)
                    .text_color(c(t.text_muted))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(b)
            }))
            .child(
                div()
                    .t_caption()
                    .text_color(c(t.text_faint))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(foot),
            );
        let card = menu_motion(card, "mr-card", false, leaving, self.reduced());
        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(gpui::point(chip.left(), chip.bottom()))
                    .anchor(gpui::Anchor::TopLeft)
                    .snap_to_window_with_margin(zpx(8.))
                    .child(card),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    /// Right-click on the chip: Open, Refresh, Stop tracking.
    pub(super) fn render_mr_menu(
        &mut self,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let leaving = self.menu_phase("mr-menu", self.mr_ui.menu, cx)?;
        let tracked = self.mr.tracked(self.current_chat.as_ref()?)?;
        let chip = self.mr_ui.painted.get()?;
        let url = tracked
            .snap
            .as_ref()
            .map(|s| s.web_url.clone())
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| tracked.mr.web_url());
        let items = vec![
            menu_item("mr-menu-open", t, true, false)
                .child("Open in browser")
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.mr_ui.menu = false;
                    cx.open_url(&url);
                    cx.notify();
                }))
                .into_any_element(),
            menu_item("mr-menu-refresh", t, true, false)
                .child("Refresh")
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.mr_refresh(cx)))
                .into_any_element(),
            menu_separator(t).into_any_element(),
            menu_item("mr-menu-stop", t, true, false)
                .child("Stop tracking")
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.mr_stop(cx)))
                .into_any_element(),
        ];
        let dismiss = cx.listener(|this, _: &MouseDownEvent, _, cx| {
            this.mr_ui.menu = false;
            cx.notify();
        });
        let panel = menu_panel("mr-menu", t, 184., false, items)
            .when(leaving.is_none(), |d| d.on_mouse_down_out(dismiss));
        let menu = menu_motion(panel, "mr-menu", false, leaving, self.reduced());
        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(gpui::point(chip.left(), chip.bottom()))
                    .anchor(gpui::Anchor::TopLeft)
                    .snap_to_window_with_margin(zpx(8.))
                    .child(menu),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    /// "Track merge request…": paste a link or `!42`.
    pub(super) fn render_mr_prompt(
        &mut self,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.mr_ui.prompt {
            return None;
        }
        let hint = match &self.mr_ui.prompt_error {
            Some(err) => div().t_small().text_color(c(t.danger)).child(err.clone()),
            None => div()
                .t_small()
                .text_color(c(t.text_faint))
                .child("Paste a merge request link, or !42 for this chat's repo."),
        };
        let card = div()
            .id("mr-prompt")
            .w(zpx(380.))
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .floating_surface(t)
            .occlude()
            .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, _, cx| {
                this.mr_ui.prompt = false;
                cx.notify();
            }))
            .child(
                div()
                    .t_body()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child("Track merge request"),
            )
            .child(
                div()
                    .px_2()
                    .py_1()
                    .rounded(RADIUS_SM)
                    .bg(c(t.input))
                    .border_1()
                    .border_color(c(t.focus))
                    .t_body()
                    .child(self.mr_input.clone()),
            )
            .child(hint)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap_1()
                    .child(
                        text_button("mr-prompt-cancel", "Cancel", t).on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| {
                                this.mr_ui.prompt = false;
                                cx.notify();
                            },
                        )),
                    )
                    .child(
                        filled_button("mr-prompt-track", "Track", t.accent, t).on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.mr_submit_prompt(cx)),
                        ),
                    ),
            );
        Some(
            div()
                .absolute()
                .top(HEADER_HEIGHT + px(8.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(motion::appear(
                    card,
                    "mr-prompt-appear",
                    motion::FAST,
                    -4.,
                    self.reduced(),
                ))
                .into_any_element(),
        )
    }

    /// Lines under transcript rows (offer, Undo, notes), by row index; rows
    /// whose lines changed since last frame are remeasured.
    pub(super) fn mr_inline_rows(&mut self) -> Rc<HashMap<usize, Vec<InlineMr>>> {
        let mut out: HashMap<usize, Vec<InlineMr>> = HashMap::new();
        if let Some(chat) = self.current_chat.clone()
            && let Some(last) = self.transcript.items.last().map(|r| r.message_id.clone())
            && let Some(session) = self.mr.sessions.get_mut(&chat)
        {
            let items = &self.transcript.items;
            let last_ix = |msg: &str| items.iter().rposition(|r| r.message_id == msg);
            let mut placed = false;
            for note in &mut session.notes {
                if note.msg_id.is_none() {
                    note.msg_id = Some(last.clone());
                    placed = true;
                }
                if let Some(ix) = note.msg_id.as_deref().and_then(last_ix) {
                    out.entry(ix).or_default().push(InlineMr::Note {
                        text: note.text.clone(),
                    });
                }
            }
            let replace = session.tracked.as_ref().map(|t| t.mr.label());
            // A turn with a card for the offered MR shows Track on the card.
            let carded = |offer: &crate::mr::Offer| {
                items.iter().any(|r| {
                    r.message_id == offer.msg_id
                        && matches!(&r.block, crate::markdown::TranscriptBlock::MrCard { card, .. } if card.mr == offer.mr)
                })
            };
            if let Some(offer) = session.offer.as_ref().filter(|offer| !carded(offer))
                && let Some(ix) = last_ix(&offer.msg_id)
            {
                let title = offer
                    .snap
                    .as_ref()
                    .map(|s| truncate(&s.title, 48))
                    .unwrap_or_default();
                out.entry(ix).or_default().push(InlineMr::Offer {
                    mr: offer.mr.clone(),
                    title,
                    replace,
                });
            }
            if let Some((c, mr, msg)) = &self.mr_ui.undo
                && *c == chat
                && session.tracked.as_ref().is_some_and(|t| t.mr == *mr)
                && let Some(ix) = last_ix(msg)
            {
                out.entry(ix)
                    .or_default()
                    .push(InlineMr::Undo { mr: mr.clone() });
            }
            if placed {
                self.mr.save(&self.state.data_dir);
            }
        }
        let mut sig: Vec<(usize, String)> = out
            .iter()
            .map(|(ix, lines)| (*ix, format!("{lines:?}")))
            .collect();
        sig.sort();
        if sig != self.mr_ui.inline_sig {
            let len = self.transcript.items.len();
            let rows: HashSet<usize> = sig
                .iter()
                .chain(self.mr_ui.inline_sig.iter())
                .map(|(ix, _)| *ix)
                .filter(|ix| *ix < len)
                .collect();
            for ix in rows {
                self.transcript.list_state.remeasure_items(ix..ix + 1);
            }
            self.mr_ui.inline_sig = sig;
        }
        Rc::new(out)
    }

    pub(super) fn mr_on_action(&self, cx: &mut Context<Self>) -> OnMrAction {
        let weak = cx.entity().downgrade();
        Rc::new(move |action, cx| {
            let _ = weak.update(cx, |view, cx| view.mr_inline_action(action, cx));
        })
    }
}

/// One line under a transcript turn.
pub(super) fn render_inline(
    line: &InlineMr,
    t: Tokens,
    reduced: bool,
    on: &OnMrAction,
) -> AnyElement {
    let button = |id: String, label: String, color: u32, action: MrAction| {
        let on = on.clone();
        div()
            .id(ElementId::Name(id.into()))
            .px_1p5()
            .rounded(RADIUS_SM)
            .cursor_pointer()
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(c(color))
            .hover_fade(c(t.hover), None)
            .child(label)
            .on_click(move |_, _, cx| on(action.clone(), cx))
    };
    let chip = || {
        div()
            .h(zpx(28.))
            .pl_2()
            .pr_1()
            .flex()
            .flex_row()
            .items_center()
            .gap_1p5()
            .rounded(RADIUS_MD)
            .border_1()
            .border_color(c(t.border))
            .bg(c(t.surface))
            .t_small()
            .child(
                div()
                    .flex_none()
                    .text_color(c(t.text_muted))
                    .child(Icon::GitLab.el(zpx(12.))),
            )
    };
    let (el, id) = match line {
        InlineMr::Note { text } => {
            return div()
                .pt_1()
                .pb_2()
                .flex()
                .flex_row()
                .items_center()
                .gap_1p5()
                .t_caption()
                .text_color(c(t.text_faint))
                .child(Icon::GitLab.el(zpx(11.)))
                .child(text.clone())
                .into_any_element();
        }
        InlineMr::Offer { mr, title, replace } => (
            chip()
                .child(
                    div()
                        .flex_none()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(c(t.text))
                        .child(mr.label()),
                )
                .when(!title.is_empty(), |d| {
                    d.child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_color(c(t.text_muted))
                            .child(title.clone()),
                    )
                })
                .child(div().text_color(c(t.text_faint)).child("·"))
                .child(button(
                    format!("mr-offer-track-{}", mr.key()),
                    replace
                        .as_ref()
                        .map_or_else(|| "Track".to_string(), |old| format!("Replace {old}")),
                    t.accent,
                    MrAction::Track(mr.clone()),
                ))
                .child(button(
                    format!("mr-offer-ignore-{}", mr.key()),
                    "Ignore".into(),
                    t.text_faint,
                    MrAction::Ignore,
                )),
            format!("mr-offer-in-{}", mr.key()),
        ),
        InlineMr::Undo { mr } => (
            chip()
                .child(
                    div()
                        .text_color(c(t.text_muted))
                        .child(format!("Tracking {} in the header", mr.label())),
                )
                .child(div().text_color(c(t.text_faint)).child("·"))
                .child(button(
                    format!("mr-undo-{}", mr.key()),
                    "Undo".into(),
                    t.accent,
                    MrAction::Undo,
                )),
            format!("mr-undo-in-{}", mr.key()),
        ),
    };
    motion::appear(
        div().pt_1().pb_2().flex().flex_row().child(el),
        ElementId::Name(id.into()),
        motion::FAST,
        4.,
        reduced,
    )
    .into_any_element()
}

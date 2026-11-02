//! Agents in the browser pane. Each chat drives a tab of its own; a chat's
//! requests run one at a time, in order (none supersedes another; only the
//! chat's stop or its shim going away cancels them). Every action waits for
//! the page to settle (and any navigation it set off) and answers with what
//! changed in the page's interactive snapshot.
//!
//! A job moves through [`Stage`]s driven by page events and timer ticks;
//! a tick re-checks its stage's deadline, so stale ticks are harmless.

use std::time::{Duration, Instant};

use gpui::{Context, Focusable as _, Window};

use super::{BrowserTab, HostRequest, KeelWindow, ShotRequest};
use crate::browser::agent::{DIALOG_DEFAULT_ACCEPT, DIALOG_TIMEOUT_SECS, DialogInfo, Fifo, Snap, SnapWant, parse_sites, site_allowed, snapshot_reply};
use crate::browser::normalize_address;

/// After an action, before looking for a navigation it set off.
const SETTLE: Duration = Duration::from_millis(350);
/// An action's navigation: how long its reply waits for the load.
const NAV_WAIT: Duration = Duration::from_secs(10);
/// A navigate's (or a reloaded tab's) load.
const LOAD_WAIT: Duration = Duration::from_secs(12);
/// `browser_wait_for`: how often it looks, its default and longest wait.
const POLL: Duration = Duration::from_millis(250);
const WAIT_DEFAULT: Duration = Duration::from_secs(5);
const WAIT_MAX: Duration = Duration::from_secs(30);
/// `browser_upload`: how long the click waits for the page's file chooser.
const UPLOAD_WAIT: Duration = Duration::from_secs(2);
/// The fallback (setting files from script) reads at most this much.
const UPLOAD_MAX_BYTES: u64 = 20 * 1024 * 1024;

/// A chat's hold on a tab.
pub(super) struct AgentTab {
    pub(super) chat: String,
    queue: Fifo<HostRequest>,
    job: Option<Job>,
    /// The last compact snapshot the agent saw, for diffs.
    last_snap: Option<Snap>,
    /// Dialog and blocked-load notes for the next reply.
    notes: Vec<String>,
    /// The confirm/prompt the page is paused on, and when it defaults.
    dialog: Option<(DialogInfo, Instant)>,
    /// Last agent use (for unloading idle pages).
    pub(super) used: Instant,
}

impl AgentTab {
    pub(super) fn new(chat: String) -> Self {
        Self { chat, queue: Fifo::default(), job: None, last_snap: None, notes: Vec::new(), dialog: None, used: Instant::now() }
    }

    pub(super) fn busy(&self) -> bool {
        self.job.is_some() || !self.queue.is_empty()
    }
}

struct Job {
    req: HostRequest,
    stage: Stage,
    /// What the action did, for the reply.
    outcome: String,
    url_before: Option<String>,
    want: SnapWant,
    /// Extra reply fields (a scroll position).
    extra: serde_json::Map<String, serde_json::Value>,
    started: Instant,
    /// The load or wait ran out of time; the reply says so.
    late: bool,
}

enum Stage {
    /// Waiting for an agent script.
    Eval(u64, EvalKind),
    /// The action ran; look for a navigation at this time.
    Settle(Instant),
    /// A load is under way; reply by this time regardless.
    Load(Instant),
    /// An unloaded tab is coming back; then the request runs.
    Unpark(Instant),
    /// `wait_for`: until when, and the check in flight.
    Poll { until: Instant, check: Option<u64> },
    /// `upload`: the click went in; waiting for the page's file chooser.
    Upload { until: Instant, file_input: bool, target: String },
    /// The closing snapshot for the reply.
    Final(u64),
    Shot(ShotRequest),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EvalKind {
    Snapshot,
    Text,
    Act,
    UploadTarget,
    UploadFallback,
}

/// A JSON value as a JS literal argument.
fn js(v: &serde_json::Value) -> String {
    v.to_string()
}

fn parse_eval(body: &str) -> Result<serde_json::Value, String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| match v {
            serde_json::Value::String(s) => serde_json::from_str(&s).ok(),
            other => Some(other),
        })
        .filter(serde_json::Value::is_object)
        .ok_or_else(|| format!("the page script returned {body:?}"))
}

/// A `browser_wait_for` URL pattern: a substring, or a glob with `*`.
fn url_matches(url: &str, pattern: &str) -> bool {
    if !pattern.contains('*') {
        return url.contains(pattern);
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut rest = url;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        match rest.find(part) {
            Some(at) if i > 0 || at == 0 || pattern.starts_with('*') => rest = &rest[at + part.len()..],
            _ => return false,
        }
    }
    pattern.ends_with('*') || parts.last().is_none_or(|p| p.is_empty() || url.ends_with(p))
}

impl KeelWindow {
    /// The tab chat `chat` drives.
    pub(super) fn agent_tab_ix(&self, chat: &str) -> Option<usize> {
        self.browser.tabs.iter().position(|t| t.agent.as_ref().is_some_and(|a| a.chat == chat))
    }

    /// What chat `chat`'s agent is doing in the browser right now: the
    /// running request's op and the page's host. `None` when idle.
    pub(in crate::view) fn browser_activity(&self, chat: &str) -> Option<String> {
        let tab = &self.browser.tabs[self.agent_tab_ix(chat)?];
        let a = tab.agent.as_ref()?;
        if !a.busy() {
            return None;
        }
        let op = a.job.as_ref().map(|j| j.req.op.replace('_', " ")).unwrap_or_else(|| "queued".into());
        let host = tab.page.url.as_deref().and_then(|u| u.split("://").nth(1)).and_then(|rest| rest.split('/').next()).filter(|h| !h.is_empty());
        Some(match host {
            Some(host) => format!("{op} · {host}"),
            None => op,
        })
    }

    /// Settings → Browser → Save.
    pub(in crate::view) fn save_browser_sites(&mut self, cx: &mut Context<Self>) {
        let text = self.browser_sites.read(cx).value().to_string();
        self.state.ui_settings.browser_allowed_sites = parse_sites(&text);
        self.state.save_settings();
        tracing::info!(target: "keel_ui::browser", sites = self.state.ui_settings.browser_allowed_sites.len(), "browser allowed sites saved");
        cx.notify();
    }

    /// The toolbar's per-chat switch. Turning it off also stops the chat's
    /// running and queued browser work.
    #[allow(dead_code)]
    pub(super) fn toggle_chat_browser(&mut self, cx: &mut Context<Self>) {
        let Some(chat) = self.current_chat.clone() else { return };
        let off = &mut self.state.ui_settings.browser_off_chats;
        if let Some(at) = off.iter().position(|c| *c == chat) {
            off.remove(at);
        } else {
            off.push(chat.clone());
            self.on_browser_cancel(String::new(), chat, cx);
        }
        self.state.save_settings();
        cx.notify();
    }

    #[allow(dead_code)]
    pub(super) fn chat_browser_off(&self) -> bool {
        false
    }

    fn sites(&self) -> Vec<String> {
        self.state.ui_settings.browser_allowed_sites.clone()
    }

    fn browser_off(&self, _chat: &str) -> bool {
        false
    }

    /// Run `f` with the window (the native page needs it), even when
    /// nothing renders.
    fn agent_defer(&self, cx: &mut Context<Self>, f: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static) {
        let handle = self.browser.handle;
        let weak = cx.entity().downgrade();
        cx.defer(move |cx| {
            let _ = handle.update(cx, |_, window, cx| {
                let _ = weak.update(cx, |this, cx| f(this, window, cx));
            });
        });
    }

    /// A tick for tab `tab_id` after `after`.
    fn agent_after(&self, tab_id: u64, after: Duration, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(after + Duration::from_millis(5)).await;
            let _ = this.update(cx, |this, cx| this.agent_defer(cx, move |this, window, cx| this.agent_tick(tab_id, window, cx)));
        })
        .detach();
    }

    fn next_seq(&mut self) -> u64 {
        self.browser.seq += 1;
        self.browser.seq
    }

    /// The open chat changed: the pane shows its tab (or, when it has
    /// none, a tab of the user's), without moving focus.
    pub(super) fn follow_chat_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.browser.shown_chat == self.current_chat {
            return;
        }
        self.browser.shown_chat = self.current_chat.clone();
        let chat = self.current_chat.clone();
        let target = match chat.as_deref().and_then(|c| self.agent_tab_ix(c)) {
            Some(ix) => Some(ix),
            None => {
                let b = &mut self.browser;
                if b.visible_in_strip(b.tab(), chat.as_deref()) {
                    None
                } else {
                    Some(b.tabs.iter().position(|t| b.visible_in_strip(t, chat.as_deref())).unwrap_or_else(|| b.push_tab()))
                }
            }
        };
        let Some(ix) = target.filter(|ix| *ix != self.browser.active) else {
            return;
        };
        self.browser.release_focus();
        self.browser.active = ix;
        let url = self.browser.tabs[ix].page.url.clone().unwrap_or_default();
        cx.defer_in(window, move |this, window, cx| {
            this.browser.address.update(cx, |s, cx| s.set_value(url, window, cx));
            if this.browser.open && this.browser.active == ix {
                this.unpark_tab(ix, window, cx);
            }
            cx.notify();
        });
    }

    /// ACP/MCP host tool from the engine: queued on the chat's own tab.
    pub(in crate::view) fn on_browser_host_request(&mut self, event: keel_proto::SubscribeEvent, cx: &mut Context<Self>) {
        let keel_proto::SubscribeEvent::BrowserRequest { request_id, op, url, params, chat } = event else {
            return;
        };
        let Some(client) = self.client.clone() else {
            return;
        };
        let req = HostRequest { request_id, op, url, params, client, chat };
        self.agent_defer(cx, move |this, window, cx| this.route_request(req, window, cx));
    }

    /// The engine gave up on a request (its shim went away or it timed
    /// out), or the chat was stopped: drop that work.
    pub(in crate::view) fn on_browser_cancel(&mut self, request_id: String, chat: String, cx: &mut Context<Self>) {
        self.agent_defer(cx, move |this, window, cx| {
            let tabs: Vec<usize> = (0..this.browser.tabs.len()).collect();
            for ix in tabs {
                let Some(a) = this.browser.tabs[ix].agent.as_mut() else { continue };
                let whole_chat = !chat.is_empty() && a.chat == chat;
                let dropped = a.queue.cancel(|r| whole_chat || (!request_id.is_empty() && r.request_id == request_id));
                for r in dropped {
                    Self::reply_browser(r.client, r.request_id, Err("cancelled".into()), cx);
                }
                let running = a.job.as_ref().is_some_and(|j| whole_chat || (!request_id.is_empty() && j.req.request_id == request_id));
                if running {
                    tracing::info!(target: "keel_ui::browser", chat = %a.chat, "browser job cancelled");
                    this.finish_job(ix, Err("cancelled".into()), window, cx);
                }
            }
        });
    }

    /// Find or make the chat's tab and queue the request on it.
    fn route_request(&mut self, req: HostRequest, window: &mut Window, cx: &mut Context<Self>) {
        self.browser.agent_at = Some(Instant::now());
        self.schedule_browser_park(cx);
        cx.notify();
        if self.browser_off(&req.chat) {
            return Self::reply_browser(req.client, req.request_id, Err("The user turned the browser off for this chat.".into()), cx);
        }
        let ix = match self.agent_tab_ix(&req.chat) {
            Some(ix) => ix,
            None => {
                let b = &mut self.browser;
                // A lone empty tab nobody uses is taken as is.
                let empty = |t: &BrowserTab| t.agent.is_none() && t.page.url.is_none() && t.preview.is_none();
                let ix = if b.tabs.len() == 1 && empty(&b.tabs[0]) { 0 } else { b.push_tab() };
                b.tabs[ix].agent = Some(AgentTab::new(req.chat.clone()));
                if self.current_chat.as_deref() == Some(req.chat.as_str()) || req.chat.is_empty() {
                    self.browser.active = ix;
                }
                ix
            }
        };
        let a = self.browser.tabs[ix].agent.as_mut().expect("agent tab");
        a.used = Instant::now();
        if let Some(req) = a.queue.push(req) {
            self.start_job(ix, req, window, cx);
        } else {
            tracing::debug!(target: "keel_ui::browser", queued = a.queue.len(), "browser request queued behind the chat's running one");
        }
    }

    fn set_job(&mut self, ix: usize, req: HostRequest, stage: Stage) {
        let url_before = self.browser.tabs[ix].page.url.clone();
        let want = SnapWant::parse(req.params["snapshot"].as_str(), SnapWant::Diff);
        let job = Job { req, stage, outcome: String::new(), url_before, want, extra: Default::default(), started: Instant::now(), late: false };
        if let Some(a) = self.browser.tabs[ix].agent.as_mut() {
            a.job = Some(job);
        }
    }

    fn job_mut(&mut self, ix: usize) -> Option<&mut Job> {
        self.browser.tabs.get_mut(ix)?.agent.as_mut()?.job.as_mut()
    }

    /// Start the chat's next request on tab `ix` (the queue picked it).
    fn start_job(&mut self, ix: usize, req: HostRequest, window: &mut Window, cx: &mut Context<Self>) {
        self.browser.agent_at = Some(Instant::now());
        let op = req.op.clone();
        if self.browser_off(&req.chat) {
            self.set_job(ix, req, Stage::Settle(Instant::now()));
            return self.finish_job(ix, Err("The user turned the browser off for this chat.".into()), window, cx);
        }
        // A paused page can only take the dialog's answer.
        let dialog = self.browser.tabs[ix].agent.as_ref().and_then(|a| a.dialog.as_ref()).map(|(d, _)| d.ask());
        if let Some(ask) = dialog.filter(|_| !matches!(op.as_str(), "handle_dialog" | "console" | "open_pane")) {
            self.set_job(ix, req, Stage::Settle(Instant::now()));
            return self.finish_job(ix, Err(format!("A dialog is open. {ask}")), window, cx);
        }
        let sites = self.sites();
        match op.as_str() {
            "open_pane" => {
                let result = self.host_ensure_open(ix, &req.chat, window, cx);
                self.set_job(ix, req, Stage::Settle(Instant::now()));
                self.finish_job(ix, result, window, cx)
            }
            "console" => {
                let level = req.params["level"].as_str().unwrap_or("all").to_string();
                let limit = req.params["limit"].as_u64().unwrap_or(50).clamp(1, 300) as usize;
                let clear = req.params["clear"].as_bool().unwrap_or(false);
                let text = self.browser.tabs[ix].console.read(&level, limit, clear);
                self.set_job(ix, req, Stage::Settle(Instant::now()));
                let reply = self.page_reply(ix, text);
                self.finish_job(ix, Ok(reply), window, cx)
            }
            "navigate" => self.agent_navigate(ix, req, &sites, window, cx),
            _ => {
                let url = self.browser.tabs[ix].page.url.clone();
                if let Some(url) = url.filter(|u| !site_allowed(u, &sites)) {
                    self.set_job(ix, req, Stage::Settle(Instant::now()));
                    let err = format!("This chat's tab is on {url}, which isn't on the allowed sites list (Settings → Browser). Navigate to an allowed site first.");
                    return self.finish_job(ix, Err(err), window, cx);
                }
                // Unloaded after idle: the page comes back first.
                if self.unpark_tab(ix, window, cx) {
                    let tab_id = self.browser.tabs[ix].id;
                    self.set_job(ix, req, Stage::Unpark(Instant::now() + LOAD_WAIT));
                    self.agent_after(tab_id, LOAD_WAIT, cx);
                    return;
                }
                let Some(native) = self.browser.tabs[ix].native.clone() else {
                    self.set_job(ix, req, Stage::Settle(Instant::now()));
                    return self.finish_job(ix, Err(Self::no_page_error()), window, cx);
                };
                native.set_background(true);
                native.set_agent(true, &sites);
                self.run_page_op(ix, req, &native, window, cx);
            }
        }
    }

    fn run_page_op(&mut self, ix: usize, req: HostRequest, native: &std::rc::Rc<crate::browser::NativePage>, window: &mut Window, cx: &mut Context<Self>) {
        let seq = self.next_seq();
        let tab_id = self.browser.tabs[ix].id;
        let p = req.params.clone();
        match req.op.as_str() {
            "snapshot" => {
                let text = p["mode"].as_str() == Some("text");
                self.set_job(ix, req, Stage::Eval(seq, if text { EvalKind::Text } else { EvalKind::Snapshot }));
                native.eval_agent(seq, &format!("snapshot({})", js(&serde_json::json!({ "mode": if text { "text" } else { "interactive" } }))));
            }
            "click" | "type" | "select" | "press" | "scroll" => {
                let op = req.op.clone();
                self.set_job(ix, req, Stage::Eval(seq, EvalKind::Act));
                native.eval_agent(seq, &format!("act({}, {})", js(&op.into()), js(&p)));
            }
            "wait_for" => {
                let ms = p["timeout_ms"].as_u64().map(Duration::from_millis).unwrap_or(WAIT_DEFAULT).min(WAIT_MAX);
                let ms = if p["seconds"].is_number() { Duration::from_secs_f64(p["seconds"].as_f64().unwrap_or(0.).clamp(0., 30.)) } else { ms };
                self.set_job(ix, req, Stage::Poll { until: Instant::now() + ms, check: None });
                self.agent_tick(tab_id, window, cx);
            }
            "handle_dialog" => {
                let accept = p["accept"].as_bool().unwrap_or(true);
                let text = p["text"].as_str().map(str::to_string);
                let info = self.browser.tabs[ix].agent.as_mut().and_then(|a| a.dialog.take()).map(|(d, _)| d);
                self.set_job(ix, req, Stage::Settle(Instant::now() + SETTLE));
                match (native.answer_dialog(accept, text.as_deref()), info) {
                    (Some(kind), info) => {
                        let what = info.map(|d| format!(" {:?}", d.message.chars().take(120).collect::<String>())).unwrap_or_default();
                        let answer = if !accept { "dismissed".to_string() } else if let Some(t) = text.filter(|_| kind == crate::browser::agent::DialogKind::Prompt) { format!("answered {t:?} to") } else { "accepted".to_string() };
                        if let Some(job) = self.job_mut(ix) {
                            job.outcome = format!("{answer} the {} dialog{what}", kind.name());
                        }
                        self.agent_after(tab_id, SETTLE, cx);
                    }
                    (None, _) => self.finish_job(ix, Err("No dialog is open on this chat's page.".into()), window, cx),
                }
            }
            "upload" => {
                let files: Vec<std::path::PathBuf> =
                    p["files"].as_array().map(|a| a.iter().filter_map(|f| f.as_str()).map(std::path::PathBuf::from).collect()).unwrap_or_default();
                if files.is_empty() {
                    self.set_job(ix, req, Stage::Settle(Instant::now()));
                    return self.finish_job(ix, Err("files is required".into()), window, cx);
                }
                native.set_upload(Some(files));
                self.set_job(ix, req, Stage::Eval(seq, EvalKind::UploadTarget));
                native.eval_agent(seq, &format!("act('upload_target', {})", js(&p)));
            }
            "screenshot" => {
                let full_page = p["full_page"].as_bool().unwrap_or(false);
                let shot = ShotRequest { request_id: req.request_id.clone(), client: req.client.clone(), full_page, full: None, frames: Vec::new(), asked_y: 0., at_y: 0. };
                self.set_job(ix, req, Stage::Shot(shot));
                if full_page {
                    native.request_page_size();
                } else {
                    native.request_screenshot(false);
                }
            }
            other => {
                let err = format!("unknown browser op: {other}");
                self.set_job(ix, req, Stage::Settle(Instant::now()));
                self.finish_job(ix, Err(err), window, cx)
            }
        }
    }

    /// Load `req.url` in the chat's tab without opening the pane; the reply
    /// waits for the load and carries the page's snapshot.
    fn agent_navigate(&mut self, ix: usize, req: HostRequest, sites: &[String], window: &mut Window, cx: &mut Context<Self>) {
        let Some(url) = normalize_address(&req.url) else {
            self.set_job(ix, req, Stage::Settle(Instant::now()));
            return self.finish_job(ix, Err("url was rejected (use http/https)".into()), window, cx);
        };
        if !site_allowed(&url, sites) {
            self.set_job(ix, req, Stage::Settle(Instant::now()));
            let err = format!("{url} isn't on the allowed sites list (Settings → Browser): {}", sites.join(", "));
            return self.finish_job(ix, Err(err), window, cx);
        }
        if !self.ensure_native_page(ix, window, cx) {
            let err = self.browser.tabs[ix].error.clone().unwrap_or_else(|| "the browser couldn't start".into());
            self.set_job(ix, req, Stage::Settle(Instant::now()));
            return self.finish_job(ix, Err(err), window, cx);
        }
        if ix == self.browser.active && !self.browser.address.read(cx).focus_handle(cx).is_focused(window) {
            let shown = url.clone();
            self.browser.address.update(cx, |s, cx| s.set_value(shown, window, cx));
        }
        let tab = &mut self.browser.tabs[ix];
        if let Some(native) = &tab.native {
            native.set_background(true);
            native.set_agent(true, sites);
            native.load(&url);
        }
        tab.parked = false;
        tab.restore = None;
        tab.preview = None;
        tab.web_url = Some(url.clone());
        tab.loading(url);
        let tab_id = tab.id;
        self.set_job(ix, req, Stage::Load(Instant::now() + LOAD_WAIT));
        if let Some(job) = self.job_mut(ix) {
            job.want = SnapWant::parse(job.req.params["snapshot"].as_str(), SnapWant::Full);
            job.outcome = String::new();
        }
        self.agent_after(tab_id, LOAD_WAIT, cx);
        self.pump_load(ix, window, cx);
        cx.notify();
    }

    pub(super) fn reply_browser(client: keel_rpc::RpcClient, request_id: String, result: Result<serde_json::Value, String>, cx: &mut Context<Self>) {
        cx.spawn(async move |_, _| {
            let _ = client.browser_complete(&request_id, result).await;
        })
        .detach();
    }

    /// The reply's common fields.
    fn page_reply(&self, ix: usize, text: String) -> serde_json::Value {
        let tab = &self.browser.tabs[ix];
        serde_json::json!({
            "url": tab.page.url,
            "title": tab.page.title,
            "text": text,
            "open": self.browser.open && self.browser.active == ix,
        })
    }

    /// Reply to the running job and start the chat's next request.
    fn finish_job(&mut self, ix: usize, result: Result<serde_json::Value, String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(a) = self.browser.tabs.get_mut(ix).and_then(|t| t.agent.as_mut()) else {
            return;
        };
        let Some(job) = a.job.take() else {
            return;
        };
        if let Stage::Shot(shot) = &job.stage
            && let Some(full) = shot.full
            && let Some(native) = &self.browser.tabs[ix].native
        {
            native.finish_full_page(full.sx, full.sy);
        }
        if matches!(job.stage, Stage::Upload { .. } | Stage::Eval(_, EvalKind::UploadTarget))
            && let Some(native) = &self.browser.tabs[ix].native
        {
            native.set_upload(None);
        }
        tracing::debug!(target: "keel_ui::browser", op = %job.req.op, ms = job.started.elapsed().as_millis() as u64, ok = result.is_ok(), "browser job done");
        Self::reply_browser(job.req.client, job.req.request_id, result, cx);
        self.pump(ix, window, cx);
    }

    fn pump(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let next = self.browser.tabs.get_mut(ix).and_then(|t| t.agent.as_mut()).and_then(|a| a.queue.done());
        if let Some(next) = next {
            self.start_job(ix, next, window, cx);
        }
    }

    /// Fail every request a tab holds: it went away.
    pub(super) fn drop_agent_tab(&mut self, ix: usize, why: &str, cx: &mut Context<Self>) {
        let Some(a) = self.browser.tabs.get_mut(ix).and_then(|t| t.agent.as_mut()) else {
            return;
        };
        let mut reqs: Vec<HostRequest> = a.queue.clear();
        if let Some(job) = a.job.take() {
            reqs.insert(0, job.req);
        }
        for r in reqs {
            Self::reply_browser(r.client, r.request_id, Err(why.to_string()), cx);
        }
    }

    /// Timer: move the tab's job (or dialog) along if its time has come.
    fn agent_tick(&mut self, tab_id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.browser.tab_ix(tab_id) else {
            return;
        };
        let now = Instant::now();
        // An unanswered dialog gets the default answer.
        if let Some(a) = self.browser.tabs[ix].agent.as_mut()
            && a.dialog.as_ref().is_some_and(|(_, at)| now >= *at)
        {
            let (info, _) = a.dialog.take().expect("checked");
            if let Some(native) = &self.browser.tabs[ix].native
                && native.answer_dialog(DIALOG_DEFAULT_ACCEPT, None).is_some()
            {
                let a = self.browser.tabs[ix].agent.as_mut().expect("agent");
                a.notes.push(format!(
                    "{} — nobody answered in {DIALOG_TIMEOUT_SECS}s, so it was {} (the default).",
                    info.describe(),
                    if DIALOG_DEFAULT_ACCEPT { "accepted" } else { "dismissed" }
                ));
            }
        }
        let loading = self.browser.tabs[ix].page.loading;
        let page_url = self.browser.tabs[ix].page.url.clone();
        let Some(job) = self.job_mut(ix) else {
            return;
        };
        match job.stage {
            Stage::Settle(at) if now >= at => {
                let _left_page = job.url_before != page_url;
                if loading {
                    let until = now + NAV_WAIT;
                    if let Some(job) = self.job_mut(ix) {
                        job.stage = Stage::Load(until);
                    }
                    self.agent_after(tab_id, NAV_WAIT, cx);
                } else {
                    self.final_snapshot(ix, window, cx);
                }
            }
            Stage::Load(until) => {
                if !loading {
                    self.final_snapshot(ix, window, cx);
                } else if now >= until {
                    job.late = true;
                    self.final_snapshot(ix, window, cx);
                }
            }
            Stage::Unpark(until) => {
                if !loading || now >= until {
                    let a = self.browser.tabs[ix].agent.as_mut().expect("agent");
                    let req = a.job.take().expect("job").req;
                    self.start_job(ix, req, window, cx);
                }
            }
            Stage::Poll { until, check } => self.poll_wait(ix, until, check, window, cx),
            Stage::Upload { until, file_input, .. } if now >= until => {
                #[cfg(target_os = "macos")]
                if let Some(native) = &self.browser.tabs[ix].native {
                    native.set_upload(None);
                }
                if file_input {
                    self.upload_fallback(ix, window, cx);
                } else {
                    let target = match &self.job_mut(ix).map(|j| &j.stage) {
                        Some(Stage::Upload { target, .. }) => target.clone(),
                        _ => String::new(),
                    };
                    let err = format!("Clicking {target} didn't open a file chooser. Pass the ref of the page's file input (role \"file\" in the snapshot) instead.");
                    self.finish_job(ix, Err(err), window, cx);
                }
            }
            _ => {}
        }
    }

    /// One look at a `wait_for` condition.
    fn poll_wait(&mut self, ix: usize, until: Instant, check: Option<u64>, window: &mut Window, cx: &mut Context<Self>) {
        if check.is_some() {
            return;
        }
        let tab_id = self.browser.tabs[ix].id;
        let page = self.browser.tabs[ix].page.clone();
        let Some(job) = self.job_mut(ix) else { return };
        let p = job.req.params.clone();
        let mut pending: Vec<String> = Vec::new();
        if let Some(pattern) = p["url"].as_str().filter(|s| !s.is_empty())
            && !page.url.as_deref().is_some_and(|u| url_matches(u, pattern))
        {
            pending.push(format!("the URL to match {pattern:?} (it is {})", page.url.as_deref().unwrap_or("blank")));
        }
        if p["load"].as_bool().unwrap_or(false) && page.loading {
            pending.push("the page to finish loading".into());
        }
        let dom = p["text"].is_string() || p["ref"].is_string() || p["selector"].is_string();
        let timer_only = p["seconds"].is_number() && !dom && pending.is_empty();
        let now = Instant::now();
        if timer_only {
            if now >= until {
                job.outcome = format!("waited {:.1}s", job.started.elapsed().as_secs_f32());
                return self.final_snapshot(ix, window, cx);
            }
            return self.agent_after(tab_id, until - now, cx);
        }
        if !pending.is_empty() {
            if now >= until {
                let err = format!("Timed out after {:.1}s waiting for {}.", job.started.elapsed().as_secs_f32(), pending.join(" and "));
                return self.finish_job(ix, Err(err), window, cx);
            }
            return self.agent_after(tab_id, POLL, cx);
        }
        if !dom {
            job.outcome = format!("waited {:.1}s", job.started.elapsed().as_secs_f32());
            return self.final_snapshot(ix, window, cx);
        }
        {
            let seq = self.next_seq();
            if let Some(job) = self.job_mut(ix) {
                job.stage = Stage::Poll { until, check: Some(seq) };
            }
            if let Some(native) = self.browser.tabs[ix].native.clone() {
                native.eval_agent(seq, &format!("check({})", js(&p)));
            }
        }
    }

    /// Take the snapshot the reply ends with.
    fn final_snapshot(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let want = self.job_mut(ix).map(|j| j.want).unwrap_or(SnapWant::Diff);
        if want == SnapWant::None {
            return self.reply_with_snapshot(ix, None, window, cx);
        }
        if let Some(native) = self.browser.tabs[ix].native.clone() {
            let seq = self.next_seq();
            if let Some(job) = self.job_mut(ix) {
                job.stage = Stage::Final(seq);
            }
            native.eval_agent(seq, "snapshot({})");
            return;
        }
        self.reply_with_snapshot(ix, None, window, cx);
    }

    /// The action's reply: what it did, where the page went, what changed,
    /// and anything the page did meanwhile (dialogs, console errors).
    fn reply_with_snapshot(&mut self, ix: usize, snap: Option<Snap>, window: &mut Window, cx: &mut Context<Self>) {
        let unseen = self.browser.tabs[ix].console.take_unseen_errors();
        let page_url = self.browser.tabs[ix].page.url.clone();
        let Some(a) = self.browser.tabs[ix].agent.as_mut() else { return };
        let Some(job) = a.job.as_mut() else { return };
        let mut text = job.outcome.clone();
        let navigated = job.req.op != "navigate" && job.url_before.is_some() && job.url_before != page_url;
        if navigated && let Some(url) = &page_url {
            push_line(&mut text, &format!("The page navigated to {url}."));
        }
        if job.late {
            push_line(&mut text, "The page was still loading when this reply was sent; what has rendered so far:");
        }
        if let Some(snap) = snap {
            let page = snapshot_reply(a.last_snap.as_ref(), &snap, job.want);
            if !page.is_empty() {
                if !text.is_empty() {
                    text.push('\n');
                }
                push_line(&mut text, &page);
            }
            a.last_snap = Some(snap);
        }
        for note in a.notes.drain(..) {
            push_line(&mut text, &note);
        }
        if unseen > 0 {
            push_line(&mut text, &format!("({unseen} new console error{}; browser_console shows them.)", if unseen == 1 { "" } else { "s" }));
        }
        let extra = std::mem::take(&mut job.extra);
        let mut reply = self.page_reply(ix, text);
        for (k, v) in extra {
            reply[k] = v;
        }
        self.finish_job(ix, Ok(reply), window, cx);
    }

    /// `upload` fallback: the page's file input gets the files from script.
    fn upload_fallback(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(job) = self.job_mut(ix) else { return };
        let p = job.req.params.clone();
        let files: Vec<std::path::PathBuf> = p["files"].as_array().map(|a| a.iter().filter_map(|f| f.as_str()).map(std::path::PathBuf::from).collect()).unwrap_or_default();
        let mut total = 0u64;
        let mut list = Vec::new();
        for f in &files {
            let size = std::fs::metadata(f).map(|m| m.len()).unwrap_or(0);
            total += size;
            if total > UPLOAD_MAX_BYTES {
                let err = format!("The files are over {} MB together; the page's chooser didn't open, and setting them from script is capped there.", UPLOAD_MAX_BYTES / 1024 / 1024);
                return self.finish_job(ix, Err(err), window, cx);
            }
            let bytes = match std::fs::read(f) {
                Ok(b) => b,
                Err(e) => return self.finish_job(ix, Err(format!("can't read {}: {e}", f.display())), window, cx),
            };
            use base64::Engine;
            list.push(serde_json::json!({
                "name": f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                "type": mime_for(f),
                "data": base64::engine::general_purpose::STANDARD.encode(bytes),
            }));
        }
        {
            let seq = self.next_seq();
            if let Some(job) = self.job_mut(ix) {
                job.stage = Stage::Eval(seq, EvalKind::UploadFallback);
            }
            let mut args = serde_json::json!({ "files": list });
            for k in ["ref", "selector"] {
                if let Some(v) = p.get(k) {
                    args[k] = v.clone();
                }
            }
            if let Some(native) = self.browser.tabs[ix].native.clone() {
                native.eval_agent(seq, &format!("upload({})", js(&args)));
            }
        }
    }

    /// `browser_open_pane`: shows the pane on the chat's tab, when the user
    /// is in that chat.
    fn host_ensure_open(&mut self, ix: usize, chat: &str, window: &mut Window, cx: &mut Context<Self>) -> Result<serde_json::Value, String> {
        let here = chat.is_empty() || self.current_chat.as_deref() == Some(chat);
        let allowed = self.browser_allowed() && here;
        if allowed && !self.browser.open {
            if self.files.open {
                self.toggle_files(window, cx);
            }
            if self.terminal.open {
                self.set_terminal_open(false, window, cx);
            }
            self.update_right_panel_anim();
            let b = &mut self.browser;
            b.open = true;
            let n = b.anim.map_or(1, |(_, n)| n + 1);
            b.anim = Some((Instant::now(), n));
            b.expanded = false;
        }
        if allowed {
            self.activate_tab(ix, window, cx);
        }
        cx.notify();
        let page = self.browser.tabs[ix].page.clone();
        let text = if !here {
            "The user is in another chat, so the pane shows that chat's tab; this chat's tab keeps working in the background."
        } else if !allowed {
            "The pane can't show on the screen the user is on; the other browser tools work anyway."
        } else {
            ""
        };
        Ok(serde_json::json!({ "url": page.url, "title": page.title, "text": text, "open": self.browser.open && allowed }))
    }

    /// Page events that belong to an agent's job. False when not handled.
    pub(super) fn on_agent_event(&mut self, ix: usize, event: &mut Option<crate::browser::NativeEvent>, window: &mut Window, cx: &mut Context<Self>) {
        use crate::browser::NativeEvent;
        let tab_id = self.browser.tabs[ix].id;
        match event.take() {
            Some(NativeEvent::Console(entry)) => self.browser.tabs[ix].console.push(entry),
            Some(NativeEvent::Blocked(url)) => {
                if let Some(a) = self.browser.tabs[ix].agent.as_mut() {
                    a.notes.push(format!("Blocked a load of {url}: it isn't on the allowed sites list (Settings → Browser)."));
                }
            }
            Some(NativeEvent::Dialog { info, held }) => {
                let Some(a) = self.browser.tabs[ix].agent.as_mut() else { return };
                if !held {
                    a.notes.push(format!("{} — accepted automatically.", info.describe()));
                    return;
                }
                a.dialog = Some((info.clone(), Instant::now() + Duration::from_secs(DIALOG_TIMEOUT_SECS)));
                self.agent_after(tab_id, Duration::from_secs(DIALOG_TIMEOUT_SECS), cx);
                // A job waiting on the paused page answers now.
                let Some(job) = self.job_mut(ix) else { return };
                if matches!(job.stage, Stage::Shot(_)) {
                    return;
                }
                let did = if job.outcome.is_empty() { format!("The {} is waiting on a dialog.", job.req.op) } else { job.outcome.clone() };
                let mut text = did;
                push_line(&mut text, &info.ask());
                let reply = self.page_reply(ix, text);
                self.finish_job(ix, Ok(reply), window, cx);
            }
            Some(NativeEvent::FilePicker(got)) => {
                let Some(job) = self.job_mut(ix) else {
                    if got.is_none()
                        && let Some(a) = self.browser.tabs[ix].agent.as_mut()
                    {
                        a.notes.push("The page asked for a file chooser; none was shown. Use browser_upload with the file input's ref to choose files.".into());
                    }
                    return;
                };
                if let (Some(n), Stage::Upload { .. } | Stage::Eval(_, EvalKind::UploadTarget)) = (got, &job.stage) {
                    let target = match &job.stage {
                        Stage::Upload { target, .. } => target.clone(),
                        _ => "the page".into(),
                    };
                    let names: Vec<String> = job.req.params["files"].as_array().map(|a| a.iter().filter_map(|f| f.as_str()).filter_map(|f| std::path::Path::new(f).file_name().map(|n| n.to_string_lossy().into_owned())).collect()).unwrap_or_default();
                    job.outcome = format!("uploaded {n} file{} ({}) into {target}", if n == 1 { "" } else { "s" }, names.into_iter().take(n).collect::<Vec<_>>().join(", "));
                    job.stage = Stage::Settle(Instant::now() + SETTLE);
                    self.agent_after(tab_id, SETTLE, cx);
                }
            }
            Some(NativeEvent::Eval(seq, raw)) => self.on_eval(ix, seq, raw, window, cx),
            other => *event = other,
        }
    }

    #[cfg(target_os = "macos")]
    fn on_eval(&mut self, ix: usize, seq: u64, raw: Result<String, String>, window: &mut Window, cx: &mut Context<Self>) {
        let tab_id = self.browser.tabs[ix].id;
        // Unloading an idle page: its scroll position, kept for its return.
        if self.browser.tabs[ix].park_seq == Some(seq) {
            return self.finish_park(ix, raw.ok(), cx);
        }
        let page = self.browser.tabs[ix].page.clone();
        let Some(job) = self.job_mut(ix) else { return };
        let stage = match &job.stage {
            Stage::Eval(s, kind) if *s == seq => Some(*kind),
            _ => None,
        };
        let job = self.job_mut(ix).expect("job");
        if let Some(kind) = stage {
            let value = match raw.and_then(|b| parse_eval(&b)) {
                Ok(v) => v,
                Err(e) => return self.finish_job(ix, Err(e), window, cx),
            };
            if value["ok"].as_bool() != Some(true) {
                let err = value["error"].as_str().unwrap_or("the page action failed").to_string();
                return self.finish_job(ix, Err(err), window, cx);
            }
            match kind {
                EvalKind::Snapshot | EvalKind::Text => {
                    let text = value["text"].as_str().unwrap_or_default().to_string();
                    let url = value["url"].as_str().map(str::to_string).or(page.url);
                    let reply_text = if kind == EvalKind::Text {
                        text
                    } else {
                        let snap = Snap { url: url.clone().unwrap_or_default(), text: text.clone() };
                        if let Some(a) = self.browser.tabs[ix].agent.as_mut() {
                            a.last_snap = Some(snap);
                        }
                        if text.is_empty() { "(no interactive elements; try mode \"text\")".into() } else { text }
                    };
                    let mut reply = self.page_reply(ix, reply_text);
                    reply["url"] = url.into();
                    if let Some(t) = value["title"].as_str() {
                        reply["title"] = t.into();
                    }
                    self.finish_job(ix, Ok(reply), window, cx);
                }
                EvalKind::Act | EvalKind::UploadFallback => {
                    job.outcome = value["text"].as_str().unwrap_or_default().to_string();
                    if value["scroll"].is_object() {
                        job.extra.insert("scroll".into(), value["scroll"].clone());
                    }
                    job.stage = Stage::Settle(Instant::now() + SETTLE);
                    self.agent_after(tab_id, SETTLE, cx);
                }
                EvalKind::UploadTarget => {
                    let info = value["text"].as_str().and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok()).unwrap_or_default();
                    job.stage = Stage::Upload {
                        until: Instant::now() + UPLOAD_WAIT,
                        file_input: info["file_input"].as_bool().unwrap_or(false),
                        target: info["label"].as_str().unwrap_or("the element").to_string(),
                    };
                    self.agent_after(tab_id, UPLOAD_WAIT, cx);
                }
            }
            return;
        }
        match job.stage {
            Stage::Final(s) if s == seq => {
                let snap = raw.ok().and_then(|b| parse_eval(&b).ok()).filter(|v| v["ok"].as_bool() == Some(true)).map(|v| Snap {
                    url: v["url"].as_str().map(str::to_string).or(page.url.clone()).unwrap_or_default(),
                    text: v["text"].as_str().unwrap_or_default().to_string(),
                });
                self.reply_with_snapshot(ix, snap, window, cx);
            }
            Stage::Poll { until, check: Some(s) } if s == seq => {
                let value = raw.and_then(|b| parse_eval(&b)).unwrap_or_default();
                let detail = value["detail"].as_str().unwrap_or_default().to_string();
                if value["ok"].as_bool() == Some(true) {
                    job.outcome = format!("waited {:.1}s: {detail}", job.started.elapsed().as_secs_f32());
                    return self.final_snapshot(ix, window, cx);
                }
                if Instant::now() >= until {
                    let err = format!("Timed out after {:.1}s: {detail} (page {}).", job.started.elapsed().as_secs_f32(), page.url.as_deref().unwrap_or("blank"));
                    return self.finish_job(ix, Err(err), window, cx);
                }
                job.stage = Stage::Poll { until, check: None };
                self.agent_after(tab_id, POLL, cx);
            }
            _ => {}
        }
    }

    /// The running screenshot, taken out of the job (put back with
    /// [`Self::put_shot`] while frames remain).
    pub(super) fn take_shot(&mut self, ix: usize) -> Option<ShotRequest> {
        let job = self.job_mut(ix)?;
        if !matches!(job.stage, Stage::Shot(_)) {
            return None;
        }
        match std::mem::replace(&mut job.stage, Stage::Settle(Instant::now())) {
            Stage::Shot(shot) => Some(shot),
            _ => None,
        }
    }

    pub(super) fn put_shot(&mut self, ix: usize, shot: ShotRequest) {
        if let Some(job) = self.job_mut(ix) {
            job.stage = Stage::Shot(shot);
        }
    }

    /// The screenshot replies on its own (the stitch runs in the
    /// background); the chat's next request needn't wait for it.
    pub(super) fn shot_handed_off(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(a) = self.browser.tabs.get_mut(ix).and_then(|t| t.agent.as_mut()) {
            a.job = None;
        }
        self.pump(ix, window, cx);
    }

    /// A screenshot failed: the job replies with the error.
    pub(super) fn shot_failed(&mut self, ix: usize, err: String, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_job(ix, Err(err), window, cx);
    }

    /// The tab finished loading: a job waiting on it moves on.
    pub(super) fn agent_page_loaded(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        // An unloaded page that came back: its scroll position too.
        if let Some((x, y)) = self.browser.tabs[ix].restore.take()
            && let Some(native) = self.browser.tabs[ix].native.clone()
        {
            let seq = self.next_seq();
            native.eval_script(seq, &format!("window.scrollTo({x}, {y}); null"));
        }
        let waiting = self.job_mut(ix).is_some_and(|j| matches!(j.stage, Stage::Load(_) | Stage::Unpark(_)));
        if waiting {
            let tab_id = self.browser.tabs[ix].id;
            self.agent_tick(tab_id, window, cx);
        }
    }

    /// Unload `ix`'s page after idle, keeping where it was scrolled.
    pub(super) fn start_park(&mut self, ix: usize) {
        let seq = self.next_seq();
        let tab = &mut self.browser.tabs[ix];
        if tab.park_seq.is_some() {
            return;
        }
        if let Some(native) = tab.native.clone() {
            tab.park_seq = Some(seq);
            native.eval_script(seq, "JSON.stringify({x: scrollX, y: scrollY})");
        }
    }

    fn finish_park(&mut self, ix: usize, raw: Option<String>, cx: &mut Context<Self>) {
        let tab = &mut self.browser.tabs[ix];
        tab.park_seq = None;
        let busy = tab.agent.as_ref().is_some_and(AgentTab::busy);
        let shown = self.browser.open && self.browser.active == ix;
        if busy || shown || tab.native.is_none() {
            return;
        }
        let pos = raw.as_deref().and_then(|b| parse_eval(b).ok()).and_then(|v| Some((v["x"].as_f64()?, v["y"].as_f64()?)));
        tab.restore = pos.filter(|(x, y)| *x > 0. || *y > 0.);
        tab.native = None;
        tab._events = None;
        tab.parked = true;
        tab.preview = None;
        tab.page.url = tab.web_url.clone();
        tab.page.loading = false;
        tracing::info!(target: "keel_ui::browser", url = ?tab.web_url, "idle page unloaded");
        cx.notify();
    }
}

fn push_line(text: &mut String, line: &str) {
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(line);
}

fn mime_for(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("pdf") => "application/pdf",
        Some("txt" | "md") => "text/plain",
        Some("csv") => "text/csv",
        Some("json") => "application/json",
        Some("html" | "htm") => "text/html",
        Some("zip") => "application/zip",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_for_url_patterns() {
        assert!(url_matches("https://a.io/checkout?x=1", "/checkout"));
        assert!(url_matches("https://a.io/orders/42", "*/orders/*"));
        assert!(url_matches("https://a.io/orders/42", "https://a.io/*"));
        assert!(!url_matches("https://a.io/cart", "*/orders/*"));
        assert!(url_matches("https://a.io/done", "*done"));
        assert!(!url_matches("https://a.io/done/x", "*done"));
    }
}

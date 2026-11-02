//! Settings (⌘,): the model providers Keel's agent is signed in to and connected services (GitLab on gitlab.com or a
//! self-hosted domain). Sign-in happens in Pi's own `/login`, run against
//! Keel's Pi folder; Keel only lists what is stored there.

use gpui::{AnyElement, ClickEvent, Context, ElementId, IntoElement, div, prelude::*};
use keel_proto::ThemeChoice;
use keel_theme::Tokens;
use crate::style::zpx;

use super::KeelWindow;
use super::sidebar::{filled_button, text_button};
use crate::icons::Icon;
use crate::style::{KeelStyled, MONO, RADIUS_MD, RADIUS_SM, TRANSCRIPT_MAX_WIDTH, c};

/// One agent CLI as `harness_status` reports it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct HarnessStatus {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub installed: bool,
    pub version: String,
    pub path: Option<String>,
    /// `None` until a sign-in check is asked for.
    pub accounts: Option<Vec<Account>>,
}

/// A sign-in in progress, as `sign_in_status` last reported it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct SignInUi {
    /// `starting`, `browser`, `device`, `working`, `done`, `failed`, `idle`.
    pub state: String,
    pub url: String,
    pub code: String,
    pub message: String,
}

impl SignInUi {
    fn parse(v: &serde_json::Value) -> Self {
        let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
        Self { state: s("state"), url: s("url"), code: s("code"), message: s("message") }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Account {
    /// Pi's provider id, for the logo and the sign-in watch.
    pub id: String,
    pub name: String,
    pub hint: String,
    pub signed_in: bool,
    pub detail: String,
}

impl Account {
    /// The stored credential, in words: "Subscription" for an OAuth sign-in.
    fn credential(&self) -> String {
        match self.detail.as_str() {
            "oauth" => "Subscription".into(),
            "api_key" => "API key".into(),
            other => other.to_string(),
        }
    }
}

/// One connected service as `list_connections` reports it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ConnectionRow {
    pub kind: String,
    pub host: String,
    pub username: String,
    pub name: String,
}

impl HarnessStatus {
    fn parse(v: &serde_json::Value) -> Self {
        let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
        Self {
            id: s("id"),
            display_name: s("display_name"),
            description: s("description"),
            installed: v["installed"].as_bool().unwrap_or(false),
            version: s("version"),
            path: v["path"].as_str().map(str::to_string),
            accounts: v["accounts"].as_array().map(|rows| {
                rows.iter()
                    .map(|a| Account {
                        id: a["id"].as_str().unwrap_or_default().to_string(),
                        name: a["name"].as_str().unwrap_or_default().to_string(),
                        hint: a["hint"].as_str().unwrap_or_default().to_string(),
                        signed_in: a["signed_in"].as_bool().unwrap_or(false),
                        detail: a["detail"].as_str().unwrap_or_default().to_string(),
                    })
                    .collect()
            }),
        }
    }
}

impl ConnectionRow {
    pub(super) fn parse(v: &serde_json::Value) -> Self {
        let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
        Self { kind: s("kind"), host: s("host"), username: s("username"), name: s("name") }
    }
}

impl KeelWindow {
    pub(super) fn open_settings(&mut self, cx: &mut Context<Self>) {
        self.state.show_overflow_menu = false;
        self.gitlab_open = false;
        self.settings_leaving = None;
        self.settings_gen += 1;
        self.settings_open = true;
        self.browser_sites_synced = false;
        // Sign-in is read from Keel's Pi folder: instant, so always fresh.
        self.load_agent_status(cx);
        self.load_connections(cx);
        cx.notify();
    }

    pub(super) fn close_settings(&mut self, cx: &mut Context<Self>) {
        self.close_pages();
        self.finish_settings_exit(cx);
        self.show_sound_menu = false;
        self.mark_current_read(cx);
        cx.notify();
    }

    pub(super) fn toggle_settings(&mut self, cx: &mut Context<Self>) {
        let now = std::time::Instant::now();
        if let Some(last) = self.last_settings_toggle {
            if now.duration_since(last) < std::time::Duration::from_millis(300) {
                return;
            }
        }
        self.last_settings_toggle = Some(now);

        if self.settings_open || self.settings_leaving.is_some() {
            self.close_settings(cx);
        } else {
            self.open_settings(cx);
        }
    }

    /// Keel's Pi and which providers it is signed in to.
    pub(super) fn load_agent_status(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let res = client.call("harness_status", serde_json::json!({ "check_sign_in": true })).await;
            let _ = this.update(cx, |this, cx| {
                match res {
                    Ok(v) => {
                        let rows = v["harnesses"].as_array().map(|r| r.iter().map(HarnessStatus::parse).collect());
                        this.harness_status = Some(rows.unwrap_or_default());
                    }
                    Err(err) => this.report("couldn't check the agents", err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Sign in to provider `id` on Keel's Pi: the engine runs Pi's own
    /// login, Keel opens the browser (or copies GitHub's device code) and
    /// follows it here until it lands, fails, or is cancelled.
    fn start_sign_in(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.sign_in = Some((id.clone(), SignInUi { state: "starting".into(), ..Default::default() }));
        cx.notify();
        cx.spawn(async move |this, cx| {
            if let Err(err) = client.call("sign_in", serde_json::json!({ "provider": id })).await {
                let _ = this.update(cx, |this, cx| {
                    this.sign_in = Some((id.clone(), SignInUi { state: "failed".into(), message: err, ..Default::default() }));
                    cx.notify();
                });
                return;
            }
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(500)).await;
                let Ok(v) = client.call("sign_in_status", serde_json::json!({ "provider": id })).await else {
                    continue;
                };
                let next = SignInUi::parse(&v);
                let over = this
                    .update(cx, |this, cx| {
                        // Cancelled, or another provider's sign-in took over.
                        let Some(last) = this.sign_in.as_ref().filter(|(p, _)| *p == id).map(|(_, s)| s.clone()) else {
                            return true;
                        };
                        // Hand the user over once per step: open the page,
                        // and put GitHub's code on the clipboard first.
                        let fresh = last.state != next.state || last.url != next.url;
                        if fresh && next.state == "device" {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(next.code.clone()));
                        }
                        if fresh && matches!(next.state.as_str(), "browser" | "device") && !next.url.is_empty() {
                            cx.open_url(&next.url);
                        }
                        let over = match next.state.as_str() {
                            "done" => {
                                this.switch_gen += 1;
                                this.auth_landed = Some((id.clone(), this.switch_gen));
                                this.sign_in = None;
                                this.load_agent_status(cx);
                                this.refresh_models(cx);
                                true
                            }
                            "idle" => {
                                this.sign_in = None;
                                true
                            }
                            state => {
                                let failed = state == "failed";
                                this.sign_in = Some((id.clone(), next));
                                failed
                            }
                        };
                        cx.notify();
                        over
                    })
                    .unwrap_or(true);
                if over {
                    break;
                }
            }
        })
        .detach();
    }

    /// Drop every cached model list and fetch the current harness's again,
    /// after a sign-in added a provider's models. The engine dropped its
    /// cache when the sign-in finished.
    fn refresh_models(&mut self, cx: &mut Context<Self>) {
        self.models.clear();
        self.models_ready_at.clear();
        self.models_error.clear();
        let harness = self.selected_harness.clone();
        if !harness.is_empty() {
            self.ensure_models(harness, cx);
        }
    }

    fn cancel_sign_in(&mut self, cx: &mut Context<Self>) {
        let Some((id, _)) = self.sign_in.take() else {
            return;
        };
        if let Some(client) = self.client.clone() {
            cx.spawn(async move |_, _| {
                let _ = client.call("sign_in_cancel", serde_json::json!({ "provider": id })).await;
            })
            .detach();
        }
        cx.notify();
    }

    pub(super) fn load_connections_pub(&mut self, cx: &mut Context<Self>) {
        self.load_connections(cx);
    }

    fn load_connections(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            if let Ok(v) = client.call("list_connections", serde_json::json!({})).await {
                let _ = this.update(cx, |this, cx| {
                    this.connections =
                        v["connections"].as_array().map(|r| r.iter().map(ConnectionRow::parse).collect()).unwrap_or_default();
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Check the token with GitLab and keep it on success.
    fn connect_gitlab(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let host = self.gitlab_host.read(cx).value().trim().to_string();
        let host = if host.is_empty() { "https://gitlab.com".to_string() } else { host };
        let token = self.gitlab_token.read(cx).value().trim().to_string();
        let Some(client) = self.client.clone() else {
            return;
        };
        self.connect_busy = true;
        self.connect_error = None;
        // The token lives in the engine from here; clear the field now.
        self.gitlab_token.update(cx, |input, cx| input.set_value("", window, cx));
        cx.spawn(async move |this, cx| {
            let res = client.call("connect_gitlab", serde_json::json!({ "host": host, "token": token })).await;
            let _ = this.update(cx, |this, cx| {
                this.connect_busy = false;
                match res {
                    Ok(_) => this.load_connections(cx),
                    Err(err) => this.connect_error = Some(err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn disconnect(&mut self, kind: String, host: String, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let res = client.call("disconnect", serde_json::json!({ "kind": kind, "host": host })).await;
            let _ = this.update(cx, |this, cx| {
                match res {
                    Ok(_) => this.load_connections(cx),
                    Err(err) => this.report("disconnect failed", err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Remember that switch `id` just flipped, so it slides instead of snapping.
    fn note_switch_flip(&mut self, id: &str) {
        self.switch_gen += 1;
        self.switch_flips.retain(|_, (_, at)| at.elapsed() < std::time::Duration::from_secs(1));
        self.switch_flips.insert(id.to_string(), (self.switch_gen, std::time::Instant::now()));
    }

    /// The flip number when switch `id` changed just now (and motion is on).
    fn recent_flip(&self, id: &str) -> Option<u64> {
        self.switch_flips
            .get(id)
            .filter(|(_, at)| !self.reduced() && at.elapsed() < crate::motion::BASE * 2)
            .map(|(n, _)| *n)
    }

    pub(super) fn render_settings(&mut self, t: Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        // A column, not a row: a row stretches the page to the viewport's height,
        // so it never overflows and there's nothing to scroll.
        div()
            .id("settings-scroll")
            .track_scroll(&self.dotify.settings_scroll)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .items_center()
            .child(
                div()
                    .w_full()
                    .max_w(TRANSCRIPT_MAX_WIDTH)
                    .px_8()
                    .pt_6()
                    .pb_16()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .pb_6()
                            .child(
                                div()
                                    .text_size(zpx(18.))
                                    .line_height(zpx(24.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(c(t.text))
                                    .child("Settings"),
                            )
                            .child(text_button("settings-done-btn", "Done", t).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.close_settings(cx);
                            }))),
                    )
                    .child(self.render_appearance_section(t, cx))
                    .child(section_divider(t))
                    .child(self.render_notifications_section(t, cx))
                    .child(section_divider(t))
                    .child(self.render_background_section(t, cx))
                    .child(section_divider(t))
                    .child(self.render_agents_section(t, cx))
                    .child(section_divider(t))
                    .child(self.render_browser_section(t, cx))
                    .child(section_divider(t))
                    .child(self.render_connections_section(t, cx)),
            )
    }

    fn render_agents_section(&mut self, t: Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(h) = self.harness_status.clone().unwrap_or_default().into_iter().next() else {
            return div()
                .flex()
                .flex_col()
                .child(section_header("Accounts", "Sign in to a model provider so Keel can use its models.", None, t))
                .into_any_element();
        };
        if !h.installed {
            return div()
                .flex()
                .flex_col()
                .child(section_header("Accounts", "Sign in to a model provider so Keel can use its models.", None, t))
                .child(
                    div()
                        .py_3()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .t_body()
                                .text_color(c(t.danger))
                                .child(Icon::Alert.el(zpx(14.)))
                                .child("Keel's bundled Pi is missing"),
                        )
                        .child(note(t, "Run scripts/fetch-pi.sh, or point PI_EXECUTABLE at a Pi binary, then restart Keel.")),
                )
                .into_any_element();
        }
        let rows: Vec<gpui::Div> = h
            .accounts
            .clone()
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(ix, account)| self.render_account_row(ix, account, t, cx))
            .collect();
        div()
            .flex()
            .flex_col()
            .child(section_header("Accounts", "Sign in to a model provider so Keel can use its models.", None, t))
            .child(settings_list(rows, t))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_baseline()
                    .gap_3()
                    .pt_3()
                    .child(div().flex_1().min_w_0().child(note(t, "Sign-in opens in your browser and is kept for Keel only, apart from your own Pi.")))
                    .when(!h.version.is_empty(), |d| {
                        d.child(div().flex_none().t_caption().font_family(MONO).text_color(c(t.text_faint)).child(format!("Pi {}", h.version)))
                    }),
            )
            .into_any_element()
    }

    /// One provider: logo, name and what it takes (or what its sign-in
    /// needs now), then its state on the right: connected and how, the
    /// sign-in's next step, or Sign in.
    fn render_account_row(&self, _ix: usize, account: Account, t: Tokens, cx: &mut Context<Self>) -> gpui::Div {
        let reduced = self.reduced();
        let active = self.sign_in.as_ref().filter(|(id, _)| *id == account.id).map(|(_, s)| s.clone());
        let small_button = |id: String, label: &'static str, primary: bool| {
            div()
                .id(ElementId::Name(id.into()))
                .flex_none()
                .px_2p5()
                .py_1()
                .rounded(RADIUS_MD)
                .when(primary, |d| d.border_1().border_color(c(t.border)).text_color(c(t.text)).font_weight(gpui::FontWeight::MEDIUM))
                .when(!primary, |d| d.text_color(c(t.text_muted)))
                .t_small()
                .cursor_pointer()
                .hover(|s| s.bg(c(t.hover)).text_color(c(t.text)))
                .active(|s| s.bg(c(t.selected)).opacity(0.85))
                .child(label)
        };
        // What the sign-in needs from the user now, in place of the hint.
        let mut subline = (!account.hint.is_empty()).then(|| (account.hint.clone(), t.text_faint));
        let state: AnyElement = if account.signed_in && active.is_none() {
            let status = div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1p5()
                .t_small()
                .text_color(c(t.text_muted))
                .child(Icon::Check.el(zpx(13.)).text_color(c(t.success)))
                .child(account.credential());
            match &self.auth_landed {
                // Just signed in: the change is the news, so it eases in once.
                Some((id, n)) if *id == account.id => crate::motion::appear(
                    status,
                    ElementId::Name(format!("auth-landed-{id}-{n}").into()),
                    crate::motion::SLOW,
                    4.,
                    reduced,
                )
                .into_any_element(),
                _ => status.into_any_element(),
            }
        } else if let Some(sign_in) = active.filter(|s| s.state != "failed") {
            let spinner = crate::motion::spin(
                Icon::Spinner.el(zpx(12.)).text_color(c(t.text_faint)),
                ElementId::Name(format!("signin-spin-{}", account.id).into()),
                reduced,
            );
            subline = Some((
                match sign_in.state.as_str() {
                    "browser" => "Finish signing in in your browser".to_string(),
                    "device" => "Code copied. Confirm it on the page that opened".to_string(),
                    "working" if !sign_in.message.is_empty() => sign_in.message.trim_end_matches('.').to_string(),
                    _ => "Starting sign-in".to_string(),
                },
                t.text_muted,
            ));
            let reopen = (!sign_in.url.is_empty()).then(|| {
                let url = sign_in.url.clone();
                small_button(format!("signin-open-{}", account.id), "Open again", false)
                    .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| cx.open_url(&url)))
            });
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .when(sign_in.state == "device", |d| {
                    d.child(
                        div()
                            .mr_1()
                            .px_2()
                            .py_0p5()
                            .rounded(RADIUS_MD)
                            .bg(c(t.hover))
                            .font_family(MONO)
                            .t_small()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(c(t.text))
                            .child(sign_in.code.clone()),
                    )
                })
                .when(sign_in.state != "device", |d| d.child(div().mr_1().child(spinner)))
                .children(reopen)
                .child(
                    small_button(format!("signin-cancel-{}", account.id), "Cancel", false)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.cancel_sign_in(cx))),
                )
                .into_any_element()
        } else {
            if let Some((_, failed)) = self.sign_in.as_ref().filter(|(id, s)| *id == account.id && s.state == "failed") {
                let why = failed.message.trim().trim_end_matches('.');
                subline = Some((if why.is_empty() { "Sign-in failed".to_string() } else { format!("Sign-in failed: {why}") }, t.danger));
            }
            let id = account.id.clone();
            small_button(format!("signin-{}", account.id), if account.signed_in { "Sign in again" } else { "Sign in" }, true)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.start_sign_in(id.clone(), cx)))
                .into_any_element()
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .py_3()
            .child(
                div()
                    .size(zpx(28.))
                    .flex_none()
                    .rounded(RADIUS_MD)
                    .bg(c(t.input))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(c(if account.signed_in { t.text } else { t.text_muted }))
                    .child(Icon::provider(&account.id).el(zpx(15.))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().t_body().font_weight(gpui::FontWeight::MEDIUM).text_color(c(t.text)).child(account.name.clone()))
                    .when_some(subline, |d, (text, color)| {
                        d.child(div().t_small().text_color(c(color)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(text))
                    }),
            )
            .child(state)
    }

    /// Settings → Browser: which sites agents may load. The per-chat on/off
    /// switch lives in the browser pane's toolbar.
    fn render_browser_section(&mut self, t: Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let saved = self.state.ui_settings.browser_allowed_sites.clone();
        let typed = crate::browser::agent::parse_sites(&self.browser_sites.read(cx).value().to_string());
        let dirty = typed != saved;
        let status = if saved.is_empty() {
            "Agents may open any site.".to_string()
        } else {
            format!("Agents may open only: {}.", saved.join(", "))
        };
        div()
            .flex()
            .flex_col()
            .child(section_header("Browser", "What agents may do in Keel's browser pane. Your own browsing isn't limited.", None, t))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .py_3()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(div().t_body().font_weight(gpui::FontWeight::MEDIUM).text_color(c(t.text)).child("Allowed sites"))
                            .child(div().t_small().text_color(c(t.text_muted)).child(
                                "Hosts separated by commas. A host covers its subdomains; *.example.com covers only subdomains. Leave empty to allow every site.",
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .h(zpx(32.))
                                    .px_2p5()
                                    .flex()
                                    .items_center()
                                    .rounded(RADIUS_MD)
                                    .bg(c(t.input))
                                    .border_1()
                                    .border_color(c(t.border))
                                    .t_body()
                                    .child(self.browser_sites.clone()),
                            )
                            .child(
                                filled_button("save-browser-sites-btn", "Save", t.accent, t)
                                    .when(!dirty, |b| b.opacity(0.5))
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.save_browser_sites(cx))),
                            ),
                    )
                    .child(div().t_small().text_color(c(t.text_muted)).child(status)),
            )
    }

    fn render_connections_section(&mut self, t: Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.connect_busy;
        let error = self.connect_error.clone();
        let non_dotify: Vec<_> = self.connections.clone().into_iter().filter(|c| c.kind != "dotify").collect();

        let conn_rows: Vec<gpui::Div> = non_dotify
            .into_iter()
            .map(|conn| {
                let (kind, host) = (conn.kind.clone(), conn.host.clone());
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .py_3()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2p5()
                            .child(div().size(zpx(7.)).flex_none().rounded_full().bg(c(t.success)))
                            .child(
                                div()
                                    .t_body()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(c(t.text))
                                    .child(if conn.kind == "gitlab" { "GitLab".to_string() } else { conn.kind.clone() }),
                            )
                            .child(div().t_small().text_color(c(t.text_muted)).child(format!("{} · @{}", conn.host.trim_start_matches("https://"), conn.username)))
                            .when(conn.name != conn.username, |d| d.child(div().t_small().text_color(c(t.text_faint)).child(conn.name.clone()))),
                    )
                    .child(
                        text_button_owned(format!("disconnect-{}", conn.host), "Disconnect", t)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.disconnect(kind.clone(), host.clone(), cx))),
                    )
            })
            .collect();

        let gitlab_form = div()
            .flex()
            .flex_col()
            .gap_3()
            .py_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(div().t_body().font_weight(gpui::FontWeight::MEDIUM).text_color(c(t.text)).child("Connect GitLab"))
                    .child(div().t_small().text_color(c(t.text_muted)).child(
                        "gitlab.com or your own instance. Create a personal access token with the read_api scope under User settings → Access tokens.",
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(field(t, "Address", self.gitlab_host.clone().into_any_element()))
                    .child(field(t, "Token", self.gitlab_token.clone().into_any_element())),
            )
            .when_some(error, |d, err| {
                d.child(div().flex().flex_row().items_center().gap_1p5().t_small().text_color(c(t.danger)).child(Icon::Alert.el(zpx(12.))).child(err))
            })
            .child(
                div().flex().flex_row().justify_end().child(
                    filled_button("connect-gitlab-btn", if busy { "Checking…" } else { "Connect" }, t.accent, t)
                        .when(busy, |b| b.opacity(0.6))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            if !this.connect_busy {
                                this.connect_gitlab(window, cx);
                            }
                        })),
                ),
            );

        div()
            .flex()
            .flex_col()
            .child(section_header("Connections", "Services Keel can talk to for you. Tokens stay on this Mac, readable only by you.", None, t))
            .when(self.dotify_visible(), |s| s.child(self.render_dotify_settings_card(t, cx)))
            .when(!conn_rows.is_empty(), |s| s.child(settings_list(conn_rows, t)).child(div().h(gpui::px(1.)).w_full().bg(c(t.border)).my_3()))
            .child(gitlab_form)
    }
}

impl KeelWindow {
    /// Settings → Connections → Dotify: connected (who, env, Test
    /// connection, Disconnect, the reminder) or the token form.

    /// Change Settings → Notifications, save, and redraw the badges.
    fn set_notify(&mut self, change: impl FnOnce(&mut keel_proto::NotifySettings)) {
        change(&mut self.state.ui_settings.notifications);
        self.state.save_settings();
        self.refresh_dock_badge();
    }

    fn notify_toggle(&mut self, id: &'static str, change: impl FnOnce(&mut keel_proto::NotifySettings)) {
        self.note_switch_flip(id);
        self.set_notify(change);
    }

    fn preview_sound(&mut self) {
        let n = &self.state.ui_settings.notifications;
        if let Some(path) = crate::background::sound_path(&self.state.data_dir, &n.sound) {
            crate::platform_notify::play_sound(&path, n.volume.max(0.05));
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
            let result = cx.background_spawn(async move { crate::background::import_sound(&data_dir, &path) }).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(name) => {
                        this.import_error = None;
                        this.set_notify(|n| n.sound = keel_proto::SoundChoice::Custom(name));
                        this.preview_sound();
                    }
                    Err(err) => this.import_error = Some(format!("Couldn't use that sound: {err}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn choose_background(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Use picture".into()),
        });
        let data_dir = self.state.data_dir.clone();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let result = cx.background_spawn(async move { crate::background::import_background(&data_dir, &path) }).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(name) => {
                        this.import_error = None;
                        this.state.ui_settings.background = Some(name);
                        this.state.save_settings();
                        this.forget_hero_path();
                    }
                    Err(err) => this.import_error = Some(format!("Couldn't use that picture: {err}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn reset_background(&mut self, cx: &mut Context<Self>) {
        crate::background::reset_background(&self.state.data_dir);
        self.state.ui_settings.background = None;
        self.state.save_settings();
        self.import_error = None;
        self.forget_hero_path();
        cx.notify();
    }

    /// The background changed: look the picture up again on the next frame.
    fn forget_hero_path(&mut self) {
        *self.hero_path.borrow_mut() = None;
    }

    fn render_appearance_section(&mut self, t: Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let current_theme = self.state.ui_settings.theme;
        let theme_options = [
            ("System", None),
            ("Light", Some(ThemeChoice::Light)),
            ("Dark", Some(ThemeChoice::Dark)),
        ];

        let current_motion = self.state.ui_settings.reduced_motion;
        let motion_options = [
            ("System", None),
            ("On", Some(true)),
            ("Off", Some(false)),
        ];

        let rows = vec![
            setting_row(
                t,
                "Theme",
                Some("Interface color scheme"),
                segmented(
                    theme_options.into_iter().map(|(label, choice)| {
                        chip(format!("theme-{label}"), label.to_string(), current_theme == choice, t)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.state.set_theme(choice);
                                cx.notify();
                            }))
                            .into_any_element()
                    }),
                    t,
                )
                .into_any_element(),
            ),
            setting_row(
                t,
                "Reduced motion",
                Some("Minimize interface animations"),
                segmented(
                    motion_options.into_iter().map(|(label, val)| {
                        chip(format!("motion-{label}"), label.to_string(), current_motion == val, t)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.state.set_reduced_motion(val);
                                this.transcript.reduced_motion = this.state.effective_reduced_motion(this.os_reduced_motion);
                                cx.notify();
                            }))
                            .into_any_element()
                    }),
                    t,
                )
                .into_any_element(),
            ),
        ];

        div()
            .flex()
            .flex_col()
            .child(section_header("Appearance", "Theme and motion preferences.", None, t))
            .child(settings_list(rows, t))
    }

    fn render_notifications_section(&mut self, t: Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        use keel_proto::{BadgeStyle, SoundChoice};
        let n = self.state.ui_settings.notifications.clone();
        let switch = |this: &Self, id: &'static str, on: bool| toggle(id.to_string(), on, this.recent_flip(id), t);

        let badges = [(BadgeStyle::Count, "Count"), (BadgeStyle::Dot, "Dot"), (BadgeStyle::None, "None")];
        // Banners need macOS's permission, which can change in System Settings
        // at any time: look again (throttled) whenever this page draws.
        use crate::platform_notify::{BannerPermission, banner_permission};
        crate::platform_notify::refresh_permission();
        let permission = banner_permission();
        let banner_status = (n.banner && permission != BannerPermission::Allowed).then(|| match permission {
            BannerPermission::Denied => setting_row(
                t,
                "Notifications are off for Keel in System Settings",
                Some("No banners until you allow them there. Sound and badge still work."),
                text_button_owned("notify-open-settings".into(), "Open Settings", t)
                    .on_click(|_, _, _| crate::platform_notify::open_notification_settings())
                    .into_any_element(),
            ),
            BannerPermission::NotAsked => setting_row(
                t,
                "Keel needs permission to show banners",
                Some("macOS asks once. If you dismiss it, turn Keel on in System Settings → Notifications."),
                text_button_owned("notify-ask".into(), "Allow", t)
                    .on_click(|_, _, _| crate::platform_notify::request_permission())
                    .into_any_element(),
            ),
            _ => setting_row(
                t,
                "Banners need the Keel app",
                Some("This build runs outside Keel.app (scripts/bundle-mac.sh), so macOS won't show its banners."),
                div().into_any_element(),
            ),
        });

        // Sound: a picker that opens a list in place, and a preview.
        if self.show_sound_menu && self.system_sounds.is_empty() {
            self.system_sounds = crate::background::system_sounds();
        }
        let sound_label = crate::background::sound_label(&n.sound);
        let volumes = [(0.25_f32, "25%"), (0.5, "50%"), (0.75, "75%"), (1.0, "100%")];
        let nearest = volumes
            .iter()
            .min_by(|a, b| (a.0 - n.volume).abs().total_cmp(&(b.0 - n.volume).abs()))
            .map(|v| v.0)
            .unwrap_or(0.75);

        let sound_control = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1p5()
            .child(
                div()
                    .id("sound-picker")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1p5()
                    .px_2p5()
                    .h(zpx(26.))
                    .rounded(RADIUS_MD)
                    .bg(c(t.input))
                    .border_1()
                    .border_color(c(t.border))
                    .t_small()
                    .cursor_pointer()
                    .hover(|s| s.bg(c(t.hover)))
                    .active(|s| s.opacity(0.85))
                    .child(sound_label)
                    .child(div().text_color(c(t.text_faint)).child(if self.show_sound_menu { "▴" } else { "▾" }))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.show_sound_menu = !this.show_sound_menu;
                        cx.notify();
                    })),
            )
            .when(!matches!(n.sound, SoundChoice::None), |d| {
                d.child(text_button("sound-preview-btn", "Preview", t).on_click(cx.listener(|this, _: &ClickEvent, _, _| this.preview_sound())))
            });

        let sound_menu: Option<gpui::Stateful<gpui::Div>> = if self.show_sound_menu {
            let mut options: Vec<(SoundChoice, String)> = vec![
                (SoundChoice::None, "None".into()),
                (SoundChoice::Default, crate::background::sound_label(&SoundChoice::Default)),
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
            let list = div()
                .id("sound-list")
                .max_h(zpx(220.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .p_1()
                .my_2()
                .rounded(RADIUS_MD)
                .bg(c(t.input))
                .border_1()
                .border_color(c(t.border))
                .children(options.into_iter().enumerate().map(|(ix, (choice, label))| {
                    let selected = choice == n.sound;
                    div()
                        .id(ElementId::Name(format!("sound-opt-{ix}").into()))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .rounded(RADIUS_MD)
                        .t_small()
                        .cursor_pointer()
                        .when(selected, |d| d.bg(c(t.selected)))
                        .hover(|s| s.bg(c(t.hover)))
                        .child(div().w(zpx(12.)).text_color(c(t.accent)).child(if selected { "✓" } else { "" }))
                        .child(label)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            let choice = choice.clone();
                            this.set_notify(|n| n.sound = choice);
                            this.preview_sound();
                            cx.notify();
                        }))
                }))
                .child(
                    div()
                        .id("sound-opt-custom")
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .rounded(RADIUS_MD)
                        .t_small()
                        .text_color(c(t.accent))
                        .cursor_pointer()
                        .hover(|s| s.bg(c(t.hover)))
                        .child(div().w(zpx(12.)))
                        .child("Custom…")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.choose_sound(cx))),
                );
            Some(list)
        } else {
            None
        };

        let sound_row = div()
            .flex()
            .flex_col()
            .child(setting_row(t, "Sound", None, sound_control.into_any_element()))
            .children(sound_menu);

        let mut rows: Vec<gpui::Div> = vec![
            setting_row(
                t,
                "Agent finished",
                None,
                switch(self, "notify-finished", n.on_finished)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.notify_toggle("notify-finished", |n| n.on_finished = !n.on_finished);
                        cx.notify();
                    }))
                    .into_any_element(),
            ),
            setting_row(
                t,
                "Needs your input",
                Some("A question or a permission request"),
                switch(self, "notify-input", n.on_needs_input)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.notify_toggle("notify-input", |n| n.on_needs_input = !n.on_needs_input);
                        cx.notify();
                    }))
                    .into_any_element(),
            ),
            setting_row(
                t,
                "Turn error",
                Some("The turn failed or was cut off"),
                switch(self, "notify-error", n.on_error)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.notify_toggle("notify-error", |n| n.on_error = !n.on_error);
                        cx.notify();
                    }))
                    .into_any_element(),
            ),
            setting_row(
                t,
                "Badge",
                Some("On the Dock icon and the sidebar's chats"),
                segmented(
                    badges.iter().map(|(style, label)| {
                        let style = *style;
                        chip(format!("badge-{label}"), (*label).to_string(), n.badge == style, t)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.set_notify(|n| n.badge = style);
                                cx.notify();
                            }))
                            .into_any_element()
                    }),
                    t,
                )
                .into_any_element(),
            ),
            setting_row(
                t,
                "Bounce the Dock icon",
                Some("When an agent needs your input"),
                switch(self, "notify-bounce", n.bounce)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.notify_toggle("notify-bounce", |n| n.bounce = !n.bounce);
                        cx.notify();
                    }))
                    .into_any_element(),
            ),
            setting_row(
                t,
                "Banner",
                Some("A macOS notification. Click it to open the chat."),
                switch(self, "notify-banner", n.banner)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.notify_toggle("notify-banner", |n| n.banner = !n.banner);
                        if this.state.ui_settings.notifications.banner && banner_permission() == BannerPermission::NotAsked {
                            crate::platform_notify::request_permission();
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
            ),
        ];

        if let Some(status_row) = banner_status {
            rows.push(status_row);
        }

        rows.push(sound_row);
        rows.push(setting_row(
            t,
            "Volume",
            None,
            segmented(
                volumes.iter().map(|(volume, label)| {
                    let volume = *volume;
                    chip(format!("volume-{label}"), (*label).to_string(), nearest == volume, t)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.set_notify(|n| n.volume = volume);
                            this.preview_sound();
                            cx.notify();
                        }))
                        .into_any_element()
                }),
                t,
            )
            .into_any_element(),
        ));
        rows.push(setting_row(
            t,
            "Only when Keel is in the background",
            None,
            switch(self, "notify-sound-bg", n.sound_in_background_only)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.notify_toggle("notify-sound-bg", |n| n.sound_in_background_only = !n.sound_in_background_only);
                    cx.notify();
                }))
                .into_any_element(),
        ));

        div()
            .flex()
            .flex_col()
            .child(section_header("Notifications", "When an agent finishes, needs you, or fails in a chat you aren't looking at.", None, t))
            .child(settings_list(rows, t))
    }

    fn render_background_section(&mut self, t: Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let custom = crate::background::background_path(&self.state.data_dir, self.state.ui_settings.background.as_deref());
        let dim = self.state.ui_settings.background_dim.unwrap_or(0.);
        let steps = [(0.0_f32, "Off"), (0.2, "20%"), (0.4, "40%"), (0.6, "60%"), (0.8, "80%")];
        let nearest = steps
            .iter()
            .min_by(|a, b| (a.0 - dim).abs().total_cmp(&(b.0 - dim).abs()))
            .map(|s| s.0)
            .unwrap_or(0.);
        let thumb = div()
            .w(zpx(144.))
            .h(zpx(81.))
            .flex_none()
            .relative()
            .overflow_hidden()
            .rounded(RADIUS_MD)
            .border_1()
            .border_color(c(t.border))
            .child(
                custom
                    .clone()
                    .map_or_else(|| gpui::img(crate::icons::HERO_IMAGE), gpui::img)
                    .size_full()
                    .object_fit(gpui::ObjectFit::Cover),
            )
            .when(dim > 0., |d| d.child(div().absolute().inset_0().bg(gpui::hsla(0., 0., 0., dim))));
        let error = self.import_error.clone();

        let picture_row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_4()
            .py_3()
            .child(thumb)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(
                        div()
                            .t_body()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(c(t.text))
                            .child(if custom.is_some() { "Your picture" } else { "Built-in landscape" }),
                    )
                    .child(
                        div()
                            .t_small()
                            .text_color(c(t.text_muted))
                            .child("PNG, JPEG, HEIC or WebP. Keel keeps its own copy, scaled to 2560 px."),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .pt_1()
                            .child(
                                filled_button_owned("bg-choose-btn".into(), "Choose image…", t)
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.choose_background(cx))),
                            )
                            .when(custom.is_some(), |d| {
                                d.child(
                                    text_button("bg-reset-btn", "Reset to default", t)
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.reset_background(cx))),
                                )
                            }),
                    ),
            );

        let dim_row = setting_row(
            t,
            "Dim",
            Some("Darken the picture behind the text"),
            segmented(
                steps.iter().map(|(step, label)| {
                    let step = *step;
                    chip(format!("dim-{label}"), (*label).to_string(), nearest == step, t)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.state.ui_settings.background_dim = (step > 0.).then_some(step);
                            this.state.save_settings();
                            cx.notify();
                        }))
                        .into_any_element()
                }),
                t,
            )
            .into_any_element(),
        );

        let glow_row = setting_row(
            t,
            "Chat glow",
            Some("A faint glow of the picture under the chat input"),
            segmented(
                [("Off", true), ("On", false)].into_iter().map(|(label, off)| {
                    chip(format!("chat-glow-{label}"), label.to_string(), self.state.ui_settings.chat_glow_off == off, t)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.state.ui_settings.chat_glow_off = off;
                            this.state.save_settings();
                            cx.notify();
                        }))
                        .into_any_element()
                }),
                t,
            )
            .into_any_element(),
        );

        let rows = vec![picture_row, dim_row, glow_row];

        div()
            .flex()
            .flex_col()
            .child(section_header("Background", "The picture behind the new-chat screen.", None, t))
            .child(settings_list(rows, t))
            .when_some(error, |d, err| {
                d.child(
                    div()
                        .pt_2()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1p5()
                        .t_small()
                        .text_color(c(t.danger))
                        .child(Icon::Alert.el(zpx(12.)))
                        .child(err),
                )
            })
    }
}

/// Section title with subtitle.
fn section_header(title: &'static str, blurb: &'static str, action: Option<AnyElement>, t: Tokens) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_baseline()
        .justify_between()
        .pb_3()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .text_size(zpx(15.))
                        .line_height(zpx(20.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(c(t.text))
                        .child(title),
                )
                .child(div().t_small().text_color(c(t.text_muted)).child(blurb)),
        )
        .children(action)
}

/// 1px rule with 32px vertical margin separating major sections.
fn section_divider(t: Tokens) -> gpui::Div {
    div().h(gpui::px(1.)).w_full().bg(c(t.border)).my_8()
}

/// Settings list container that places a 1px hairline divider between rows.
fn settings_list(rows: Vec<gpui::Div>, t: Tokens) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .children(rows.into_iter().enumerate().map(|(ix, row)| {
            row.when(ix > 0, |d| d.border_t_1().border_color(c(t.border)))
        }))
}

/// A setting: its label (and an explanation) on the left, the control on the right.
fn setting_row(t: Tokens, label: &'static str, caption: Option<&'static str>, control: AnyElement) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .py_3()
        .gap_4()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .t_body()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(c(t.text))
                        .child(label),
                )
                .children(caption.map(|text| div().t_small().text_color(c(t.text_muted)).child(text))),
        )
        .child(control)
}

/// Options side by side in a unified pill track; one is picked.
fn segmented(options: impl Iterator<Item = AnyElement>, t: Tokens) -> gpui::Div {
    div()
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .p(zpx(2.))
        .gap(zpx(2.))
        .rounded(RADIUS_MD)
        .bg(c(t.input))
        .border_1()
        .border_color(c(t.border))
        .children(options)
}

/// One option of a [`segmented`] control.
fn chip(id: String, label: String, selected: bool, t: Tokens) -> gpui::Stateful<gpui::Div> {
    div()
        .id(ElementId::Name(id.into()))
        .px_2p5()
        .h(zpx(22.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(RADIUS_SM)
        .t_small()
        .cursor_pointer()
        .map(|d| {
            if selected {
                d.bg(c(t.accent))
                    .text_color(c(t.text_on_accent))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .active(|s| s.opacity(0.9))
            } else {
                d.text_color(c(t.text_muted))
                    .hover(|s| s.text_color(c(t.text)).bg(c(t.hover)))
                    .active(|s| s.opacity(0.8))
            }
        })
        .child(label)
}

fn note(t: Tokens, text: &'static str) -> gpui::Div {
    div().t_small().text_color(c(t.text_muted)).child(text)
}

/// A labelled text field on the surface.
fn field(t: Tokens, label: &'static str, input: AnyElement) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .child(div().w(zpx(64.)).flex_none().t_small().text_color(c(t.text_muted)).child(label))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .h(zpx(32.))
                .px_2p5()
                .flex()
                .items_center()
                .rounded(RADIUS_MD)
                .bg(c(t.input))
                .border_1()
                .border_color(c(t.border))
                .t_body()
                .child(input),
        )
}

/// A switch: a pill with a knob. Right after a flip (`flip` names it) the
/// knob slides across, stretching a little mid-travel, while the track and
/// knob colors blend to their new state.
fn toggle(id: String, on: bool, flip: Option<u64>, t: Tokens) -> gpui::Stateful<gpui::Div> {
    use gpui::AnimationExt as _;
    const TRAVEL: f32 = 12.;
    const KNOB: f32 = 14.;
    let paint = move |el: gpui::Div, at: f32| {
        // `at`: 0 = off, 1 = on. The knob is widest halfway.
        let stretch = 4. * (1. - (2. * at - 1.).abs());
        let width = KNOB + stretch;
        let left = (TRAVEL + KNOB - width) * at;
        el.bg(c(mix(t.hover, t.accent, at)))
            .child(div().h(zpx(KNOB)).rounded_full().w(zpx(width)).ml(zpx(left)).bg(c(mix(t.text_faint, t.text_on_accent, at))))
    };
    let track = div().w(zpx(30.)).h(zpx(18.)).p(zpx(2.)).flex_none().rounded_full().flex().items_center();
    let body: gpui::AnyElement = match flip {
        Some(n) => track
            .with_animation(
                ElementId::Name(format!("{id}-flip-{n}").into()),
                gpui::Animation::new(crate::motion::BASE).with_easing(crate::motion::ease_out),
                move |el, delta| paint(el, if on { delta } else { 1. - delta }),
            )
            .into_any_element(),
        None => paint(track, if on { 1. } else { 0. }).into_any_element(),
    };
    div().id(ElementId::Name(id.into())).flex_none().cursor_pointer().child(body)
}

/// Blend two `0xRRGGBBAA` colors, `at` 0 → `from`, 1 → `to`.
fn mix(from: u32, to: u32, at: f32) -> u32 {
    let at = at.clamp(0., 1.);
    (0..4).fold(0, |acc, i| {
        let shift = 24 - 8 * i;
        let (a, b) = (((from >> shift) & 0xFF) as f32, ((to >> shift) & 0xFF) as f32);
        acc | (((a + (b - a) * at).round() as u32) << shift)
    })
}

/// `filled_button` (accent) with a runtime id.
fn filled_button_owned(id: String, label: &'static str, t: Tokens) -> gpui::Stateful<gpui::Div> {
    div()
        .id(ElementId::Name(id.into()))
        .flex_none()
        .px_2p5()
        .py_1()
        .rounded(RADIUS_MD)
        .bg(c(t.accent))
        .t_small()
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(c(t.text_on_accent))
        .cursor_pointer()
        .hover(|s| s.opacity(0.9))
        .child(label)
}

/// `text_button` with a runtime id.
fn text_button_owned(id: String, label: &'static str, t: Tokens) -> gpui::Stateful<gpui::Div> {
    div()
        .id(ElementId::Name(id.into()))
        .px_2p5()
        .py_1()
        .flex_none()
        .rounded(RADIUS_MD)
        .t_small()
        .text_color(c(t.accent))
        .cursor_pointer()
        .hover(|s| s.bg(c(t.hover)))
        .child(label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_blends_each_channel() {
        assert_eq!(mix(0x000000FF, 0xFF8040FF, 0.), 0x000000FF);
        assert_eq!(mix(0x000000FF, 0xFF8040FF, 1.), 0xFF8040FF);
        assert_eq!(mix(0x00000000, 0x80808080, 0.5), 0x40404040);
    }

    #[test]
    fn harness_status_rows_parse() {
        let v = serde_json::json!({
            "id": "keel", "display_name": "Keel", "installed": true, "version": "1.3.0", "path": "/p/pi",
            "accounts": [
                {"id": "anthropic", "name": "Claude", "hint": "Pro or Max plan", "signed_in": true, "detail": "oauth"},
                {"id": "xai", "name": "xAI", "hint": "", "signed_in": false, "detail": "not signed in"}
            ],
        });
        let h = HarnessStatus::parse(&v);
        let accounts = h.accounts.as_ref().expect("checked");
        assert_eq!((h.id.as_str(), h.installed, accounts.len()), ("keel", true, 2));
        assert_eq!((accounts[0].id.as_str(), accounts[0].hint.as_str()), ("anthropic", "Pro or Max plan"));
        assert_eq!(accounts[0].credential(), "Subscription");
        assert!(accounts[0].signed_in && !accounts[1].signed_in);
        // Not asked yet: no accounts, not an empty list.
        assert_eq!(HarnessStatus::parse(&serde_json::json!({"id": "keel", "accounts": null})).accounts, None);
        assert_eq!(
            SignInUi::parse(&serde_json::json!({"state": "device", "code": "0A50-65F9", "url": "https://github.com/login/device"})),
            SignInUi { state: "device".into(), code: "0A50-65F9".into(), url: "https://github.com/login/device".into(), message: String::new() }
        );
        assert_eq!(HarnessStatus::parse(&serde_json::json!({"id": "keel"})).path, None);
    }
}

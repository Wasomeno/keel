//! Settings (⌘,): the model providers Keel's agent is signed in to and connected services (GitLab on gitlab.com or a
//! self-hosted domain). Sign-in happens in Pi's own `/login`, run against
//! Keel's Pi folder; Keel only lists what is stored there.

use crate::style::zpx;
use gpui::{
    AnimationExt, AnyElement, ClickEvent, Context, ElementId, IntoElement, UniformListScrollHandle,
    div, prelude::*, uniform_list,
};
use keel_proto::ThemeChoice;
use keel_theme::Tokens;

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
        Self {
            state: s("state"),
            url: s("url"),
            code: s("code"),
            message: s("message"),
        }
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
        Self {
            kind: s("kind"),
            host: s("host"),
            username: s("username"),
            name: s("name"),
        }
    }
}

/// One skill as `list_skills` reports it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct SkillRow {
    pub name: String,
    pub description: String,
    pub insert: String,
    pub scope: String,
    pub path: String,
    pub enabled: bool,
    pub manual_only: bool,
    pub removable: bool,
    /// Repo the skill was installed from, or a shared name prefix. Empty if none.
    pub collection: String,
    pub author: String,
    pub source_url: String,
}

impl SkillRow {
    fn parse(v: &serde_json::Value) -> Self {
        let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
        Self {
            name: s("name"),
            description: s("description"),
            insert: s("insert"),
            scope: s("scope"),
            path: s("path"),
            enabled: v["enabled"].as_bool().unwrap_or(false),
            manual_only: v["manual_only"].as_bool().unwrap_or(false),
            removable: v["removable"].as_bool().unwrap_or(false),
            collection: s("collection"),
            author: s("author"),
            source_url: s("source_url"),
        }
    }
}

/// Status filter for skills list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SkillStatusFilter {
    All,
    Enabled,
    Disabled,
}

/// Where an added skill is about to be copied, or a delete/replace still to confirm.
#[derive(Clone)]
pub(super) enum SkillConfirm {
    Replace {
        source: String,
        dest: String,
        name: String,
    },
    Remove {
        path: String,
        name: String,
    },
}

/// Settings → Skills. The list replaces the settings column while `open`.
pub(super) struct SkillsPage {
    pub open: bool,
    pub epoch: u64,
    /// When a page transition between Settings Home and Skills is running: `(epoch, leaving)`.
    pub transition: Option<(u64, bool)>,
    pub load_gen: u64,
    pub rows: Option<Result<Vec<SkillRow>, String>>,
    /// Name and scope of a skill just added, so its row plays `appear` once.
    pub added: Option<(String, String)>,
    pub notice: Option<String>,
    pub confirm: Option<SkillConfirm>,
    pub row_errors: std::collections::HashMap<String, String>,
    /// Own scroll, so opening the drill-in does not inherit the Settings offset.
    pub scroll: gpui::ScrollHandle,
    /// Virtualized list scroll handle for the compact skills list.
    pub list_scroll: UniformListScrollHandle,
    /// Filter by author / creator. None = All.
    pub author_filter: Option<String>,
    /// Filter by active/inactive status.
    pub status_filter: SkillStatusFilter,
    /// Generation bumped on creator or status filter change to animate the list.
    pub filter_epoch: u64,
    /// Path of the skill currently open in instructions view.
    pub expanded_skill: Option<String>,
    /// Cached SKILL.md content for expanded skills.
    pub expanded_content: std::collections::HashMap<String, String>,
    /// Skill currently shown in the instruction modal dialog.
    pub modal_skill: Option<SkillRow>,
    /// Skill leaving the modal dialog (exit animation).
    pub modal_leaving: Option<(u64, SkillRow)>,
    pub modal_gen: u64,
    pub modal_scroll: gpui::ScrollHandle,
}

impl Default for SkillsPage {
    fn default() -> Self {
        Self {
            open: false,
            epoch: 0,
            transition: None,
            load_gen: 0,
            rows: None,
            added: None,
            notice: None,
            confirm: None,
            row_errors: std::collections::HashMap::new(),
            scroll: gpui::ScrollHandle::default(),
            list_scroll: UniformListScrollHandle::new(),
            author_filter: None,
            status_filter: SkillStatusFilter::All,
            filter_epoch: 0,
            expanded_skill: None,
            expanded_content: std::collections::HashMap::new(),
            modal_skill: None,
            modal_leaving: None,
            modal_gen: 0,
            modal_scroll: gpui::ScrollHandle::default(),
        }
    }
}

/// Computes (draw_left, draw_w) with asymmetric leading/trailing edge morphology.
fn compute_pill_geometry(start_left: f32, end_left: f32, item_w: f32, delta: f32) -> (f32, f32) {
    let dist = end_left - start_left;
    if dist.abs() < 0.1 {
        return (end_left, item_w);
    }
    let lead_t = crate::motion::ease_out_strong(delta);
    let lag_curve = crate::motion::glide_curve(delta);
    let trail_t = 0.55 * lead_t + 0.45 * lag_curve;

    if dist > 0. {
        // Moving right: right edge leads, left edge trails
        let curr_right = (start_left + item_w) + dist * lead_t;
        let curr_left = start_left + dist * trail_t;
        let curr_w = (curr_right - curr_left).max(item_w);
        (curr_left, curr_w)
    } else {
        // Moving left: left edge leads, right edge trails
        let curr_left = start_left + dist * lead_t;
        let curr_right = (start_left + item_w) + dist * trail_t;
        let curr_w = (curr_right - curr_left).max(item_w);
        (curr_left, curr_w)
    }
}

/// Inner text renderer for the clipped active mask layer.
fn render_active_text_row(
    labels: &[&'static str],
    item_w: f32,
    text_color: u32,
    draw_left: f32,
) -> gpui::Div {
    const PAD: f32 = 2.0;
    const GAP: f32 = 2.0;
    const H: f32 = 24.0;
    div()
        .absolute()
        .top(zpx(0.))
        .left(zpx(PAD - draw_left))
        .h(zpx(H))
        .flex()
        .flex_row()
        .items_center()
        .gap(zpx(GAP))
        .children(labels.iter().map(|label| {
            div()
                .w(zpx(item_w))
                .h(zpx(H))
                .flex()
                .items_center()
                .justify_center()
                .t_small()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(c(text_color))
                .child(*label)
        }))
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
        self.load_skills(cx);
        cx.notify();
    }

    pub(super) fn close_settings(&mut self, cx: &mut Context<Self>) {
        self.close_pages();
        self.finish_settings_exit(cx);
        self.skills.open = false;
        self.skills.transition = None;
        self.skills.confirm = None;
        self.skills.expanded_skill = None;
        self.skills.author_filter = None;
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
            let res = client
                .call(
                    "harness_status",
                    serde_json::json!({ "check_sign_in": true }),
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                match res {
                    Ok(v) => {
                        let rows = v["harnesses"]
                            .as_array()
                            .map(|r| r.iter().map(HarnessStatus::parse).collect());
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
        self.sign_in = Some((
            id.clone(),
            SignInUi {
                state: "starting".into(),
                ..Default::default()
            },
        ));
        cx.notify();
        cx.spawn(async move |this, cx| {
            if let Err(err) = client
                .call("sign_in", serde_json::json!({ "provider": id }))
                .await
            {
                let _ = this.update(cx, |this, cx| {
                    this.sign_in = Some((
                        id.clone(),
                        SignInUi {
                            state: "failed".into(),
                            message: err,
                            ..Default::default()
                        },
                    ));
                    cx.notify();
                });
                return;
            }
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(500))
                    .await;
                let Ok(v) = client
                    .call("sign_in_status", serde_json::json!({ "provider": id }))
                    .await
                else {
                    continue;
                };
                let next = SignInUi::parse(&v);
                let over = this
                    .update(cx, |this, cx| {
                        // Cancelled, or another provider's sign-in took over.
                        let Some(last) = this
                            .sign_in
                            .as_ref()
                            .filter(|(p, _)| *p == id)
                            .map(|(_, s)| s.clone())
                        else {
                            return true;
                        };
                        // Hand the user over once per step: open the page,
                        // and put GitHub's code on the clipboard first.
                        let fresh = last.state != next.state || last.url != next.url;
                        if fresh && next.state == "device" {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                next.code.clone(),
                            ));
                        }
                        if fresh
                            && matches!(next.state.as_str(), "browser" | "device")
                            && !next.url.is_empty()
                        {
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
                let _ = client
                    .call("sign_in_cancel", serde_json::json!({ "provider": id }))
                    .await;
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
                    this.connections = v["connections"]
                        .as_array()
                        .map(|r| r.iter().map(ConnectionRow::parse).collect())
                        .unwrap_or_default();
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Check the token with GitLab and keep it on success.
    fn connect_gitlab(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let host = self.gitlab_host.read(cx).value().trim().to_string();
        let host = if host.is_empty() {
            "https://gitlab.com".to_string()
        } else {
            host
        };
        let token = self.gitlab_token.read(cx).value().trim().to_string();
        let Some(client) = self.client.clone() else {
            return;
        };
        self.connect_busy = true;
        self.connect_error = None;
        // The token lives in the engine from here; clear the field now.
        self.gitlab_token
            .update(cx, |input, cx| input.set_value("", window, cx));
        cx.spawn(async move |this, cx| {
            let res = client
                .call(
                    "connect_gitlab",
                    serde_json::json!({ "host": host, "token": token }),
                )
                .await;
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
            let res = client
                .call(
                    "disconnect",
                    serde_json::json!({ "kind": kind, "host": host }),
                )
                .await;
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
        self.switch_flips
            .retain(|_, (_, at)| at.elapsed() < std::time::Duration::from_secs(1));
        self.switch_flips
            .insert(id.to_string(), (self.switch_gen, std::time::Instant::now()));
    }

    /// The flip number when switch `id` changed just now (and motion is on).
    fn recent_flip(&self, id: &str) -> Option<u64> {
        self.switch_flips
            .get(id)
            .filter(|(_, at)| !self.reduced() && at.elapsed() < std::time::Duration::from_millis(150))
            .map(|(n, _)| *n)
    }

    /// Remember that segmented control `id` just changed selection from `from_ix` to `to_ix`.
    fn note_segmented_flip(&mut self, id: &str, from_ix: usize, to_ix: usize) {
        self.segmented_gen += 1;
        self.segmented_flips
            .retain(|_, (_, _, _, at)| at.elapsed() < std::time::Duration::from_secs(1));
        self.segmented_flips
            .insert(id.to_string(), (from_ix, to_ix, self.segmented_gen, std::time::Instant::now()));
    }

    /// The recent flip info `(from_ix, to_ix, generation)` when segmented control `id` changed just now (and motion is on).
    fn recent_segmented_flip(&self, id: &str) -> Option<(usize, usize, u64)> {
        self.segmented_flips
            .get(id)
            .filter(|(_, _, _, at)| !self.reduced() && at.elapsed() < std::time::Duration::from_millis(150))
            .map(|(from, to, generation, _)| (*from, *to, *generation))
    }

    /// Renders a sleek segmented control with Emil Kowalski motion physics:
    /// - Optical clip-mask architecture: duplicate active text layer clipped inside
    ///   the sliding indicator, creating a mathematically seamless continuous color transition.
    /// - Physical elastic morphology: asymmetric leading and trailing edge physics
    ///   (`ease_out_strong` on leading edge, inertial `glide_curve` lag on trailing edge).
    /// - Snappy 150ms duration with zero initial hesitation.
    /// - Instant cuts with zero animation when reduced motion is enabled.
    fn render_segmented_control<T: Copy + PartialEq + 'static, F>(
        &self,
        group_id: &'static str,
        options: &[(&'static str, T)],
        current: T,
        item_w: f32,
        t: Tokens,
        cx: &mut Context<Self>,
        on_select: F,
    ) -> gpui::Div
    where
        F: Fn(&mut Self, T, &mut Context<Self>) + Copy + 'static,
    {
        use gpui::AnimationExt as _;

        let current_ix = options.iter().position(|(_, val)| *val == current).unwrap_or(0);
        let flip = self.recent_segmented_flip(group_id);
        let count = options.len();
        const PAD: f32 = 2.0;
        const GAP: f32 = 2.0;
        const H: f32 = 24.0;
        const PILL_RADIUS: f32 = 5.0;
        const TRACK_RADIUS: f32 = 8.0;

        // Account for 1px border on each side (total 2px width and 2px height)
        let inner_w = PAD * 2.0 + (item_w * count as f32) + (GAP * (count.saturating_sub(1)) as f32);
        let total_w = inner_w + 2.0;
        let total_h = H + PAD * 2.0 + 2.0;
        let left_of = |ix: usize| PAD + ix as f32 * (item_w + GAP);

        let labels_list: Vec<&'static str> = options.iter().map(|(l, _)| *l).collect();
        let text_on_accent = t.text_on_accent;

        let animated_pill: gpui::AnyElement = match flip {
            Some((from_ix, to_ix, generation)) => {
                let start_left = left_of(from_ix);
                let end_left = left_of(to_ix);
                let labels_copy = labels_list.clone();

                let make_pill = move |draw_left: f32, draw_w: f32| {
                    div()
                        .absolute()
                        .top(zpx(PAD))
                        .left(zpx(draw_left))
                        .w(zpx(draw_w))
                        .h(zpx(H))
                        .rounded(zpx(PILL_RADIUS))
                        .bg(c(t.accent))
                        .overflow_hidden()
                        .shadow(vec![gpui::BoxShadow {
                            color: c(t.shadow).opacity(0.2),
                            offset: gpui::point(zpx(0.), zpx(1.)),
                            blur_radius: zpx(1.5),
                            spread_radius: zpx(0.),
                            inset: false,
                        }])
                        .child(render_active_text_row(
                            &labels_copy,
                            item_w,
                            text_on_accent,
                            draw_left,
                        ))
                };

                let initial_pill = make_pill(start_left, item_w);
                initial_pill
                    .with_animation(
                        ElementId::Name(format!("{group_id}-seg-flip-{generation}").into()),
                        gpui::Animation::new(std::time::Duration::from_millis(150)),
                        move |_el, delta| {
                            let (draw_left, draw_w) =
                                compute_pill_geometry(start_left, end_left, item_w, delta);
                            make_pill(draw_left, draw_w)
                        },
                    )
                    .into_any_element()
            }
            None => {
                let draw_left = left_of(current_ix);
                div()
                    .absolute()
                    .top(zpx(PAD))
                    .left(zpx(draw_left))
                    .w(zpx(item_w))
                    .h(zpx(H))
                    .rounded(zpx(PILL_RADIUS))
                    .bg(c(t.accent))
                    .overflow_hidden()
                    .shadow(vec![gpui::BoxShadow {
                        color: c(t.shadow).opacity(0.2),
                        offset: gpui::point(zpx(0.), zpx(1.)),
                        blur_radius: zpx(1.5),
                        spread_radius: zpx(0.),
                        inset: false,
                    }])
                    .child(render_active_text_row(
                        &labels_list,
                        item_w,
                        text_on_accent,
                        draw_left,
                    ))
                    .into_any_element()
            }
        };

        // Layer 1: Inactive base text row
        let inactive_labels = div()
            .absolute()
            .top(zpx(PAD))
            .left(zpx(PAD))
            .h(zpx(H))
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(GAP))
            .children(options.iter().map(|(label, _)| {
                div()
                    .w(zpx(item_w))
                    .h(zpx(H))
                    .flex()
                    .items_center()
                    .justify_center()
                    .t_small()
                    .font_weight(gpui::FontWeight::NORMAL)
                    .text_color(c(t.text_muted))
                    .child(*label)
            }));

        // Layer 3: Interactive hit targets with subtle hover feedback
        let hit_targets = div()
            .absolute()
            .top(zpx(PAD))
            .left(zpx(PAD))
            .h(zpx(H))
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(GAP))
            .children(options.iter().enumerate().map(|(ix, (label, val))| {
                let is_selected = ix == current_ix;
                let val = *val;
                div()
                    .id(ElementId::Name(format!("{group_id}-{label}").into()))
                    .w(zpx(item_w))
                    .h(zpx(H))
                    .rounded(zpx(PILL_RADIUS))
                    .cursor_pointer()
                    .when(!is_selected, |d| {
                        d.hover(|s| s.bg(c(t.hover).opacity(0.35)))
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if ix != current_ix {
                            this.note_segmented_flip(group_id, current_ix, ix);
                        }
                        on_select(this, val, cx);
                        cx.notify();
                    }))
            }));

        // Composite track: explicitly sized without parent padding so absolute layers
        // are positioned with exact pixel-perfect symmetry inside the 1px border.
        div()
            .relative()
            .flex_none()
            .w(zpx(total_w))
            .h(zpx(total_h))
            .rounded(zpx(TRACK_RADIUS))
            .bg(c(t.input))
            .border_1()
            .border_color(c(t.border))
            .child(inactive_labels)
            .child(animated_pill)
            .child(hit_targets)
    }

    pub(super) fn render_settings(
        &mut self,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        if self.skills.transition.is_some() {
            self.render_skills_drill(t, cx).into_any_element()
        } else if self.skills.open {
            self.render_skills_page(t, cx).into_any_element()
        } else {
            self.render_settings_home(t, cx).into_any_element()
        }
    }

    fn render_settings_home(&mut self, t: Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let current_scroll_y = self.dotify.settings_scroll.offset().y;
        if self.show_sound_menu {
            if let Some(prev) = self.sound_menu_scroll_y {
                if (current_scroll_y - prev).abs() > gpui::px(1.0) {
                    self.show_sound_menu = false;
                    self.sound_menu_scroll_y = None;
                }
            } else {
                self.sound_menu_scroll_y = Some(current_scroll_y);
            }
        } else {
            self.sound_menu_scroll_y = None;
        }

        // A column, not a row: a row stretches the page to the viewport's height,
        // so it never overflows and there's nothing to scroll.
        div()
            .id("settings-scroll")
            .track_scroll(&self.dotify.settings_scroll)
            .when(self.show_sound_menu, |d| {
                d.on_scroll_wheel(cx.listener(|this, _, _, cx| {
                    if this.show_sound_menu {
                        this.show_sound_menu = false;
                        this.sound_menu_scroll_y = None;
                        cx.notify();
                    }
                }))
            })
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
                            .child(text_button("settings-done-btn", "Done", t).on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.close_settings(cx);
                                }),
                            )),
                    )
                    .child(self.render_appearance_section(t, cx))
                    .child(section_divider(t))
                    .child(self.render_notifications_section(t, cx))
                    .child(section_divider(t))
                    .child(self.render_background_section(t, cx))
                    .child(section_divider(t))
                    .child(self.render_agents_section(t, cx))
                    .child(section_divider(t))
                    .child(self.render_skills_entry(t, cx))
                    .child(section_divider(t))
                    .child(self.render_browser_section(t, cx))
                    .child(section_divider(t))
                    .child(self.render_connections_section(t, cx)),
            )
    }

    fn render_agents_section(&mut self, t: Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(h) = self
            .harness_status
            .clone()
            .unwrap_or_default()
            .into_iter()
            .next()
        else {
            return div()
                .flex()
                .flex_col()
                .child(section_header(
                    "Accounts",
                    "Sign in to a model provider so Keel can use its models.",
                    None,
                    t,
                ))
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
    fn render_account_row(
        &self,
        _ix: usize,
        account: Account,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let reduced = self.reduced();
        let active = self
            .sign_in
            .as_ref()
            .filter(|(id, _)| *id == account.id)
            .map(|(_, s)| s.clone());
        let small_button = |id: String, label: &'static str, primary: bool| {
            div()
                .id(ElementId::Name(id.into()))
                .flex_none()
                .px_2p5()
                .py_1()
                .rounded(RADIUS_MD)
                .when(primary, |d| {
                    d.border_1()
                        .border_color(c(t.border))
                        .text_color(c(t.text))
                        .font_weight(gpui::FontWeight::MEDIUM)
                })
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
                    "working" if !sign_in.message.is_empty() => {
                        sign_in.message.trim_end_matches('.').to_string()
                    }
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
                .when(sign_in.state != "device", |d| {
                    d.child(div().mr_1().child(spinner))
                })
                .children(reopen)
                .child(
                    small_button(format!("signin-cancel-{}", account.id), "Cancel", false)
                        .on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.cancel_sign_in(cx)),
                        ),
                )
                .into_any_element()
        } else {
            if let Some((_, failed)) = self
                .sign_in
                .as_ref()
                .filter(|(id, s)| *id == account.id && s.state == "failed")
            {
                let why = failed.message.trim().trim_end_matches('.');
                subline = Some((
                    if why.is_empty() {
                        "Sign-in failed".to_string()
                    } else {
                        format!("Sign-in failed: {why}")
                    },
                    t.danger,
                ));
            }
            let id = account.id.clone();
            small_button(
                format!("signin-{}", account.id),
                if account.signed_in {
                    "Sign in again"
                } else {
                    "Sign in"
                },
                true,
            )
            .on_click(
                cx.listener(move |this, _: &ClickEvent, _, cx| this.start_sign_in(id.clone(), cx)),
            )
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
                    .text_color(c(if account.signed_in {
                        t.text
                    } else {
                        t.text_muted
                    }))
                    .child(Icon::provider(&account.id).el(zpx(15.))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .t_body()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(c(t.text))
                            .child(account.name.clone()),
                    )
                    .when_some(subline, |d, (text, color)| {
                        d.child(
                            div()
                                .t_small()
                                .text_color(c(color))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(text),
                        )
                    }),
            )
            .child(state)
    }

    /// Settings → Browser: which sites agents may load. The per-chat on/off
    /// switch lives in the browser pane's toolbar.
    fn render_browser_section(&mut self, t: Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let saved = self.state.ui_settings.browser_allowed_sites.clone();
        let typed =
            crate::browser::agent::parse_sites(&self.browser_sites.read(cx).value().to_string());
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

    fn render_connections_section(
        &mut self,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let busy = self.connect_busy;
        let error = self.connect_error.clone();
        let non_dotify: Vec<_> = self
            .connections
            .clone()
            .into_iter()
            .filter(|c| c.kind != "dotify")
            .collect();

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
                            .child(
                                div()
                                    .size(zpx(7.))
                                    .flex_none()
                                    .rounded_full()
                                    .bg(c(t.success)),
                            )
                            .child(
                                div()
                                    .t_body()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(c(t.text))
                                    .child(if conn.kind == "gitlab" {
                                        "GitLab".to_string()
                                    } else {
                                        conn.kind.clone()
                                    }),
                            )
                            .child(div().t_small().text_color(c(t.text_muted)).child(format!(
                                "{} · @{}",
                                conn.host.trim_start_matches("https://"),
                                conn.username
                            )))
                            .when(conn.name != conn.username, |d| {
                                d.child(
                                    div()
                                        .t_small()
                                        .text_color(c(t.text_faint))
                                        .child(conn.name.clone()),
                                )
                            }),
                    )
                    .child(
                        text_button_owned(format!("disconnect-{}", conn.host), "Disconnect", t)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.disconnect(kind.clone(), host.clone(), cx)
                            })),
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
            .child(section_header(
                "Connections",
                "Services Keel can talk to for you. Tokens stay on this Mac, readable only by you.",
                None,
                t,
            ))
            .when(self.dotify_visible(), |s| {
                s.child(self.render_dotify_settings_card(t, cx))
            })
            .when(!conn_rows.is_empty(), |s| {
                s.child(settings_list(conn_rows, t))
                    .child(div().h(gpui::px(1.)).w_full().bg(c(t.border)).my_3())
            })
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

    fn notify_toggle(
        &mut self,
        id: &'static str,
        change: impl FnOnce(&mut keel_proto::NotifySettings),
    ) {
        self.note_switch_flip(id);
        self.set_notify(change);
    }

    pub(super) fn preview_sound(&mut self, cx: &mut Context<Self>) {
        let n = &self.state.ui_settings.notifications;
        if let Some(path) = crate::background::sound_path(&self.state.data_dir, &n.sound) {
            crate::platform_notify::play_sound(&path, n.volume.max(0.05));
            self.sound_preview_epoch = self.sound_preview_epoch.wrapping_add(1);
            let epoch = self.sound_preview_epoch;
            self.note_switch_flip("sound-preview");
            cx.notify();
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(850))
                    .await;
                let _ = this.update(cx, |this, cx| {
                    if this.sound_preview_epoch == epoch {
                        this.sound_preview_epoch = 0;
                        this.note_switch_flip("sound-preview");
                        cx.notify();
                    }
                });
            })
            .detach();
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
            let result = cx
                .background_spawn(async move { crate::background::import_sound(&data_dir, &path) })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(name) => {
                        this.import_error = None;
                        this.set_notify(|n| n.sound = keel_proto::SoundChoice::Custom(name));
                        this.preview_sound(cx);
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
            let result = cx
                .background_spawn(
                    async move { crate::background::import_background(&data_dir, &path) },
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(name) => {
                        this.import_error = None;
                        this.state.ui_settings.background = Some(name);
                        this.state.save_settings();
                        this.forget_hero_path();
                    }
                    Err(err) => {
                        this.import_error = Some(format!("Couldn't use that picture: {err}"))
                    }
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

        let is_reduced = self.state.effective_reduced_motion(self.os_reduced_motion);

        let rows = vec![
            setting_row(
                t,
                "Theme",
                Some("Interface color scheme"),
                self.render_segmented_control(
                    "theme",
                    &theme_options,
                    current_theme,
                    58.0,
                    t,
                    cx,
                    |this, choice, _| {
                        this.state.set_theme(choice);
                    },
                )
                .into_any_element(),
            ),
            setting_row(
                t,
                "Reduced motion",
                Some("Minimize interface animations"),
                toggle(
                    "settings-motion".into(),
                    is_reduced,
                    self.recent_flip("settings-motion"),
                    t,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    let cur = this.state.effective_reduced_motion(this.os_reduced_motion);
                    this.note_switch_flip("settings-motion");
                    this.state.set_reduced_motion(Some(!cur));
                    this.transcript.reduced_motion = !cur;
                    cx.notify();
                }))
                .into_any_element(),
            ),
        ];

        div()
            .flex()
            .flex_col()
            .child(section_header(
                "Appearance",
                "Theme and motion preferences.",
                None,
                t,
            ))
            .child(settings_list(rows, t))
    }

    fn render_notifications_section(
        &mut self,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        use keel_proto::{BadgeStyle, SoundChoice};
        let n = self.state.ui_settings.notifications.clone();
        let switch = |this: &Self, id: &'static str, on: bool| {
            toggle(id.to_string(), on, this.recent_flip(id), t)
        };

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
        let volumes = [
            (0.25_f32, "25%"),
            (0.5, "50%"),
            (0.75, "75%"),
            (1.0, "100%"),
        ];
        let nearest = volumes
            .iter()
            .min_by(|a, b| (a.0 - n.volume).abs().total_cmp(&(b.0 - n.volume).abs()))
            .map(|v| v.0)
            .unwrap_or(0.75);

        let sound_menu: Option<AnyElement> = if self.show_sound_menu {
            let mut options: Vec<(SoundChoice, String)> = vec![
                (SoundChoice::None, "None".into()),
                (
                    SoundChoice::Default,
                    crate::background::sound_label(&SoundChoice::Default),
                ),
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

            let weak_dismiss = cx.entity().downgrade();
            let dismiss = move |_: &gpui::MouseDownEvent, _: &mut gpui::Window, cx: &mut gpui::App| {
                let _ = weak_dismiss.update(cx, |this: &mut KeelWindow, cx| {
                    this.note_menu_dismissed("sound-list");
                    this.show_sound_menu = false;
                    cx.notify();
                });
            };

            let mut items: Vec<AnyElement> = options
                .into_iter()
                .enumerate()
                .map(|(ix, (choice, label))| {
                    let selected = choice == n.sound;
                    div()
                        .id(ElementId::Name(format!("sound-opt-{ix}").into()))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .rounded(RADIUS_SM)
                        .t_small()
                        .cursor_pointer()
                        .when(selected, |d| d.bg(c(t.selected)))
                        .when(!selected, |d| d.hover(|s| s.bg(c(t.hover))))
                        .child(
                            div()
                                .w(zpx(12.))
                                .text_color(c(t.accent))
                                .child(if selected { "✓" } else { "" }),
                        )
                        .child(label)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            let choice = choice.clone();
                            this.show_sound_menu = false;
                            this.set_notify(|n| n.sound = choice);
                            this.preview_sound(cx);
                            cx.notify();
                        }))
                        .into_any_element()
                })
                .collect();

            items.push(
                div()
                    .id("sound-opt-custom")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded(RADIUS_SM)
                    .t_small()
                    .text_color(c(t.accent))
                    .cursor_pointer()
                    .hover(|s| s.bg(c(t.hover)))
                    .child(div().w(zpx(12.)))
                    .child("Custom…")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.show_sound_menu = false;
                        this.choose_sound(cx);
                    }))
                    .into_any_element(),
            );

            Some(super::floating_menu_positioned_with_max_h(
                "sound-list",
                t,
                180.,
                200.,
                gpui::Anchor::TopLeft,
                None,
                false,
                None,
                self.reduced(),
                dismiss,
                items,
            ))
        } else {
            None
        };

        let is_playing = self.sound_preview_epoch > 0;
        let is_none = matches!(n.sound, SoundChoice::None);
        let preview_flip = self
            .switch_flips
            .get("sound-preview")
            .filter(|(_, at)| !self.reduced() && at.elapsed() < std::time::Duration::from_millis(220))
            .map(|(n, _)| *n);

        let sound_anchor = div()
            .relative()
            .child(
                div()
                    .id("sound-picker")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_2p5()
                    .h(zpx(26.))
                    .rounded(RADIUS_MD)
                    .bg(if self.show_sound_menu {
                        c(t.selected)
                    } else {
                        c(t.input)
                    })
                    .border_1()
                    .border_color(if self.show_sound_menu {
                        c(t.accent)
                    } else {
                        c(t.border)
                    })
                    .t_small()
                    .text_color(c(t.text))
                    .cursor_pointer()
                    .hover(|s| s.bg(c(t.hover)))
                    .active(|s| s.bg(c(t.selected)).opacity(0.85))
                    .child(
                        Icon::Volume2
                            .el(zpx(12.))
                            .text_color(c(if is_none { t.text_faint } else { t.text_muted })),
                    )
                    .child(sound_label)
                    .child(
                        Icon::ChevronDown
                            .el(zpx(11.))
                            .text_color(c(t.text_muted))
                            .with_transformation(gpui::Transformation::rotate(gpui::radians(
                                if self.show_sound_menu {
                                    std::f32::consts::PI
                                } else {
                                    0.0
                                },
                            ))),
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        if this.menu_just_dismissed("sound-list") {
                            return;
                        }
                        this.show_sound_menu = !this.show_sound_menu;
                        cx.notify();
                    })),
            )
            .children(sound_menu);

        let sound_control = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(sound_anchor)
            .when(!is_none, |d| {
                d.child(
                    render_sound_preview_btn(is_playing, preview_flip, self.reduced(), t)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.preview_sound(cx))),
                )
            });

        let sound_row = setting_row(
            t,
            "Sound",
            Some("Played when an agent finishes or needs attention"),
            sound_control.into_any_element(),
        );

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
                        this.notify_toggle("notify-input", |n| {
                            n.on_needs_input = !n.on_needs_input
                        });
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
                self.render_segmented_control(
                    "badge",
                    &[
                        ("Count", BadgeStyle::Count),
                        ("Dot", BadgeStyle::Dot),
                        ("None", BadgeStyle::None),
                    ],
                    n.badge,
                    52.0,
                    t,
                    cx,
                    |this, style, _| {
                        this.set_notify(|n| n.badge = style);
                    },
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
                        if this.state.ui_settings.notifications.banner
                            && banner_permission() == BannerPermission::NotAsked
                        {
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
            self.render_segmented_control(
                "volume",
                &[
                    ("25%", 0.25_f32),
                    ("50%", 0.5),
                    ("75%", 0.75),
                    ("100%", 1.0),
                ],
                nearest,
                46.0,
                t,
                cx,
                |this, volume, cx| {
                    this.set_notify(|n| n.volume = volume);
                    this.preview_sound(cx);
                },
            )
            .into_any_element(),
        ));
        rows.push(setting_row(
            t,
            "Only when Keel is in the background",
            None,
            switch(self, "notify-sound-bg", n.sound_in_background_only)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.notify_toggle("notify-sound-bg", |n| {
                        n.sound_in_background_only = !n.sound_in_background_only
                    });
                    cx.notify();
                }))
                .into_any_element(),
        ));

        div()
            .flex()
            .flex_col()
            .child(section_header(
                "Notifications",
                "When an agent finishes, needs you, or fails in a chat you aren't looking at.",
                None,
                t,
            ))
            .child(settings_list(rows, t))
    }

    fn render_background_section(&mut self, t: Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let custom = crate::background::background_path(
            &self.state.data_dir,
            self.state.ui_settings.background.as_deref(),
        );
        let dim = self.state.ui_settings.background_dim.unwrap_or(0.);
        let steps = [
            (0.0_f32, "Off"),
            (0.2, "20%"),
            (0.4, "40%"),
            (0.6, "60%"),
            (0.8, "80%"),
        ];
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
            .when(dim > 0., |d| {
                d.child(div().absolute().inset_0().bg(gpui::hsla(0., 0., 0., dim)))
            });
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
                            .child(if custom.is_some() {
                                "Your picture"
                            } else {
                                "Built-in landscape"
                            }),
                    )
                    .child(div().t_small().text_color(c(t.text_muted)).child(
                        "PNG, JPEG, HEIC or WebP. Keel keeps its own copy, scaled to 2560 px.",
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .pt_1()
                            .child(
                                filled_button_owned("bg-choose-btn".into(), "Choose image…", t)
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.choose_background(cx)
                                    })),
                            )
                            .when(custom.is_some(), |d| {
                                d.child(
                                    text_button("bg-reset-btn", "Reset to default", t).on_click(
                                        cx.listener(|this, _: &ClickEvent, _, cx| {
                                            this.reset_background(cx)
                                        }),
                                    ),
                                )
                            }),
                    ),
            );

        let dim_row = setting_row(
            t,
            "Dim",
            Some("Darken the picture behind the text"),
            self.render_segmented_control(
                "dim",
                &[
                    ("Off", 0.0_f32),
                    ("20%", 0.2),
                    ("40%", 0.4),
                    ("60%", 0.6),
                    ("80%", 0.8),
                ],
                nearest,
                42.0,
                t,
                cx,
                |this, step, _| {
                    this.state.ui_settings.background_dim = (step > 0.).then_some(step);
                    this.state.save_settings();
                },
            )
            .into_any_element(),
        );

        let glow_row = setting_row(
            t,
            "Chat glow",
            Some("A faint glow of the picture under the chat input"),
            self.render_segmented_control(
                "chat-glow",
                &[("Off", true), ("On", false)],
                self.state.ui_settings.chat_glow_off,
                46.0,
                t,
                cx,
                |this, off, _| {
                    this.state.ui_settings.chat_glow_off = off;
                    this.state.save_settings();
                },
            )
            .into_any_element(),
        );

        let rows = vec![picture_row, dim_row, glow_row];

        div()
            .flex()
            .flex_col()
            .child(section_header(
                "Background",
                "The picture behind the new-chat screen.",
                None,
                t,
            ))
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

    fn pi_missing(&self) -> bool {
        self.harness_status
            .as_ref()
            .is_some_and(|rows| rows.first().is_some_and(|row| !row.installed))
    }

    pub(super) fn load_skills(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.skills.load_gen += 1;
        let ticket = self.skills.load_gen;
        cx.spawn(async move |this, cx| {
            let res = client
                .call("list_skills", serde_json::json!({ "cwd": serde_json::Value::Null }))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.skills.load_gen != ticket {
                    return;
                }
                this.skills.rows = Some(match res {
                    Ok(v) => Ok(v["skills"]
                        .as_array()
                        .map(|rows| {
                            rows.iter()
                                .map(SkillRow::parse)
                                .filter(|r| r.scope != "project")
                                .collect()
                        })
                        .unwrap_or_default()),
                    Err(err) => Err(err),
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn render_skills_entry(&mut self, t: Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let count = match &self.skills.rows {
            Some(Ok(rows)) if rows.is_empty() => "None yet".to_string(),
            Some(Ok(rows)) => {
                let on = rows.iter().filter(|row| row.enabled).count();
                let off = rows.len() - on;
                match (on, off) {
                    (on, 0) => format!("{on} on"),
                    (0, off) => format!("{off} off"),
                    (on, off) => format!("{on} on, {off} off"),
                }
            }
            _ => String::new(),
        };
        // A section of its own, same title and blurb as Appearance or Browser,
        // not another account row. The count and chevron are the way in.
        div()
            .id("skills-open-row")
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap_4()
            .cursor_pointer()
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.open_skills(cx)))
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_size(zpx(15.))
                            .line_height(zpx(20.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(c(t.text))
                            .child("Skills"),
                    )
                    .child(
                        div()
                            .t_small()
                            .text_color(c(t.text_muted))
                            .child("Instructions the agent can load"),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .t_small()
                    .text_color(c(t.text_muted))
                    .child(count)
                    .child(
                        Icon::ChevronDown
                            .el(zpx(14.))
                            .text_color(c(t.text_faint))
                            .with_transformation(gpui::Transformation::rotate(gpui::radians(
                                -std::f32::consts::FRAC_PI_2,
                            ))),
                    ),
            )
    }

    pub(super) fn open_skills(&mut self, cx: &mut Context<Self>) {
        if self.skills.open {
            return;
        }
        self.skills.epoch = self.skills.epoch.wrapping_add(1);
        let n = self.skills.epoch;
        self.skills.open = true;
        self.skills.notice = None;
        self.skills.confirm = None;
        self.skills.added = None;
        self.skills.author_filter = None;
        self.skills.status_filter = SkillStatusFilter::All;
        self.skills.filter_epoch = 0;
        self.skills.expanded_skill = None;
        self.skills.modal_skill = None;
        self.skills.modal_leaving = None;
        self.skills
            .scroll
            .set_offset(gpui::point(gpui::px(0.), gpui::px(0.)));
        self.skills.list_scroll.scroll_to_item(0, gpui::ScrollStrategy::Top);

        if self.reduced() {
            self.skills.transition = None;
        } else {
            self.skills.transition = Some((n, false));
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(180))
                    .await;
                let _ = this.update(cx, |this, cx| {
                    if this.skills.transition == Some((n, false)) {
                        this.skills.transition = None;
                        cx.notify();
                    }
                });
            })
            .detach();
        }

        self.load_skills(cx);
        cx.notify();
    }

    fn close_skills(&mut self, cx: &mut Context<Self>) {
        if !self.skills.open {
            return;
        }
        self.skills.open = false;
        self.skills.confirm = None;
        self.skills.notice = None;
        self.skills.added = None;
        self.skills.author_filter = None;
        self.skills.expanded_skill = None;
        self.skills.modal_skill = None;
        self.skills.modal_leaving = None;

        if self.reduced() {
            self.skills.transition = None;
            cx.notify();
            return;
        }

        self.skills.epoch = self.skills.epoch.wrapping_add(1);
        let n = self.skills.epoch;
        self.skills.transition = Some((n, true));
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(160))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.skills.transition == Some((n, true)) {
                    this.skills.transition = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn open_skill_modal(&mut self, skill: &SkillRow, cx: &mut Context<Self>) {
        self.skills.modal_gen = self.skills.modal_gen.wrapping_add(1);
        self.skills.modal_skill = Some(skill.clone());
        self.skills.modal_leaving = None;
        self.skills.expanded_skill = Some(skill.path.clone());
        self.skills
            .modal_scroll
            .set_offset(gpui::point(gpui::px(0.), gpui::px(0.)));
        if !self.skills.expanded_content.contains_key(&skill.path) {
            let file = std::path::Path::new(&skill.path).join("SKILL.md");
            let text = std::fs::read_to_string(&file).unwrap_or_default();
            self.skills.expanded_content.insert(skill.path.clone(), text);
        }
        cx.notify();
    }

    pub(super) fn dismiss_skill_modal(&mut self, cx: &mut Context<Self>) {
        if let Some(skill) = self.skills.modal_skill.take() {
            self.skills.expanded_skill = None;
            if self.reduced() {
                self.skills.modal_leaving = None;
                cx.notify();
                return;
            }
            let generation = self.skills.modal_gen;
            self.skills.modal_leaving = Some((generation, skill));
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(crate::motion::FAST)
                    .await;
                let _ = this.update(cx, |this, cx| {
                    if this.skills.modal_leaving.as_ref().map(|(n, _)| *n) == Some(generation) {
                        this.skills.modal_leaving = None;
                        cx.notify();
                    }
                });
            })
            .detach();
            cx.notify();
        }
    }

    /// Settings stays mounted underneath and crossfades with a subtle 16 px slide.
    fn render_skills_drill(&mut self, t: Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui::AnimationExt as _;
        let reduced = self.reduced();
        let (epoch, leaving) = self.skills.transition.unwrap_or((0, false));
        let duration = if reduced {
            std::time::Duration::from_millis(1)
        } else if leaving {
            std::time::Duration::from_millis(160)
        } else {
            std::time::Duration::from_millis(180)
        };
        let home = self.render_settings_home(t, cx).into_any_element();
        let skills = self.render_skills_page(t, cx).into_any_element();
        div()
            .flex_1()
            .min_h_0()
            .relative()
            .overflow_hidden()
            .child(
                div().absolute().inset_0().child(
                    div()
                        .size_full()
                        .flex()
                        .flex_col()
                        .child(home)
                        .with_animation(
                            ("skills-drill-parent", epoch as usize),
                            gpui::Animation::new(duration).with_easing(crate::motion::ease_out),
                            move |el, delta| {
                                if leaving {
                                    el.opacity(delta).relative().left(zpx(-12. * (1. - delta)))
                                } else {
                                    el.opacity(1. - delta).relative().left(zpx(-12. * delta))
                                }
                            },
                        ),
                ),
            )
            .child(
                div().absolute().inset_0().overflow_hidden().child(
                    div()
                        .size_full()
                        .flex()
                        .flex_col()
                        .bg(c(t.window))
                        .child(skills)
                        .with_animation(
                            ("skills-drill-child", epoch as usize),
                            gpui::Animation::new(duration).with_easing(crate::motion::ease_out),
                            move |el, delta| {
                                if leaving {
                                    el.opacity(1. - delta).relative().left(zpx(16. * delta))
                                } else {
                                    el.opacity(delta).relative().left(zpx(16. * (1. - delta)))
                                }
                            },
                        ),
                ),
            )
    }

    fn render_skills_page(&mut self, t: Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let body = self.render_skills_body(t, cx);
        let modal = self.render_skill_modal(t, cx);
        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .child(
                div()
                    .w_full()
                    .max_w(TRANSCRIPT_MAX_WIDTH)
                    .h_full()
                    .px_8()
                    .pt_6()
                    .pb_4()
                    .flex()
                    .flex_col()
                    .child(body),
            )
            .children(modal)
    }

    fn render_skills_body(&mut self, t: Tokens, cx: &mut Context<Self>) -> gpui::Div {
        let missing = self.pi_missing();
        let query = self.skills_filter.read(cx).value().to_string();
        let needle = query.trim().to_lowercase();
        let rows = self.skills.rows.clone();
        let reduced = self.reduced();
        let mut body = div().size_full().flex().flex_col().gap_3();

        if missing {
            return body.child(
                div()
                    .py_3()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .t_body()
                    .text_color(c(t.danger))
                    .child(Icon::Alert.el(zpx(14.)))
                    .child("Keel's bundled Pi is missing"),
            );
        }

        let all_skills = match rows {
            None => return body,
            Some(Err(err)) => {
                return body.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .py_2()
                        .child(
                            div()
                                .t_body()
                                .text_color(c(t.text))
                                .child("Couldn't read skills."),
                        )
                        .child(div().t_small().text_color(c(t.danger)).child(err))
                        .child(text_button("skills-retry", "Retry", t).on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.load_skills(cx)),
                        )),
                );
            }
            Some(Ok(skills)) => skills,
        };

        let total = all_skills.len();
        let enabled_count = all_skills.iter().filter(|r| r.enabled).count();

        body = body.child(self.render_skills_header(t, missing, total, enabled_count, cx));

        if let Some(notice) = self.skills.notice.clone() {
            body = body.child(div().pb_2().t_small().text_color(c(t.danger)).child(notice));
        }
        if let Some(confirm) = self.skills.confirm.clone() {
            body = body.child(self.render_skill_confirm(&confirm, t, cx));
        }

        // Collect author stats
        let mut author_counts: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        for s in &all_skills {
            let auth = if s.author.is_empty() {
                if !s.collection.is_empty() {
                    s.collection.clone()
                } else {
                    "Other".to_string()
                }
            } else {
                s.author.clone()
            };
            *author_counts.entry(auth).or_insert(0) += 1;
        }
        let mut authors: Vec<(String, usize)> = author_counts.into_iter().collect();
        authors.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

        body = body.child(self.render_skill_controls(&authors, t, cx));

        // Filtering
        let status_filter = self.skills.status_filter;
        let author_filter = self.skills.author_filter.clone();

        let matched: Vec<SkillRow> = all_skills
            .into_iter()
            .filter(|row| {
                // Status filter
                match status_filter {
                    SkillStatusFilter::All => {}
                    SkillStatusFilter::Enabled => {
                        if !row.enabled {
                            return false;
                        }
                    }
                    SkillStatusFilter::Disabled => {
                        if row.enabled {
                            return false;
                        }
                    }
                }
                // Author filter
                if let Some(ref selected_auth) = author_filter {
                    let auth = if row.author.is_empty() {
                        if !row.collection.is_empty() {
                            &row.collection
                        } else {
                            "Other"
                        }
                    } else {
                        &row.author
                    };
                    if auth != selected_auth {
                        return false;
                    }
                }
                // Search query
                if !needle.is_empty() {
                    let matches_name = row.name.to_lowercase().contains(&needle);
                    let matches_desc = row.description.to_lowercase().contains(&needle);
                    let matches_auth = row.author.to_lowercase().contains(&needle);
                    let matches_coll = row.collection.to_lowercase().contains(&needle);
                    let matches_insert = row.insert.to_lowercase().contains(&needle);
                    if !matches_name
                        && !matches_desc
                        && !matches_auth
                        && !matches_coll
                        && !matches_insert
                    {
                        return false;
                    }
                }
                true
            })
            .collect();

        if matched.is_empty() {
            let is_filtering = !needle.is_empty()
                || author_filter.is_some()
                || status_filter != SkillStatusFilter::All;
            let empty_box = if is_filtering {
                div()
                    .py_12()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .child(Icon::Search.el(zpx(24.)).text_color(c(t.text_faint)))
                    .child(
                        div()
                            .t_body()
                            .text_color(c(t.text_muted))
                            .child("No skills match your search or filter."),
                    )
                    .child(
                        text_button("skills-reset-filters", "Reset filters", t).on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.skills.author_filter = None;
                                this.skills.status_filter = SkillStatusFilter::All;
                                this.skills.list_scroll.scroll_to_item(0, gpui::ScrollStrategy::Top);
                                cx.notify();
                            }),
                        ),
                    )
            } else {
                div()
                    .py_12()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .child(Icon::Puzzle.el(zpx(26.)).text_color(c(t.text_faint)))
                    .child(
                        div()
                            .t_body()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(c(t.text))
                            .child("No skills yet"),
                    )
                    .child(
                        div()
                            .t_small()
                            .text_color(c(t.text_muted))
                            .child("Click “+ Add skill…” to install a skill folder into Keel."),
                    )
            };
            return body.child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(empty_box),
            );
        }

        let count = matched.len();
        let weak = cx.entity().downgrade();
        let switch_flips = self.switch_flips.clone();
        let row_errors = self.skills.row_errors.clone();

        const SKILL_ROW_H: f32 = 62.0;

        let list_el = uniform_list("skills-list", count, move |range, _window, _cx| {
            range
                .map(|ix| {
                    let skill = &matched[ix];
                    let path = skill.path.clone();
                    let name = skill.name.clone();
                    let author = skill.author.clone();
                    let insert = skill.insert.clone();
                    let description = skill.description.clone();
                    let scope = skill.scope.clone();
                    let enabled = skill.enabled;
                    let packaged = scope == "package";
                    let error = row_errors.get(&path).cloned();
                    let flip = switch_flips
                        .get(&format!("skill-{path}"))
                        .filter(|(_, at)| !reduced && at.elapsed() < crate::motion::BASE * 2)
                        .map(|(n, _)| *n);

                    let weak_row = weak.clone();
                    let skill_for_click = skill.clone();
                    let path_for_toggle = path.clone();
                    let scope_for_toggle = scope.clone();

                    let card = div()
                        .id(ElementId::Name(format!("skill-card-{path}").into()))
                        .h(zpx(56.))
                        .w_full()
                        .px_3p5()
                        .py_2()
                        .rounded(RADIUS_MD)
                        .bg(c(t.input).opacity(0.35))
                        .border_1()
                        .border_color(c(t.border).opacity(0.5))
                        .hover(|s| s.bg(c(t.hover)).border_color(c(t.border)))
                        .cursor_pointer()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .on_click(move |_, _, cx| {
                            let skill = skill_for_click.clone();
                            let _ = weak_row.update(cx, |this, cx| {
                                this.open_skill_modal(&skill, cx);
                            });
                        })
                        // Left: Puzzle Icon + Name/Author/Slug + 1-line Description
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_3()
                                .child(
                                    div()
                                        .size(zpx(32.))
                                        .flex_none()
                                        .rounded(RADIUS_SM)
                                        .bg(if enabled {
                                            c(t.accent).opacity(0.12)
                                        } else {
                                            c(t.input)
                                        })
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(
                                            Icon::Puzzle.el(zpx(15.)).text_color(c(if enabled {
                                                t.accent
                                            } else {
                                                t.text_faint
                                            })),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .justify_center()
                                        .gap(zpx(2.))
                                        .child(
                                            div()
                                                .flex()
                                                .flex_row()
                                                .items_center()
                                                .gap_2()
                                                .min_w_0()
                                                .child(
                                                    div()
                                                        .text_size(zpx(13.5))
                                                        .line_height(zpx(18.))
                                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                                        .text_color(c(t.text))
                                                        .child(name.clone()),
                                                )
                                                .when(!author.is_empty(), |d| {
                                                    d.child(
                                                        div()
                                                            .id(ElementId::Name(
                                                                format!("skill-row-author-{path}").into(),
                                                            ))
                                                            .flex()
                                                            .flex_row()
                                                            .items_center()
                                                            .px_1p5()
                                                            .h(zpx(17.))
                                                            .rounded(RADIUS_SM)
                                                            .bg(c(t.window))
                                                            .border_1()
                                                            .border_color(c(t.border).opacity(0.5))
                                                            .child(
                                                                div()
                                                                    .text_size(zpx(11.))
                                                                    .font_weight(gpui::FontWeight::MEDIUM)
                                                                    .text_color(c(t.text_muted))
                                                                    .child(format!("by {author}")),
                                                            ),
                                                    )
                                                })
                                                .child(
                                                    div()
                                                        .px_1p5()
                                                        .h(zpx(17.))
                                                        .flex()
                                                        .items_center()
                                                        .rounded(RADIUS_SM)
                                                        .bg(c(t.window))
                                                        .border_1()
                                                        .border_color(c(t.border).opacity(0.4))
                                                        .font_family(MONO)
                                                        .text_size(zpx(10.5))
                                                        .text_color(c(t.accent).opacity(0.9))
                                                        .child(insert.clone()),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .min_w_0()
                                                .truncate()
                                                .text_size(zpx(12.))
                                                .line_height(zpx(16.))
                                                .text_color(c(t.text_muted))
                                                .child(description.clone()),
                                        ),
                                ),
                        )
                        // Right: Error dot (if any) + "Instructions" button + Toggle Switch
                        .child(
                            div()
                                .flex_none()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_3()
                                .when(error.is_some(), |d| {
                                    d.child(
                                        div()
                                            .px_1p5()
                                            .py_0p5()
                                            .rounded(RADIUS_SM)
                                            .bg(c(t.danger).opacity(0.12))
                                            .text_size(zpx(11.))
                                            .text_color(c(t.danger))
                                            .child("Error"),
                                    )
                                })
                                .child(
                                    div()
                                        .id(ElementId::Name(format!("skill-row-btn-{path}").into()))
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap_1p5()
                                        .px_2()
                                        .py_1()
                                        .rounded(RADIUS_SM)
                                        .bg(c(t.hover).opacity(0.5))
                                        .border_1()
                                        .border_color(c(t.border).opacity(0.4))
                                        .hover(|s| s.bg(c(t.hover)).border_color(c(t.border)))
                                        .t_small()
                                        .text_color(c(t.text_muted))
                                        .child(
                                            Icon::ScrollText
                                                .el(zpx(12.))
                                                .text_color(c(t.text_faint)),
                                        )
                                        .child("Instructions"),
                                )
                                .child(if packaged {
                                    div()
                                        .t_small()
                                        .text_color(c(t.text_faint))
                                        .child("In package")
                                        .into_any_element()
                                } else {
                                    let weak_toggle = weak.clone();
                                    let path_t = path_for_toggle.clone();
                                    let scope_t = scope_for_toggle.clone();
                                    toggle(format!("skill-{path_t}"), enabled, flip, t)
                                        .on_click(move |_, _, cx| {
                                            cx.stop_propagation();
                                            let path = path_t.clone();
                                            let scope = scope_t.clone();
                                            let _ = weak_toggle.update(cx, |this, cx| {
                                                this.toggle_skill(path, !enabled, scope, cx);
                                            });
                                        })
                                        .into_any_element()
                                }),
                        );

                    div()
                        .id(("skill-row", ix))
                        .h(zpx(SKILL_ROW_H))
                        .w_full()
                        .pb(zpx(6.))
                        .child(card)
                        .into_any_element()
                })
                .collect()
        })
        .track_scroll(&self.skills.list_scroll)
        .size_full();

        let filter_epoch = self.skills.filter_epoch as usize;
        let animated_list = if reduced {
            div().flex_1().min_h_0().w_full().child(list_el).into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .with_animation(
                    ("skills-filter-list", filter_epoch),
                    gpui::Animation::new(crate::motion::BASE).with_easing(crate::motion::ease_out),
                    |el, delta| {
                        let ease = crate::motion::ease_out(delta);
                        el.opacity(0.15 + 0.85 * ease)
                            .relative()
                            .top(zpx(8. * (1.0 - ease)))
                    },
                )
                .child(list_el)
                .into_any_element()
        };

        body.child(animated_list)
    }

    fn render_skills_header(
        &mut self,
        t: Tokens,
        missing: bool,
        total: usize,
        enabled_count: usize,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .pb_4()
                    .child(
                        div()
                            .id("skills-back-btn")
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1p5()
                            .py_1()
                            .pr_1()
                            .rounded(RADIUS_MD)
                            .t_small()
                            .text_color(c(t.text_muted))
                            .cursor_pointer()
                            .hover(|s| s.text_color(c(t.text)))
                            .active(|s| s.opacity(0.65))
                            .child(Icon::Back.el(zpx(14.)))
                            .child("Settings")
                            .on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| this.close_skills(cx)),
                            ),
                    )
                    .when(!missing, |d| {
                        d.child(
                            filled_button("skills-add-btn", "+ Add skill…", t.accent, t).on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.choose_skill_folder(cx)
                                }),
                            ),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_end()
                    .justify_between()
                    .pb_2()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(zpx(20.))
                                    .line_height(zpx(26.))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(c(t.text))
                                    .child("Skills"),
                            )
                            .child(
                                div().t_small().text_color(c(t.text_muted)).child(
                                    "Skills available to Keel across all your chats. Inspect instructions and manage access.",
                                ),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .t_small()
                            .text_color(c(t.text_faint))
                            .child(format!("{enabled_count} of {total} active")),
                    ),
            )
    }

    fn render_skill_controls(
        &mut self,
        authors: &[(String, usize)],
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let author_filter = self.skills.author_filter.clone();
        let status_filter = self.skills.status_filter;

        div()
            .flex()
            .flex_col()
            .gap_2p5()
            .pb_2()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    // Search box
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h(zpx(34.))
                            .px_2p5()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .rounded(RADIUS_MD)
                            .bg(c(t.input))
                            .border_1()
                            .border_color(c(t.border))
                            .t_body()
                            .text_color(c(t.text_muted))
                            .child(Icon::Search.el(zpx(14.)).text_color(c(t.text_faint)))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(self.skills_filter.clone().into_any_element()),
                            ),
                    )
                    // Status segmented control
                    .child(self.render_segmented_control(
                        "skill-status",
                        &[
                            ("All", SkillStatusFilter::All),
                            ("Enabled", SkillStatusFilter::Enabled),
                            ("Disabled", SkillStatusFilter::Disabled),
                        ],
                        status_filter,
                        66.0,
                        t,
                        cx,
                        |this, mode, _| {
                            this.skills.status_filter = mode;
                            this.skills.filter_epoch = this.skills.filter_epoch.wrapping_add(1);
                        },
                    )),
            )
            // Author / Provenance pills
            .when(!authors.is_empty(), |d| {
                d.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .flex_wrap()
                        .gap_1p5()
                        .pb_1()
                        .child(
                            creator_pill(
                                "skill-author-all".into(),
                                "All Creators".into(),
                                author_filter.is_none(),
                                t,
                            )
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.skills.author_filter = None;
                                this.skills.filter_epoch = this.skills.filter_epoch.wrapping_add(1);
                                cx.notify();
                            })),
                        )
                        .children(authors.iter().map(|(author, count)| {
                            let is_sel = author_filter.as_deref() == Some(author);
                            let author_val = author.clone();
                            creator_pill(
                                format!("skill-author-{author}"),
                                format!("{author} ({count})"),
                                is_sel,
                                t,
                            )
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                if this.skills.author_filter.as_deref() == Some(&author_val) {
                                    this.skills.author_filter = None;
                                } else {
                                    this.skills.author_filter = Some(author_val.clone());
                                }
                                this.skills.filter_epoch = this.skills.filter_epoch.wrapping_add(1);
                                cx.notify();
                            }))
                            .into_any_element()
                        })),
                )
            })
    }

    fn render_skill_modal(
        &mut self,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (skill, leaving, modal_gen) = match (&self.skills.modal_skill, &self.skills.modal_leaving) {
            (Some(s), _) => (s.clone(), false, self.skills.modal_gen),
            (None, Some((n, s))) => (s.clone(), true, *n),
            _ => return None,
        };

        let reduced = self.reduced();
        let name = skill.name.clone();
        let path = skill.path.clone();
        let author = skill.author.clone();
        let source_url = skill.source_url.clone();
        let insert = skill.insert.clone();
        let description = skill.description.clone();
        let removable = skill.removable && skill.scope != "package" && skill.scope != "shared";

        let instructions = self
            .skills
            .expanded_content
            .get(&path)
            .cloned()
            .unwrap_or_else(|| {
                let file = std::path::Path::new(&path).join("SKILL.md");
                std::fs::read_to_string(&file).unwrap_or_default()
            });

        let mut card = div()
            .id(("skill-instruction-modal-card", modal_gen as usize))
            .w(zpx(720.))
            .max_w(gpui::relative(0.92))
            .max_h(gpui::relative(0.85))
            .rounded(crate::style::RADIUS_LG)
            .bg(c(t.surface))
            .border_1()
            .border_color(c(t.border))
            .floating_surface(t)
            .flex()
            .flex_col()
            .overflow_hidden()
            .on_click(|_: &ClickEvent, _, cx| cx.stop_propagation());

        // Header
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap_3()
            .px_5()
            .py_3p5()
            .border_b_1()
            .border_color(c(t.border).opacity(0.5))
            .bg(c(t.window).opacity(0.5))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2p5()
                    .flex_wrap()
                    .child(
                        div()
                            .size(zpx(28.))
                            .rounded(RADIUS_SM)
                            .bg(c(t.accent).opacity(0.12))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(Icon::Puzzle.el(zpx(15.)).text_color(c(t.accent))),
                    )
                    .child(
                        div()
                            .text_size(zpx(16.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(c(t.text))
                            .child(name.clone()),
                    )
                    .when(!author.is_empty(), |d| {
                        d.child(
                            div()
                                .id(ElementId::Name(format!("skill-modal-author-{name}").into()))
                                .flex()
                                .flex_row()
                                .items_center()
                                .px_2()
                                .h(zpx(20.))
                                .rounded(RADIUS_SM)
                                .bg(c(t.window))
                                .border_1()
                                .border_color(c(t.border).opacity(0.5))
                                .child(
                                    div()
                                        .text_size(zpx(11.5))
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(c(t.text_muted))
                                        .child(format!("by {author}")),
                                ),
                        )
                    })
                    .child(
                        div()
                            .px_2()
                            .h(zpx(20.))
                            .flex()
                            .items_center()
                            .rounded(RADIUS_SM)
                            .bg(c(t.window))
                            .border_1()
                            .border_color(c(t.border).opacity(0.5))
                            .font_family(MONO)
                            .text_size(zpx(11.5))
                            .text_color(c(t.accent))
                            .child(insert),
                    ),
            )
            .child(
                div()
                    .id("skill-modal-close-x")
                    .size(zpx(26.))
                    .rounded(RADIUS_SM)
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .text_color(c(t.text_muted))
                    .hover(|s| s.bg(c(t.hover)).text_color(c(t.text)))
                    .active(|s| s.opacity(0.7))
                    .child(Icon::Close.el(zpx(14.)))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.dismiss_skill_modal(cx);
                    })),
            );
        card = card.child(header);

        // Body: Scrollable markdown content with description callout
        let body_content = div()
            .id("skill-modal-scroll")
            .track_scroll(&self.skills.modal_scroll)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_5()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .p_3()
                    .rounded(RADIUS_MD)
                    .bg(c(t.input).opacity(0.4))
                    .border_1()
                    .border_color(c(t.border).opacity(0.5))
                    .t_body()
                    .text_color(c(t.text))
                    .child(description),
            )
            .child(render_skill_instructions(&instructions, t));
        card = card.child(body_content);

        // Footer
        let path_finder = path.clone();
        let footer = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px_5()
            .py_3()
            .border_t_1()
            .border_color(c(t.border).opacity(0.5))
            .bg(c(t.window).opacity(0.4))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .child({
                        let path_finder = path_finder.clone();
                        row_text("skill-modal-finder", "Reveal in Finder", t.text_faint, t)
                            .on_click(cx.listener(move |_, _: &ClickEvent, _, _| {
                                reveal_in_finder(&path_finder);
                            }))
                    })
                    .when(!source_url.is_empty(), |d| {
                        let url = source_url.clone();
                        d.child(
                            row_text("skill-modal-source", "View source ↗", t.text_faint, t)
                                .on_click(cx.listener(move |_, _: &ClickEvent, _, _| {
                                    open_browser_url(&url);
                                })),
                        )
                    })
                    .when(removable, |d| {
                        let path_remove = path_finder.clone();
                        let name_remove = name.clone();
                        d.child(
                            row_text("skill-modal-remove", "Remove skill", t.danger, t).on_click(
                                cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.dismiss_skill_modal(cx);
                                    this.skills.confirm = Some(SkillConfirm::Remove {
                                        path: path_remove.clone(),
                                        name: name_remove.clone(),
                                    });
                                    cx.notify();
                                }),
                            ),
                        )
                    }),
            )
            .child(
                filled_button("skill-modal-done-btn", "Done", t.accent, t).on_click(cx.listener(
                    |this, _: &ClickEvent, _, cx| {
                        this.dismiss_skill_modal(cx);
                    },
                )),
            );
        card = card.child(footer);

        // Motion for the modal card
        let card_el = if leaving {
            crate::motion::dismiss(card, ("skill-modal-card-out", modal_gen as usize), reduced)
        } else {
            crate::motion::appear(
                card,
                ("skill-modal-card-in", modal_gen as usize),
                crate::motion::BASE,
                14.,
                reduced,
            )
            .into_any_element()
        };

        // Scrim
        let scrim = div()
            .id(("skill-modal-scrim", modal_gen as usize))
            .absolute()
            .inset_0()
            .occlude()
            .bg(gpui::rgba(0x00000080))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.dismiss_skill_modal(cx);
            }))
            .child(card_el);

        let scrim = if reduced {
            scrim.into_any_element()
        } else if leaving {
            scrim
                .with_animation(
                    ("skill-scrim-out", modal_gen as usize),
                    gpui::Animation::new(crate::motion::FAST).with_easing(crate::motion::ease_in),
                    |el, delta| el.opacity(1.0 - delta),
                )
                .into_any_element()
        } else {
            scrim
                .with_animation(
                    ("skill-scrim-in", modal_gen as usize),
                    gpui::Animation::new(crate::motion::BASE).with_easing(crate::motion::ease_out),
                    |el, delta| el.opacity(delta),
                )
                .into_any_element()
        };

        Some(gpui::deferred(scrim).with_priority(3).into_any_element())
    }

    fn render_skill_confirm(
        &mut self,
        confirm: &SkillConfirm,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let (text, action, danger) = match confirm {
            SkillConfirm::Replace { name, .. } => (
                format!("“{name}” is already in your skills. Replace the existing folder?"),
                "Replace",
                true,
            ),
            SkillConfirm::Remove { name, .. } => (
                format!("Remove “{name}”? This permanently deletes its folder from your skills."),
                "Remove",
                true,
            ),
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap_3()
            .p_3()
            .mb_3()
            .rounded(RADIUS_MD)
            .bg(c(if danger { t.danger } else { t.accent }).opacity(0.1))
            .border_1()
            .border_color(c(if danger { t.danger } else { t.accent }).opacity(0.3))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .t_small()
                    .text_color(c(if danger { t.danger } else { t.text }))
                    .child(text),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        row_text("skills-confirm-go", action, if danger { t.danger } else { t.accent }, t)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.confirm_skill(cx))),
                    )
                    .child(
                        text_button("skills-confirm-cancel", "Cancel", t).on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| {
                                this.skills.confirm = None;
                                cx.notify();
                            },
                        )),
                    ),
            )
    }

    fn toggle_skill_expanded(&mut self, path: &str, cx: &mut Context<Self>) {
        if self.skills.modal_skill.as_ref().map(|s| s.path.as_str()) == Some(path) {
            self.dismiss_skill_modal(cx);
        } else if let Some(Ok(rows)) = self.skills.rows.as_ref() {
            if let Some(skill) = rows.iter().find(|r| r.path == path) {
                let skill_clone = skill.clone();
                self.open_skill_modal(&skill_clone, cx);
            }
        }
    }

    /// Debug socket: filter to the pack/author, or expand a skill by name.
    pub(super) fn dev_toggle_skill_pack(&mut self, label: &str, cx: &mut Context<Self>) {
        if let Some(Ok(rows)) = self.skills.rows.as_ref() {
            if rows.iter().any(|r| {
                r.author.eq_ignore_ascii_case(label) || r.collection.eq_ignore_ascii_case(label)
            }) {
                self.skills.author_filter =
                    if self.skills.author_filter.as_deref() == Some(label) {
                        None
                    } else {
                        Some(label.to_string())
                    };
                self.skills.filter_epoch = self.skills.filter_epoch.wrapping_add(1);
                cx.notify();
                return;
            }
            if let Some(skill) = rows.iter().find(|r| r.name.eq_ignore_ascii_case(label)) {
                let path = skill.path.clone();
                self.toggle_skill_expanded(&path, cx);
            }
        }
    }

    fn toggle_skill(&mut self, path: String, enabled: bool, scope: String, cx: &mut Context<Self>) {
        if let Some(Ok(rows)) = self.skills.rows.as_mut() {
            if let Some(row) = rows.iter_mut().find(|row| row.path == path) {
                row.enabled = enabled;
            }
        }
        self.skills.row_errors.remove(&path);
        self.note_switch_flip(&format!("skill-{path}"));
        let Some(client) = self.client.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let res = client
                .call(
                    "set_skill_enabled",
                    serde_json::json!({ "path": path, "enabled": enabled, "scope": scope, "cwd": serde_json::Value::Null }),
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                match res {
                    Ok(_) => {
                        this.skills.row_errors.remove(&path);
                        this.commands.clear();
                        this.load_skills(cx);
                    }
                    Err(_) => {
                        if let Some(Ok(rows)) = this.skills.rows.as_mut() {
                            if let Some(row) = rows.iter_mut().find(|row| row.path == path) {
                                row.enabled = !enabled;
                            }
                        }
                        this.note_switch_flip(&format!("skill-{path}"));
                        this.skills.row_errors.insert(path, "Couldn't update settings.".into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn choose_skill_folder(&mut self, cx: &mut Context<Self>) {
        if self.pi_missing() {
            return;
        }
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add skill".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.skills.notice = None;
                this.skills.confirm = None;
                let source = path.to_string_lossy().into_owned();
                this.submit_add_skill(source, "user", false, cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn submit_add_skill(
        &mut self,
        source: String,
        dest: &str,
        replace: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let dest = dest.to_string();
        let folder = std::path::Path::new(&source)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        cx.spawn(async move |this, cx| {
            let res = client
                .call(
                    "add_skill",
                    serde_json::json!({ "source": source, "dest": dest, "cwd": serde_json::Value::Null, "replace": replace }),
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                match res {
                    Ok(_) => {
                        this.skills.confirm = None;
                        this.skills.notice = None;
                        this.skills.added = Some((folder, "user".to_string()));
                        this.commands.clear();
                        this.load_skills(cx);
                    }
                    Err(err) => {
                        if let Some(name) = err.strip_prefix("exists:").map(str::to_string) {
                            this.skills.confirm = Some(SkillConfirm::Replace { source, dest, name });
                        } else {
                            this.skills.notice = Some(err);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn confirm_skill(&mut self, cx: &mut Context<Self>) {
        let Some(confirm) = self.skills.confirm.take() else {
            return;
        };
        match confirm {
            SkillConfirm::Replace { source, dest, .. } => {
                self.submit_add_skill(source, &dest, true, cx)
            }
            SkillConfirm::Remove { path, .. } => self.remove_skill(path, cx),
        }
    }

    fn remove_skill(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let res = client
                .call(
                    "remove_skill",
                    serde_json::json!({ "path": path, "cwd": serde_json::Value::Null }),
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                match res {
                    Ok(_) => {
                        this.skills.notice = None;
                        this.commands.clear();
                        this.load_skills(cx);
                    }
                    Err(err) => this.skills.notice = Some(err),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// Section title with subtitle.
fn section_header(
    title: &'static str,
    blurb: &'static str,
    action: Option<AnyElement>,
    t: Tokens,
) -> gpui::Div {
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
    div().flex().flex_col().children(
        rows.into_iter()
            .enumerate()
            .map(|(ix, row)| row.when(ix > 0, |d| d.border_t_1().border_color(c(t.border)))),
    )
}

/// A setting: its label (and an explanation) on the left, the control on the right.
fn setting_row(
    t: Tokens,
    label: &'static str,
    caption: Option<&'static str>,
    control: AnyElement,
) -> gpui::Div {
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
                .children(
                    caption.map(|text| div().t_small().text_color(c(t.text_muted)).child(text)),
                ),
        )
        .child(control)
}

/// Filter pill for creators.
fn creator_pill(
    id: String,
    label: String,
    selected: bool,
    t: Tokens,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(ElementId::Name(id.into()))
        .px_2p5()
        .h(zpx(23.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(RADIUS_SM)
        .t_small()
        .cursor_pointer()
        .border_1()
        .map(|d| {
            if selected {
                d.bg(c(t.accent).opacity(0.14))
                    .border_color(c(t.accent).opacity(0.8))
                    .text_color(c(t.accent))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .active(|s| s.opacity(0.85))
            } else {
                d.bg(c(t.hover).opacity(0.35))
                    .border_color(c(t.border).opacity(0.45))
                    .text_color(c(t.text_muted))
                    .hover(|s| {
                        s.bg(c(t.hover))
                            .border_color(c(t.border))
                            .text_color(c(t.text))
                    })
                    .active(|s| s.opacity(0.75))
            }
        })
        .child(label)
}

fn skill_markdown_body(raw: &str) -> String {
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    if !raw.starts_with("---") {
        return raw.trim().to_string();
    }
    let rest = raw[3..].trim_start_matches(['\r', '\n']);
    if let Some(end) = rest.find("\n---") {
        rest[end + 4..].trim().to_string()
    } else {
        raw.trim().to_string()
    }
}

fn render_skill_instructions(text: &str, t: Tokens) -> gpui::Div {
    let body = skill_markdown_body(text);
    if body.is_empty() {
        return div()
            .py_2()
            .t_small()
            .text_color(c(t.text_faint))
            .child("No instructions found in SKILL.md.");
    }
    let mut out = div().flex().flex_col().gap_2();
    let lines: Vec<&str> = body.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim_end();
        let trimmed = line.trim();
        if trimmed.is_empty() {
            i += 1;
            continue;
        }
        if let Some(heading) = trimmed.strip_prefix("### ") {
            out = out.child(
                div()
                    .pt_1()
                    .text_size(zpx(13.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(c(t.text))
                    .child(heading.to_string()),
            );
            i += 1;
        } else if let Some(heading) = trimmed.strip_prefix("## ") {
            out = out.child(
                div()
                    .pt_2()
                    .pb_0p5()
                    .border_b_1()
                    .border_color(c(t.border).opacity(0.3))
                    .text_size(zpx(14.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(c(t.text))
                    .child(heading.to_string()),
            );
            i += 1;
        } else if let Some(heading) = trimmed.strip_prefix("# ") {
            out = out.child(
                div()
                    .pt_2p5()
                    .pb_1()
                    .border_b_1()
                    .border_color(c(t.border).opacity(0.4))
                    .text_size(zpx(15.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(c(t.text))
                    .child(heading.to_string()),
            );
            i += 1;
        } else if trimmed.starts_with("```") {
            let mut code_lines = Vec::new();
            i += 1;
            while i < lines.len() && !lines[i].trim().starts_with("```") {
                code_lines.push(lines[i]);
                i += 1;
            }
            if i < lines.len() {
                i += 1;
            }
            out = out.child(
                div()
                    .my_1()
                    .p_2p5()
                    .rounded(RADIUS_SM)
                    .bg(c(t.input))
                    .border_1()
                    .border_color(c(t.border).opacity(0.5))
                    .font_family(MONO)
                    .text_size(zpx(11.5))
                    .line_height(zpx(16.))
                    .text_color(c(t.text))
                    .overflow_hidden()
                    .child(code_lines.join("\n")),
            );
        } else if let Some(quote) = trimmed.strip_prefix("> ") {
            out = out.child(
                div()
                    .my_1()
                    .pl_3()
                    .py_0p5()
                    .border_l_2()
                    .border_color(c(t.accent))
                    .text_size(zpx(12.5))
                    .line_height(zpx(18.))
                    .text_color(c(t.text_muted))
                    .child(quote.to_string()),
            );
            i += 1;
        } else if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
            let item = &trimmed[2..];
            out = out.child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap_2()
                    .pl_1()
                    .child(
                        div()
                            .text_size(zpx(12.5))
                            .line_height(zpx(18.))
                            .text_color(c(t.text_faint))
                            .child("•"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(zpx(12.5))
                            .line_height(zpx(18.))
                            .text_color(c(t.text_muted))
                            .child(item.to_string()),
                    ),
            );
            i += 1;
        } else {
            out = out.child(
                div()
                    .text_size(zpx(12.5))
                    .line_height(zpx(18.))
                    .text_color(c(t.text_muted))
                    .child(trimmed.to_string()),
            );
            i += 1;
        }
    }
    out
}

fn row_text(
    id: impl Into<String>,
    label: &'static str,
    color: u32,
    t: Tokens,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(ElementId::Name(id.into().into()))
        .px_2p5()
        .py_1()
        .flex_none()
        .rounded(RADIUS_MD)
        .t_small()
        .text_color(c(color))
        .cursor_pointer()
        .hover(|s| s.bg(c(t.hover)))
        .child(label)
}

fn reveal_in_finder(path: &str) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open")
        .args(["-R", path])
        .spawn();
    #[cfg(not(target_os = "macos"))]
    {
        let folder = std::path::Path::new(path)
            .parent()
            .unwrap_or(std::path::Path::new(path));
        let _ = std::process::Command::new("xdg-open").arg(folder).spawn();
    }
}

fn open_browser_url(url: &str) {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return;
    }
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(not(target_os = "macos"))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
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
        .child(
            div()
                .w(zpx(64.))
                .flex_none()
                .t_small()
                .text_color(c(t.text_muted))
                .child(label),
        )
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
/// knob slides across, stretching mid-travel with asymmetric leading/trailing edge
/// morphology, while the track and knob colors blend to their new state with
/// Emil Kowalski motion physics.
fn toggle(id: String, on: bool, flip: Option<u64>, t: Tokens) -> gpui::Stateful<gpui::Div> {
    use gpui::AnimationExt as _;
    const TRAVEL: f32 = 12.;
    const KNOB: f32 = 14.;

    let paint = move |is_on: bool, delta: f32| {
        // Delta progress 0..1 over the flip animation.
        // Asymmetric leading/trailing edge physics:
        let lead_t = crate::motion::ease_out_strong(delta);
        let lag_curve = crate::motion::glide_curve(delta);
        let trail_t = 0.55 * lead_t + 0.45 * lag_curve;

        let (left, width, track_progress) = if is_on {
            // Turning ON: moving right (right edge leads, left edge trails)
            let right = KNOB + TRAVEL * lead_t;
            let l = TRAVEL * trail_t;
            let w = (right - l).max(KNOB);
            (l, w, lead_t)
        } else {
            // Turning OFF: moving left (left edge leads, right edge trails)
            let l = TRAVEL * (1.0 - lead_t);
            let right = KNOB + TRAVEL * (1.0 - trail_t);
            let w = (right - l).max(KNOB);
            (l, w, 1.0 - lead_t)
        };

        let track_bg = mix(t.input, t.accent, track_progress);
        let track_border = mix(t.border, t.accent, track_progress);

        // Apple-style pure white knob in light theme, subtle muted in dark theme
        let knob_bg = if t == keel_theme::Tokens::dark() {
            mix(t.text_faint, t.text_on_accent, track_progress)
        } else {
            0xFFFFFFFF
        };

        div()
            .w(zpx(30.))
            .h(zpx(18.))
            .p(zpx(1.))
            .flex_none()
            .rounded_full()
            .flex()
            .items_center()
            .bg(c(track_bg))
            .border_1()
            .border_color(c(track_border))
            .child(
                div()
                    .h(zpx(KNOB))
                    .rounded_full()
                    .w(zpx(width))
                    .ml(zpx(left))
                    .bg(c(knob_bg))
                    .border_1()
                    .border_color(c(0x00000018))
                    .shadow(vec![gpui::BoxShadow {
                        color: c(0x00000028),
                        offset: gpui::point(zpx(0.), zpx(1.)),
                        blur_radius: zpx(1.5),
                        spread_radius: zpx(0.),
                        inset: false,
                    }]),
            )
    };

    let body: gpui::AnyElement = match flip {
        Some(n) => {
            let initial_track = paint(on, 0.0);
            initial_track
                .with_animation(
                    ElementId::Name(format!("{id}-flip-{n}").into()),
                    gpui::Animation::new(std::time::Duration::from_millis(150)),
                    move |_el, delta| paint(on, delta),
                )
                .into_any_element()
        }
        None => paint(on, 1.0).into_any_element(),
    };

    div()
        .id(ElementId::Name(id.into()))
        .flex_none()
        .cursor_pointer()
        .child(body)
}

/// Blend two `0xRRGGBBAA` colors, `at` 0 → `from`, 1 → `to`.
fn mix(from: u32, to: u32, at: f32) -> u32 {
    let at = at.clamp(0., 1.);
    (0..4).fold(0, |acc, i| {
        let shift = 24 - 8 * i;
        let (a, b) = (
            ((from >> shift) & 0xFF) as f32,
            ((to >> shift) & 0xFF) as f32,
        );
        acc | (((a + (b - a) * at).round() as u32) << shift)
    })
}

/// A morphing audio preview button: smoothly transitions between idle (Play + "Preview")
/// and active (Volume2 + "Playing…") with color blending, opacity crossfade,
/// and vertical micro-drift under Emil Kowalski / Apple design principles.
fn render_sound_preview_btn(
    is_playing: bool,
    flip: Option<u64>,
    reduced: bool,
    t: Tokens,
) -> gpui::Stateful<gpui::Div> {
    use gpui::AnimationExt as _;

    let paint = move |is_playing: bool, progress: f32| {
        // Asymmetric easing: snappy start into playing, gentle settle back to idle
        let curve = if is_playing {
            crate::motion::ease_out_strong(progress)
        } else {
            crate::motion::ease_out(progress)
        };
        let play_ratio = if is_playing {
            curve
        } else {
            1.0 - curve
        };

        // Smooth color interpolation between idle and playing states
        let accent_fill = (t.accent & 0xFFFFFF00) | 0x22;
        let accent_border = (t.accent & 0xFFFFFF00) | 0x66;
        let bg_color = mix(t.input, accent_fill, play_ratio);
        let border_color = mix(t.border, accent_border, play_ratio);
        let text_color = mix(t.text_muted, t.accent, play_ratio);

        // Micro-vertical drift and crossfade
        let idle_opacity = (1.0 - play_ratio).clamp(0.0, 1.0);
        let playing_opacity = play_ratio.clamp(0.0, 1.0);
        let idle_top = -3.5 * play_ratio;
        let playing_top = 3.5 * (1.0 - play_ratio);

        let idle_layer = div()
            .absolute()
            .inset_0()
            .flex()
            .flex_row()
            .items_center()
            .justify_center()
            .gap_1p5()
            .opacity(idle_opacity)
            .top(zpx(idle_top))
            .child(Icon::Play.el(zpx(10.)).text_color(c(text_color)))
            .child(
                div()
                    .t_small()
                    .text_color(c(text_color))
                    .child("Preview"),
            );

        let playing_layer = div()
            .absolute()
            .inset_0()
            .flex()
            .flex_row()
            .items_center()
            .justify_center()
            .gap_1p5()
            .opacity(playing_opacity)
            .top(zpx(playing_top))
            .child(Icon::Volume2.el(zpx(12.)).text_color(c(text_color)))
            .child(
                div()
                    .t_small()
                    .text_color(c(text_color))
                    .child("Playing…"),
            );

        div()
            .w(zpx(88.))
            .h(zpx(26.))
            .relative()
            .rounded(RADIUS_MD)
            .overflow_hidden()
            .bg(c(bg_color))
            .border_1()
            .border_color(c(border_color))
            .child(idle_layer)
            .child(playing_layer)
    };

    let body: gpui::AnyElement = match flip {
        Some(n) if !reduced => {
            let initial = paint(is_playing, 0.0);
            initial
                .with_animation(
                    ElementId::Name(format!("sound-preview-flip-{n}").into()),
                    gpui::Animation::new(std::time::Duration::from_millis(200)),
                    move |_el, delta| paint(is_playing, delta),
                )
                .into_any_element()
        }
        _ => paint(is_playing, 1.0).into_any_element(),
    };

    div()
        .id(ElementId::Name("sound-preview-btn".into()))
        .flex_none()
        .cursor_pointer()
        .hover(|s| s.opacity(0.92))
        .active(|s| s.opacity(0.75))
        .child(body)
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
        assert_eq!(
            (h.id.as_str(), h.installed, accounts.len()),
            ("keel", true, 2)
        );
        assert_eq!(
            (accounts[0].id.as_str(), accounts[0].hint.as_str()),
            ("anthropic", "Pro or Max plan")
        );
        assert_eq!(accounts[0].credential(), "Subscription");
        assert!(accounts[0].signed_in && !accounts[1].signed_in);
        // Not asked yet: no accounts, not an empty list.
        assert_eq!(
            HarnessStatus::parse(&serde_json::json!({"id": "keel", "accounts": null})).accounts,
            None
        );
        assert_eq!(
            SignInUi::parse(
                &serde_json::json!({"state": "device", "code": "0A50-65F9", "url": "https://github.com/login/device"})
            ),
            SignInUi {
                state: "device".into(),
                code: "0A50-65F9".into(),
                url: "https://github.com/login/device".into(),
                message: String::new()
            }
        );
        assert_eq!(
            HarnessStatus::parse(&serde_json::json!({"id": "keel"})).path,
            None
        );
    }

    #[test]
    fn skill_row_parses_author_and_source_url() {
        let v = serde_json::json!({
            "name": "animate",
            "scope": "user",
            "description": "Build fluid, physical animations",
            "prompt": "Instructions",
            "path": "/Users/test/.agents/skills/animate",
            "enabled": true,
            "manual_only": false,
            "collection": "emilkowalski/skill",
            "author": "Emil Kowalski",
            "source_url": "https://github.com/emilkowalski/skill"
        });
        let r = SkillRow::parse(&v);
        assert_eq!(r.name, "animate");
        assert_eq!(r.author, "Emil Kowalski");
        assert_eq!(r.source_url, "https://github.com/emilkowalski/skill");
        assert_eq!(r.collection, "emilkowalski/skill");
        assert!(r.enabled);
        assert!(!r.manual_only);
    }

    #[test]
    fn skill_row_parses_missing_author_gracefully() {
        let v = serde_json::json!({
            "name": "custom-skill",
            "scope": "user",
            "description": "A personal custom skill",
            "path": "/Users/test/.agents/skills/custom-skill",
            "enabled": false
        });
        let r = SkillRow::parse(&v);
        assert_eq!(r.name, "custom-skill");
        assert_eq!(r.author, "");
        assert_eq!(r.source_url, "");
        assert_eq!(r.collection, "");
        assert!(!r.enabled);
    }
}

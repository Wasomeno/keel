//! Settings → Updates, its relaunch confirm, and the "ready" line in the
//! window chrome (`UPDATER.md`, "Check, download, install" and "Settings
//! and UI"). The blocking work lives in `keel-update` and runs on the
//! background executor; this file owns the state machine and the drawing.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::{AnimationExt as _, ClickEvent, Context, ElementId, IntoElement, div, prelude::*, px};
use keel_theme::Tokens;
use keel_update::{Check, GateError, Offer, Ready, Status, UpdateError, msg};

use super::KeelWindow;
use super::settings::{section_header, setting_row, settings_list, text_button_owned, toggle};
use crate::hover_fade::HoverFadeExt;
use crate::icons::Icon;
use crate::style::{HEADER_HEIGHT, KeelStyled, RADIUS_MD, RADIUS_SM, c, zpx};

/// `fraction` slot for the download thread: f32 bits, or this for unknown.
const NO_FRACTION: u32 = u32::MAX;
/// How often the window reads the download's progress.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

pub(super) struct UpdateUi {
    pub status: Status,
    /// `None` until the launch-time gate ran (or there is no config).
    pub gate: Option<Result<keel_update::Gate, GateError>>,
    /// The staged build behind `Status::Ready`.
    pub ready: Option<Ready>,
    /// The release notes link for the version on offer or staged.
    pub notes_url: Option<String>,
    /// Bumped per check, so a stale result is dropped.
    pub ticket: u64,
    pub confirm: bool,
    pub confirm_gen: u64,
    pub confirm_leaving: Option<u64>,
    /// The chrome line was closed this launch (the build stays staged).
    pub line_dismissed: bool,
    pub line_gen: u64,
    pub line_leaving: Option<u64>,
}

impl Default for UpdateUi {
    fn default() -> Self {
        Self {
            status: Status::Idle,
            gate: None,
            ready: None,
            notes_url: None,
            ticket: 0,
            confirm: false,
            confirm_gen: 0,
            confirm_leaving: None,
            line_dismissed: false,
            line_gen: 0,
            line_leaving: None,
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// One manifest request: the answer, and whether it was parsed (so
/// `checked_at` is written only then).
fn check_once() -> keel_update::Result<Check> {
    let (url, loopback) = keel_update::manifest_url()?;
    let body = keel_update::fetch_manifest(&url)?;
    keel_update::evaluate(&body, keel_update::CURRENT_VERSION, &keel_update::arch_key(), loopback)
}

/// Download, verify, stage. The work dir goes on any failure.
fn fetch_and_stage(offer: &Offer, public_key: &str, progress: &AtomicU32) -> keel_update::Result<Ready> {
    let dir = keel_update::new_work_dir()?;
    let staged = (|| {
        let archive = keel_update::download(&offer.asset, &dir, |f| {
            progress.store(f.map_or(NO_FRACTION, f32::to_bits), Ordering::Relaxed);
        })?;
        keel_update::verify_signature(&archive, &offer.asset.signature, public_key)?;
        keel_update::stage(&archive, &offer.version, offer.notes_url.clone())
    })();
    match staged {
        Ok(ready) => {
            keel_update::cleanup_stale(&std::env::temp_dir(), keel_update::CURRENT_VERSION, Some(&ready.dir));
            Ok(ready)
        }
        Err(err) => {
            let _ = std::fs::remove_dir_all(&dir);
            Err(err)
        }
    }
}

impl KeelWindow {
    /// On launch: gate, sweep old temp dirs, pick up a build staged by an
    /// earlier launch, then the daily check when it is due and allowed.
    pub(super) fn updates_boot(&mut self, cx: &mut Context<Self>) {
        if keel_update::config().is_none() {
            return;
        }
        let data_dir = self.state.data_dir.clone();
        let auto = self.state.ui_settings.auto_update != Some(false);
        cx.spawn(async move |this, cx| {
            let (gate, ready, due) = cx
                .background_spawn(async move {
                    let gate = keel_update::gate();
                    if gate.is_err() {
                        return (gate, None, false);
                    }
                    let temp = std::env::temp_dir();
                    keel_update::cleanup_stale(&temp, keel_update::CURRENT_VERSION, None);
                    let ready = keel_update::find_ready(&temp, keel_update::CURRENT_VERSION);
                    let due = ready.is_none() && keel_update::check_due(&data_dir, now_secs());
                    (gate, ready, due)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let allowed = gate.is_ok();
                this.updates.gate = Some(gate);
                if let Some(ready) = ready {
                    this.updates_set_ready(ready);
                } else if allowed && auto && due {
                    this.updates_check(false, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn updates_set_ready(&mut self, ready: Ready) {
        self.updates.status = Status::Ready { version: ready.version.clone() };
        self.updates.notes_url = ready.notes_url.clone();
        self.updates.ready = Some(ready);
    }

    fn updates_busy(&self) -> bool {
        matches!(
            self.updates.status,
            Status::Checking | Status::Available { .. } | Status::Downloading { .. } | Status::Installing
        )
    }

    /// `Check now`. `manual` failures show in Settings; automatic ones log
    /// at info and fall back to `Idle`.
    pub(super) fn updates_check(&mut self, manual: bool, cx: &mut Context<Self>) {
        if self.updates_busy() || self.updates.ready.is_some() {
            return;
        }
        match &self.updates.gate {
            Some(Ok(_)) => {}
            Some(Err(gate)) => {
                if manual {
                    self.updates.status = Status::Failed { message: gate.message().into() };
                    cx.notify();
                }
                return;
            }
            None => return,
        }
        let Some(config) = keel_update::config() else {
            return;
        };
        self.updates.ticket += 1;
        let ticket = self.updates.ticket;
        self.updates.status = Status::Checking;
        let data_dir = self.state.data_dir.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_spawn(async move {
                    let res = check_once();
                    if res.is_ok() {
                        keel_update::mark_checked(&data_dir, now_secs());
                    }
                    res
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.updates.ticket != ticket {
                    return;
                }
                match res {
                    Ok(Check::UpToDate) => this.updates.status = Status::UpToDate,
                    Ok(Check::NoBuild { version }) => {
                        tracing::info!("update: {version} has no build for {}", keel_update::arch_key());
                        this.updates_fail(manual, UpdateError::new(msg::NO_BUILD, ""));
                    }
                    Ok(Check::Update(offer)) if !keel_update::public_key_usable(config.public_key) => {
                        tracing::info!("update: {} is out, but this build has no update key", offer.version);
                        this.updates_fail(manual, UpdateError::new(msg::NO_KEY, ""));
                    }
                    Ok(Check::Update(offer)) => this.updates_download(offer, config.public_key, cx),
                    Err(err) => {
                        tracing::info!("update: check failed: {err}");
                        this.updates_fail(manual, err);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn updates_fail(&mut self, manual: bool, err: UpdateError) {
        self.updates.status = if manual {
            Status::Failed { message: err.message }
        } else {
            Status::Idle
        };
    }

    /// Starts without asking: the download is the slow part, and applying
    /// it still waits for Relaunch.
    fn updates_download(&mut self, offer: Offer, public_key: &'static str, cx: &mut Context<Self>) {
        let ticket = self.updates.ticket;
        let version = offer.version.clone();
        self.updates.notes_url = offer.notes_url.clone();
        self.updates.status = Status::Downloading { version: version.clone(), fraction: None };
        let progress = Arc::new(AtomicU32::new(NO_FRACTION));
        // Progress: read on a slow timer while the download runs.
        let seen = progress.clone();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(PROGRESS_EVERY).await;
                let bits = seen.load(Ordering::Relaxed);
                let fraction = (bits != NO_FRACTION).then(|| f32::from_bits(bits));
                let going = this.update(cx, |this, cx| {
                    let Status::Downloading { fraction: shown, .. } = &mut this.updates.status else {
                        return false;
                    };
                    if this.updates.ticket != ticket {
                        return false;
                    }
                    if *shown != fraction {
                        *shown = fraction;
                        if this.settings_open {
                            cx.notify();
                        }
                    }
                    true
                });
                if !matches!(going, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_spawn(async move { fetch_and_stage(&offer, public_key, &progress) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.updates.ticket != ticket {
                    return;
                }
                match res {
                    Ok(ready) => {
                        tracing::info!("update: {} is staged", ready.version);
                        this.updates.line_dismissed = false;
                        this.updates_set_ready(ready);
                    }
                    // A refused archive or signature always shows, even
                    // after an automatic check: something is wrong upstream.
                    Err(err) => {
                        tracing::warn!("update: {version} not staged: {err}");
                        this.updates.status = Status::Failed { message: err.message };
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn turn_in_flight(&self) -> bool {
        self.state
            .chats
            .values()
            .any(|c| c.status == keel_proto::ChatStatus::Running)
    }

    pub(super) fn open_update_confirm(&mut self, cx: &mut Context<Self>) {
        if self.updates.ready.is_none() || self.updates_busy() {
            return;
        }
        self.updates.confirm_gen += 1;
        self.updates.confirm = true;
        self.updates.confirm_leaving = None;
        cx.notify();
    }

    pub(super) fn dismiss_update_confirm(&mut self, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.updates.confirm) {
            return;
        }
        self.updates.confirm_gen += 1;
        let n = self.updates.confirm_gen;
        self.updates.confirm_leaving = Some(n);
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(crate::motion::FAST + Duration::from_millis(20))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.updates.confirm_leaving == Some(n) {
                    this.updates.confirm_leaving = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn update_confirm_open(&self) -> bool {
        self.updates.confirm
    }

    /// Escape: the confirm goes at once, without its leave fade.
    pub(super) fn cut_update_confirm(&mut self) {
        self.updates.confirm = false;
        self.updates.confirm_leaving = None;
    }

    /// Relaunch: replace the bundle, kick a daemon this bundle runs, hand
    /// the reopen to a `/bin/sh` waiter, then quit (the quit hook flushes
    /// the engine).
    fn updates_install(&mut self, cx: &mut Context<Self>) {
        let (Some(ready), Some(Ok(gate))) = (self.updates.ready.clone(), self.updates.gate.clone()) else {
            return;
        };
        let Some(config) = keel_update::config() else {
            return;
        };
        self.dismiss_update_confirm(cx);
        self.updates.status = Status::Installing;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_spawn(async move {
                    let daemon = config
                        .plist_path
                        .as_deref()
                        .map(|plist| keel_update::Daemon { plist, label: config.plist_label });
                    let installed = keel_update::install(&ready.app(), &gate.app, daemon)?;
                    keel_update::spawn_waiter(
                        std::process::id(),
                        &installed.app,
                        installed.previous.as_deref(),
                        Some(&ready.dir),
                    )
                    .map_err(|e| UpdateError::new(msg::REDOWNLOAD, format!("waiter: {e}")))?;
                    Ok::<_, UpdateError>(installed)
                })
                .await;
            let _ = this.update(cx, |this, cx| match res {
                Ok(installed) => {
                    tracing::info!(
                        "update: installed {}, daemon kicked: {}",
                        installed.app.display(),
                        installed.kickstarted
                    );
                    cx.quit();
                }
                Err(err) => {
                    tracing::warn!("update: install failed: {err}");
                    this.updates.ready = None;
                    this.updates.status = Status::Failed { message: err.message };
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn dismiss_update_line(&mut self, cx: &mut Context<Self>) {
        if self.updates.line_dismissed {
            return;
        }
        self.updates.line_dismissed = true;
        self.updates.line_gen += 1;
        let n = self.updates.line_gen;
        self.updates.line_leaving = Some(n);
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(super::NOTICE_EXIT + Duration::from_millis(20))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.updates.line_leaving == Some(n) {
                    this.updates.line_leaving = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn update_sentence(&self) -> String {
        if let Some(Err(gate)) = &self.updates.gate {
            return gate.message().to_string();
        }
        match &self.updates.status {
            Status::Idle if self.state.ui_settings.auto_update == Some(false) => {
                "Automatic checks are off.".into()
            }
            Status::Idle => "Keel checks for updates once a day.".into(),
            Status::Checking => "Checking for updates…".into(),
            Status::UpToDate => format!("Keel {} is up to date.", keel_update::CURRENT_VERSION),
            Status::Available { version } => format!("Keel {version} is available."),
            Status::Downloading { version, fraction: Some(f) } => {
                format!("Downloading Keel {version}… {}%", (f * 100.).floor() as u32)
            }
            Status::Downloading { version, fraction: None } => format!("Downloading Keel {version}…"),
            Status::Ready { version } => format!("Keel {version} is ready. Relaunch to finish updating."),
            Status::Installing => "Installing the update…".into(),
            Status::Failed { message } => message.clone(),
        }
    }

    /// Settings home, after Appearance.
    pub(super) fn render_updates_section(&mut self, t: Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let failed = matches!(self.updates.status, Status::Failed { .. })
            || matches!(self.updates.gate, Some(Err(GateError::MustMove)));
        let hide_button = matches!(self.updates.gate, Some(Err(GateError::NotInstalled)) | None);
        let busy = self.updates_busy();
        let ready = matches!(self.updates.status, Status::Ready { .. }) && self.updates.ready.is_some();
        let fraction = match &self.updates.status {
            Status::Downloading { fraction, .. } => Some(fraction.unwrap_or(0.)),
            _ => None,
        };

        let button = (!hide_button).then(|| {
            if ready {
                text_button_owned("updates-relaunch".into(), "Relaunch to update", t)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.open_update_confirm(cx)))
                    .into_any_element()
            } else if busy {
                div()
                    .px_2p5()
                    .py_1()
                    .flex_none()
                    .rounded(RADIUS_MD)
                    .t_small()
                    .text_color(c(t.text_faint))
                    .child("Check now")
                    .into_any_element()
            } else {
                text_button_owned("updates-check".into(), "Check now", t)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.updates_check(true, cx)))
                    .into_any_element()
            }
        });
        let notes = self.updates.notes_url.clone().filter(|u| u.starts_with("https://")).map(|url| {
            text_button_owned("updates-notes".into(), "Release notes", t)
                .on_click(move |_: &ClickEvent, _, cx| cx.open_url(&url))
                .into_any_element()
        });

        let version_row = div()
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
                            .child(format!("Keel {}", keel_update::CURRENT_VERSION)),
                    )
                    .child(
                        div()
                            .t_small()
                            .text_color(c(if failed { t.danger } else { t.text_muted }))
                            .child(self.update_sentence()),
                    )
                    .children(fraction.map(|f| {
                        div()
                            .mt_1()
                            .h(px(2.))
                            .w_full()
                            .max_w(zpx(220.))
                            .rounded_full()
                            .bg(c(t.border))
                            .child(
                                div()
                                    .h_full()
                                    .w(gpui::relative(f.clamp(0., 1.)))
                                    .rounded_full()
                                    .bg(c(t.accent)),
                            )
                    })),
            )
            .child(div().flex().flex_row().items_center().gap_1().children(notes).children(button));

        let auto = self.state.ui_settings.auto_update != Some(false);
        let auto_row = setting_row(
            t,
            "Check automatically",
            Some("Once a day on launch. Downloads in the background and installs only when you relaunch."),
            toggle("updates-auto".into(), auto, self.recent_flip("updates-auto"), t)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    let on = this.state.ui_settings.auto_update == Some(false);
                    this.note_switch_flip("updates-auto");
                    this.state.ui_settings.auto_update = (!on).then_some(false);
                    this.state.save_settings();
                    cx.notify();
                }))
                .into_any_element(),
        );

        div()
            .flex()
            .flex_col()
            .child(section_header("Updates", "New versions of Keel, signed and verified before they install.", None, t))
            .child(settings_list(vec![version_row, auto_row], t))
    }

    /// Centered relaunch confirm, in the delete-chats modal's style.
    pub(super) fn render_update_confirm(&mut self, t: Tokens, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let (leaving, modal_gen) = match (self.updates.confirm, self.updates.confirm_leaving) {
            (true, _) => (false, self.updates.confirm_gen),
            (false, Some(n)) => (true, n),
            _ => return None,
        };
        let version = match &self.updates.ready {
            Some(r) => r.version.clone(),
            None => String::new(),
        };
        let mut body = format!("The running session stops. Keel opens again on {version}.");
        if self.turn_in_flight() {
            body.push_str(" An agent is still working, and its turn stops too.");
        }
        let reduced = self.reduced();
        let card = div()
            .id(("update-modal-card", modal_gen as usize))
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
            .child(
                div()
                    .t_title()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Relaunch to update Keel?"),
            )
            .child(div().t_body().text_color(c(t.text_muted)).child(body))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap_2()
                    .child(
                        super::sidebar::text_button("cancel-update-btn", "Cancel", t).on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.dismiss_update_confirm(cx)),
                        ),
                    )
                    .child(
                        super::sidebar::filled_button("confirm-update-btn", "Relaunch", t.accent, t)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.updates_install(cx))),
                    ),
            );
        let card = if leaving {
            crate::motion::dismiss(card, ("update-modal-out", modal_gen as usize), reduced)
        } else {
            crate::motion::appear(card, ("update-modal-in", modal_gen as usize), crate::motion::BASE, 14., reduced)
                .into_any_element()
        };
        Some(
            gpui::deferred(crate::motion::scrim(
                div()
                    .id(("update-modal", modal_gen as usize))
                    .absolute()
                    .inset_0()
                    .occlude()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.dismiss_update_confirm(cx)))
                    .child(card),
                (if leaving { "update-scrim-out" } else { "update-scrim-in" }, modal_gen as usize),
                0.7,
                leaving,
                reduced,
            ))
            .with_priority(3),
        )
    }

    /// The one-line "ready" note under the header while Settings is closed.
    /// Its action opens Settings, where Updates sits near the top. Closing
    /// it keeps the staged build; the next launch shows it again.
    pub(super) fn render_update_line(&mut self, t: Tokens, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let Status::Ready { version } = &self.updates.status else {
            return None;
        };
        let leaving = self.updates.line_dismissed;
        if self.settings_open
            || self.settings_leaving.is_some()
            || self.status_message.is_some()
            || (leaving && self.updates.line_leaving.is_none())
        {
            return None;
        }
        let generation = self.updates.line_gen;
        let accent = c(t.accent);
        let card = div()
            .id("update-line")
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
                    .bg(accent.opacity(0.14))
                    .text_color(accent)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Icon::Reload.el(zpx(12.))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .t_small()
                    .text_color(c(t.text))
                    .truncate()
                    .child(format!("Keel {version} is ready to install.")),
            )
            .child(
                text_button_owned("update-line-open".into(), "Open Settings", t).on_click(cx.listener(
                    |this, _: &ClickEvent, _, cx| {
                        this.open_settings(cx);
                        this.dotify.settings_scroll.set_offset(gpui::point(px(0.), px(0.)));
                    },
                )),
            )
            .child(
                div()
                    .id("update-line-close")
                    .size(zpx(22.))
                    .flex_none()
                    .rounded(RADIUS_SM)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(c(t.text_faint))
                    .cursor_pointer()
                    .hover(|s| s.text_color(c(t.text)))
                    .hover_fade(c(t.hover), None)
                    .child(Icon::Close.el(zpx(11.)))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.dismiss_update_line(cx))),
            );
        // The error notice's motion: drop in on a strong ease-out, lift out faster.
        let travel = if self.reduced() { 0. } else { 1. };
        let card: gpui::AnyElement = if leaving {
            card.with_animation(
                ElementId::Name(format!("update-line-out-{generation}").into()),
                gpui::Animation::new(super::NOTICE_EXIT).with_easing(crate::motion::ease_out),
                move |el, d| el.opacity(1. - d).relative().top(zpx(-6. * d * travel)),
            )
            .into_any_element()
        } else {
            card.with_animation(
                ElementId::Name(format!("update-line-in-{generation}").into()),
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
}

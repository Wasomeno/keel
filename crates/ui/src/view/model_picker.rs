//! The composer's model menu: a pinned search over a hairline, "Recent",
//! then "All models" by provider. Three tiers share one left edge and differ
//! by type: section labels (11 px faint), provider headers (12 px semibold,
//! trailing chevron), models (13 px, check on the left). Inside a provider,
//! models run by family, newest first, with "Show N older" last. Keyboard: Up/Down move the highlight (kept in view), Enter picks
//! (or folds), Escape closes. The list logic is in [`crate::model_menu`].
//!
//! Motion: the menu and a family's fold use the strong ease-out
//! (`cubic-bezier(0.23, 1, 0.32, 1)`) at 200 ms or less; frames run only
//! while something moves, and reduced motion cuts instead.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, ClickEvent, Context, ElementId, IntoElement, Keystroke, ScrollHandle, Window, div,
    point, prelude::*, px,
};
use keel_theme::Tokens;

use super::KeelWindow;
use super::dotify_ui::Tip;
use crate::composer::ModelOption;
use crate::icons::Icon;
use crate::model_menu;
use crate::motion::{ease_out_strong, unit};
use crate::style::{KeelStyled, RADIUS_SM, c, zpx};

/// Row heights by tier: a model (and the older-versions line), a provider
/// header, a section label. Then the list's tallest before it scrolls (the
/// search above it stays put; the whole menu stays near 360 px).
const ROW_H: f32 = 30.;
const PROV_H: f32 = 28.;
const LABEL_H: f32 = 24.;
const LIST_MAX: f32 = 308.;
/// The one left text edge every tier shares, and the right inset.
const PAD_X: f32 = 10.;
const PAD_R: f32 = 8.;
/// Air between provider groups, between families, above Recent's divider,
/// and at the list's top and bottom.
const GROUP_GAP: f32 = 4.;
const FAMILY_GAP: f32 = 4.;
const DIVIDER_GAP: f32 = 6.;
const LIST_PAD: f32 = 4.;
/// A family's older versions opening or folding.
const EXPAND: Duration = Duration::from_millis(180);
/// The menu arriving and leaving.
const MENU_IN: Duration = Duration::from_millis(160);
const MENU_OUT: Duration = Duration::from_millis(120);
/// The popover hugs the longest name between these widths.
const MENU_MIN_W: f32 = 280.;
const MENU_MAX_W: f32 = 380.;

/// What a keyboard-reachable line does.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Nav {
    Model(String),
    /// A provider (`p:…`) or its older versions (`o:…`).
    Fold(String),
}

fn provider_key(p: &str) -> String {
    format!("p:{p}")
}

fn older_key(p: &str) -> String {
    format!("o:{p}")
}

#[derive(Default)]
pub(super) struct ModelMenu {
    /// The highlighted line, an index into `nav`.
    hi: usize,
    nav: Vec<Nav>,
    /// Each line's top and height in the list, for keeping it in view.
    spans: Vec<(f32, f32)>,
    content_h: f32,
    query: String,
    /// Folds by key (see [`Nav::Fold`]): open, and when that last changed.
    expanded: HashMap<String, (bool, Option<Instant>)>,
    scroll: ScrollHandle,
    /// Bring the highlight into view: `true` centres it (on open).
    reveal: Option<bool>,
    /// Just opened: set the folds, highlight the current model.
    fresh: bool,
}

/// How open a fold is (0..1) at `now`.
fn fold_amount(state: Option<&(bool, Option<Instant>)>, now: Instant) -> f32 {
    match state {
        None => 0.,
        Some(&(open, None)) => f32::from(u8::from(open)),
        Some(&(open, Some(at))) => {
            let p = unit(ease_out_strong(unit(
                now.saturating_duration_since(at).as_secs_f32() / EXPAND.as_secs_f32(),
            )));
            if open { p } else { 1. - p }
        }
    }
}

/// The fold row's chevron: right (-90°) folded, down open. In radians, since
/// gpui's `percentage()` only takes 0..=1 and this turns backwards.
fn chevron_turn(amount: f32) -> gpui::Transformation {
    gpui::Transformation::rotate(gpui::radians(
        -std::f32::consts::FRAC_PI_2 * (1. - unit(amount)),
    ))
}

/// The older-versions chevron: down folded, up open.
fn older_turn(amount: f32) -> gpui::Transformation {
    gpui::Transformation::rotate(gpui::radians(std::f32::consts::PI * unit(amount)))
}

/// The popover's width for these names (a rough measure at the 13 px body).
fn menu_width(models: &[ModelOption]) -> f32 {
    let longest = models
        .iter()
        .map(|m| m.name.chars().count())
        .max()
        .unwrap_or(0) as f32;
    let note = if models.iter().any(|m| m.context_window.is_some()) {
        48.
    } else {
        0.
    };
    // left edge + gap + trailing check + right inset + panel padding
    (longest * 7.4 + PAD_X + 8. + 14. + PAD_R + 8. + note).clamp(MENU_MIN_W, MENU_MAX_W)
}

impl KeelWindow {
    /// The menu opened: a clean search, the current model highlighted and in view.
    pub(super) fn model_menu_opened(&mut self) {
        let scroll = std::mem::take(&mut self.model_menu.scroll);
        scroll.set_offset(point(px(0.), px(0.)));
        self.model_menu = ModelMenu {
            scroll,
            fresh: true,
            reveal: Some(true),
            ..ModelMenu::default()
        };
    }

    /// Repaint while a fold moves.
    pub(super) fn model_menu_animating(&self) -> bool {
        self.show_model_picker
            && self
                .model_menu
                .expanded
                .values()
                .any(|(_, at)| at.is_some_and(|at| at.elapsed() < EXPAND))
    }

    /// Keys while the menu is open, ahead of the search field's own
    /// bindings. True when used.
    pub(super) fn model_menu_key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.show_model_picker {
            return false;
        }
        let m = key.modifiers;
        if m.platform || m.control || m.alt || m.function {
            return false;
        }
        let on = self.model_menu.nav.get(self.model_menu.hi).cloned();
        match key.key.as_str() {
            "up" | "down" => {
                let n = self.model_menu.nav.len();
                if n > 0 {
                    let hi = self.model_menu.hi.min(n - 1);
                    self.model_menu.hi = if key.key == "up" {
                        hi.saturating_sub(1)
                    } else {
                        (hi + 1).min(n - 1)
                    };
                    self.model_menu.reveal = Some(false);
                    cx.notify();
                }
                true
            }
            "enter" => {
                match on {
                    Some(Nav::Model(id)) => self.model_menu_pick(id, window, cx),
                    Some(Nav::Fold(key)) => self.model_menu_toggle(key, cx),
                    None => {}
                }
                true
            }
            // On a fold row, Left folds and Right unfolds; elsewhere they
            // move the search field's cursor.
            "left" | "right" => {
                let Some(Nav::Fold(fold)) = on else {
                    return false;
                };
                let open = self
                    .model_menu
                    .expanded
                    .get(&fold)
                    .is_some_and(|(open, _)| *open);
                if open != (key.key == "right") {
                    self.model_menu_toggle(fold, cx);
                }
                true
            }
            "escape" => {
                self.menus_cut = true;
                self.set_picker(None, cx);
                self.composer.update(cx, |s, cx| s.focus(window, cx));
                true
            }
            _ => false,
        }
    }

    /// A model picked: remembered for this harness's "Recent", then chosen.
    fn model_menu_pick(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let harness = self.selected_harness.clone();
        if !harness.is_empty() {
            let recent = self
                .state
                .ui_settings
                .recent_models
                .entry(harness)
                .or_default();
            if model_menu::push_recent(recent, &id) {
                self.state.save_settings();
            }
        }
        self.select_model(id, cx);
        self.composer.update(cx, |s, cx| s.focus(window, cx));
    }

    /// Open or fold; a reversal mid-way starts from where it is. A
    /// provider's state is remembered for this harness.
    fn model_menu_toggle(&mut self, key: String, cx: &mut Context<Self>) {
        let now = Instant::now();
        let reduced = self.reduced();
        let was = self.model_menu.expanded.get(&key).copied();
        let open = !was.is_some_and(|(open, _)| open);
        let at = if reduced {
            None
        } else {
            let amount = fold_amount(was.as_ref(), now);
            let done = unit(if open { amount } else { 1. - amount });
            Some(now.checked_sub(EXPAND.mul_f32(done * 0.999)).unwrap_or(now))
        };
        self.model_menu.expanded.insert(key.clone(), (open, at));
        if key.starts_with("p:") && !self.selected_harness.is_empty() {
            let mut providers: Vec<String> = self
                .model_menu
                .expanded
                .iter()
                .filter(|(k, (open, _))| *open && k.starts_with("p:"))
                .map(|(k, _)| k[2..].to_string())
                .collect();
            providers.sort();
            self.state
                .ui_settings
                .open_model_providers
                .insert(self.selected_harness.clone(), providers);
            self.state.save_settings();
        }
        self.kick_motion(cx);
        cx.notify();
    }

    fn model_menu_hover(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix != usize::MAX && self.model_menu.hi != ix {
            self.model_menu.hi = ix;
            cx.notify();
        }
    }

    /// The search box and the list, for [`Self::render_picker_menu`]. Also
    /// returns the popover's width.
    pub(super) fn render_model_menu(
        &mut self,
        mut items: Vec<AnyElement>,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> (Vec<AnyElement>, f32) {
        let harness = self.selected_harness.clone();
        let models = if self.demo_model_loader {
            None
        } else {
            self.models.get(&harness).cloned()
        };
        let list = models
            .as_ref()
            .map(|l| l.models.clone())
            .unwrap_or_default();
        let width = menu_width(&list);
        let query = self.picker_filter.read(cx).value().trim().to_lowercase();
        if query != self.model_menu.query {
            self.model_menu.query = query.clone();
            self.model_menu.hi = 0;
            self.model_menu.reveal = Some(false);
            self.model_menu.scroll.set_offset(point(px(0.), px(0.)));
        }
        // A fixed header: the search, whose own hairline the list scrolls under.
        let mut search = vec![self.picker_search_box(Icon::Search, t)];
        search.append(&mut items);
        let mut items = search;

        if let Some(err) = self.models_error.get(&harness).cloned() {
            let h = harness.clone();
            items.push(
                super::menu_item("model-retry", t, true, false)
                    .text_color(c(t.danger))
                    .child(format!("{err}. Retry"))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.ensure_models(h.clone(), cx);
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        let clock = self.clock_epoch.elapsed().as_secs_f32() * 1000.;
        let reduced = self.reduced();
        match models {
            None if self.demo_model_loader || self.models_loading.contains(&harness) => {
                items.push(
                    div()
                        .flex()
                        .flex_col()
                        .children(super::composer_view::model_skeleton(t, clock, reduced))
                        .into_any_element(),
                );
                return (items, width);
            }
            None => return (items, width),
            Some(l) if l.models.is_empty() => {
                items.push(super::menu_label("No models reported", t).into_any_element());
                return (items, width);
            }
            Some(_) => {}
        }

        let now = Instant::now();
        let current = self.selected_model.clone();
        let current_ix = list.iter().position(|m| m.id == current);
        let mut lines: Vec<AnyElement> = Vec::new();
        let mut nav: Vec<Nav> = Vec::new();
        let mut spans: Vec<(f32, f32)> = Vec::new();
        // The list's own top padding, so nothing sits flush under the search.
        let z = |v: f32| f32::from(zpx(v));
        let mut y = z(LIST_PAD);
        let hi = self.model_menu.hi;
        // Every tier starts at the same left edge (PAD_X); hierarchy comes
        // from type, colour and spacing, not indents.
        let section_label = |text: &'static str| {
            div()
                .flex_none()
                .h(zpx(LABEL_H))
                .pl(zpx(PAD_X))
                .pr(zpx(PAD_R))
                .pb(zpx(4.))
                .flex()
                .items_end()
                .t_caption()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(c(t.text_faint))
                .child(text)
                .into_any_element()
        };
        let gap = |h: f32| div().flex_none().h(zpx(h)).into_any_element();
        // Provider name (12 px semibold, secondary) and its count (faint).
        let provider_inner = |name: String, count: usize| {
            div()
                .flex_none()
                .h(zpx(PROV_H))
                .pl(zpx(PAD_X))
                .pr(zpx(PAD_R))
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(6.))
                .rounded(RADIUS_SM)
                .t_small()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(c(t.text_muted))
                .child(div().flex_none().whitespace_nowrap().child(name))
                .child(
                    div()
                        .flex_none()
                        .font_weight(gpui::FontWeight::NORMAL)
                        .text_color(c(t.text_faint))
                        .child(count.to_string()),
                )
                .child(div().flex_1().min_w(zpx(8.)))
        };
        // A foldable provider header: the chevron trails at the right edge.
        let provider_head = |key: String,
                             name: String,
                             count: usize,
                             amount: f32,
                             ix: usize,
                             cx: &mut Context<Self>| {
            let k = key.clone();
            provider_inner(name, count)
                .id(ElementId::Name(format!("model-fold-{key}").into()))
                .cursor_pointer()
                .when(ix == hi, |d| d.bg(c(t.hover)))
                .child(
                    div()
                        .w(zpx(14.))
                        .flex_none()
                        .flex()
                        .justify_center()
                        .text_color(c(t.text_faint))
                        .child(
                            Icon::ChevronDown
                                .el(zpx(11.))
                                .with_transformation(chevron_turn(amount)),
                        ),
                )
                .on_mouse_move(cx.listener(move |this, _: &gpui::MouseMoveEvent, _, cx| {
                    this.model_menu_hover(ix, cx)
                }))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.model_menu_toggle(k.clone(), cx)
                }))
                .into_any_element()
        };
        // One model line: 13 px name, with the check on the left.
        // `key` keeps a model listed twice (Recent and its provider)
        // apart; `older` lines read one step quieter.
        let row = |key: &str, m: &ModelOption, ix: usize, older: bool, cx: &mut Context<Self>| {
            let (id, tip) = (m.id.clone(), m.id.clone());
            let note = m
                .context_window
                .map(|w| format!("{} ctx", crate::usage::compact(w)));
            let selected = m.id == current;
            div()
                .id(ElementId::Name(format!("model-{key}-{}", m.id).into()))
                .flex_none()
                .h(zpx(ROW_H))
                .pl(zpx(PAD_X))
                .pr(zpx(PAD_R))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .rounded(RADIUS_SM)
                .t_body()
                .text_color(c(if older { t.text_muted } else { t.text }))
                .cursor_pointer()
                .when(selected, |d| d.bg(c(t.selected)))
                .when(ix == hi && !selected, |d| d.bg(c(t.hover)))
                .child(
                    div()
                        .w(zpx(14.))
                        .flex_none()
                        .text_color(c(t.accent))
                        .when(selected, |d| d.child(Icon::Check.el(zpx(13.)))),
                )
                .child(div().flex_none().whitespace_nowrap().child(m.name.clone()))
                .child(div().flex_1().min_w(zpx(8.)))
                .when_some(note, |d, n| {
                    d.child(
                        div()
                            .flex_none()
                            .t_caption()
                            .text_color(c(t.text_faint))
                            .child(n),
                    )
                })
                .tooltip(move |_, cx| cx.new(|_| Tip::new(tip.clone(), t)).into())
                .tooltip_show_delay(crate::view::tooltip::delay())
                .on_mouse_move(cx.listener(move |this, _: &gpui::MouseMoveEvent, _, cx| {
                    this.model_menu_hover(ix, cx)
                }))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.model_menu_pick(id.clone(), window, cx)
                }))
                .into_any_element()
        };
        // The provider's last line: "Show N older" / "Hide older", on the
        // model rows' geometry, 12 px faint, with a small trailing chevron.
        let older_toggle =
            |key: String, n: usize, open: bool, amount: f32, ix: usize, cx: &mut Context<Self>| {
                let k = key.clone();
                div()
                    .id(ElementId::Name(format!("model-fold-{key}").into()))
                    .flex_none()
                    .h(zpx(ROW_H))
                    .pl(zpx(PAD_X))
                    .pr(zpx(PAD_R))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .rounded(RADIUS_SM)
                    .t_small()
                    .text_color(c(if ix == hi { t.text_muted } else { t.text_faint }))
                    .cursor_pointer()
                    .when(ix == hi, |d| d.bg(c(t.hover)))
                    .child(div().w(zpx(14.)).flex_none())
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(zpx(4.))
                            .child(
                                div()
                                    .flex_none()
                                    .whitespace_nowrap()
                                    .child(model_menu::older_label(n, open)),
                            )
                            .child(
                                div().flex_none().flex().items_center().child(
                                    Icon::ChevronDown
                                        .el(zpx(10.))
                                        .with_transformation(older_turn(amount)),
                                ),
                            ),
                    )
                    .on_mouse_move(cx.listener(move |this, _: &gpui::MouseMoveEvent, _, cx| {
                        this.model_menu_hover(ix, cx)
                    }))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.model_menu_toggle(k.clone(), cx)
                    }))
                    .into_any_element()
            };
        // Clips a fold's body to its share of `full` (unscaled px).
        let fold_body = |full: f32, amount: f32, children: Vec<AnyElement>| {
            div()
                .flex_none()
                .overflow_hidden()
                .when(amount < 1., |d| d.h(zpx(full * amount)).opacity(amount))
                .child(div().flex().flex_col().children(children))
                .into_any_element()
        };

        if query.is_empty() {
            let recent: Vec<usize> = self
                .state
                .ui_settings
                .recent_models
                .get(&harness)
                .into_iter()
                .flatten()
                .filter_map(|id| list.iter().position(|m| &m.id == id))
                .take(model_menu::RECENT_MAX)
                .collect();
            if !recent.is_empty() {
                lines.push(section_label("Recent"));
                y += z(LABEL_H);
                for i in recent {
                    let ix = nav.len();
                    lines.push(row("recent", &list[i], ix, false, cx));
                    nav.push(Nav::Model(list[i].id.clone()));
                    spans.push((y, z(ROW_H)));
                    y += z(ROW_H);
                }
                lines.push(
                    div()
                        .flex_none()
                        .pt(zpx(DIVIDER_GAP))
                        .child(div().h(zpx(1.)).bg(c(t.border)))
                        .into_any_element(),
                );
                y += z(DIVIDER_GAP + 1.);
            }
            lines.push(section_label("All models"));
            y += z(LABEL_H);
            let sections = model_menu::sections(&list);
            if self.model_menu.fresh {
                let saved = self
                    .state
                    .ui_settings
                    .open_model_providers
                    .get(&harness)
                    .map(Vec::as_slice);
                let open = model_menu::open_providers(saved, &sections, current_ix);
                for s in &sections {
                    let is_open = open.contains(&s.provider);
                    self.model_menu
                        .expanded
                        .insert(provider_key(&s.provider), (is_open, None));
                    let older_current = current_ix.is_some_and(|i| s.older.contains(&i));
                    self.model_menu
                        .expanded
                        .insert(older_key(&s.provider), (is_open && older_current, None));
                }
            }
            for (si, s) in sections.into_iter().enumerate() {
                if si > 0 {
                    lines.push(gap(GROUP_GAP));
                    y += z(GROUP_GAP);
                }
                let pkey = provider_key(&s.provider);
                let pstate = self.model_menu.expanded.get(&pkey).copied();
                let popen = pstate.is_some_and(|(open, _)| open);
                let pamount = fold_amount(pstate.as_ref(), now);
                let ix = nav.len();
                nav.push(Nav::Fold(pkey.clone()));
                spans.push((y, z(PROV_H)));
                y += z(PROV_H);
                lines.push(provider_head(
                    pkey,
                    s.provider.clone(),
                    s.count(),
                    pamount,
                    ix,
                    cx,
                ));
                if pamount <= 0. {
                    continue;
                }
                // The body; only an open provider's lines take the keyboard.
                let mut body = Vec::new();
                let mut full = 0f32;
                let line = |nav: &mut Vec<Nav>,
                            spans: &mut Vec<(f32, f32)>,
                            y: &mut f32,
                            n: Option<Nav>,
                            h: f32|
                 -> usize {
                    match n.filter(|_| popen) {
                        Some(n) => {
                            nav.push(n);
                            spans.push((*y, z(h)));
                            *y += z(h);
                            nav.len() - 1
                        }
                        None => {
                            if popen {
                                *y += z(h);
                            }
                            usize::MAX
                        }
                    }
                };
                // Family order, newest first; a little air where the family changes.
                for (i, family_break) in s.listed() {
                    if family_break {
                        body.push(gap(FAMILY_GAP));
                        line(&mut nav, &mut spans, &mut y, None, FAMILY_GAP);
                        full += FAMILY_GAP;
                    }
                    let ix = line(
                        &mut nav,
                        &mut spans,
                        &mut y,
                        Some(Nav::Model(list[i].id.clone())),
                        ROW_H,
                    );
                    body.push(row("list", &list[i], ix, false, cx));
                    full += ROW_H;
                }
                if !s.older.is_empty() {
                    let okey = older_key(&s.provider);
                    let ostate = self.model_menu.expanded.get(&okey).copied();
                    let oopen = ostate.is_some_and(|(open, _)| open);
                    let oamount = fold_amount(ostate.as_ref(), now);
                    if oamount > 0. {
                        let mut older = Vec::new();
                        for &i in &s.older {
                            let ix = if oopen {
                                line(
                                    &mut nav,
                                    &mut spans,
                                    &mut y,
                                    Some(Nav::Model(list[i].id.clone())),
                                    ROW_H,
                                )
                            } else {
                                usize::MAX
                            };
                            older.push(row("older", &list[i], ix, true, cx));
                        }
                        let ofull = s.older.len() as f32 * ROW_H;
                        body.push(fold_body(ofull, oamount, older));
                        full += ofull * oamount;
                    }
                    let ix = line(
                        &mut nav,
                        &mut spans,
                        &mut y,
                        Some(Nav::Fold(okey.clone())),
                        ROW_H,
                    );
                    body.push(older_toggle(okey, s.older.len(), oopen, oamount, ix, cx));
                    full += ROW_H;
                }
                lines.push(fold_body(full, pamount, body));
            }
        } else {
            let hits = model_menu::search_by_provider(&list, &query);
            if hits.is_empty() {
                lines.push(
                    div()
                        .h(zpx(ROW_H))
                        .pl(zpx(PAD_X))
                        .flex()
                        .items_center()
                        .t_small()
                        .text_color(c(t.text_muted))
                        .child("No models match")
                        .into_any_element(),
                );
            }
            // Flat, folds ignored, still under their providers.
            for (pi, (provider, hits)) in hits.into_iter().enumerate() {
                if pi > 0 {
                    lines.push(gap(GROUP_GAP));
                    y += z(GROUP_GAP);
                }
                lines.push(provider_inner(provider, hits.len()).into_any_element());
                y += z(PROV_H);
                for i in hits {
                    let ix = nav.len();
                    lines.push(row("hit", &list[i], ix, false, cx));
                    nav.push(Nav::Model(list[i].id.clone()));
                    spans.push((y, z(ROW_H)));
                    y += z(ROW_H);
                }
            }
        }
        y += z(LIST_PAD);

        let menu = &mut self.model_menu;
        if std::mem::take(&mut menu.fresh) {
            menu.hi = nav
                .iter()
                .position(|n| *n == Nav::Model(current.clone()))
                .unwrap_or(0);
            // Re-render so the highlight shows on the current model.
            cx.notify();
        }
        menu.hi = menu.hi.min(nav.len().saturating_sub(1));
        menu.nav = nav;
        menu.spans = spans;
        menu.content_h = y;
        if let Some(centre) = menu.reveal {
            let view = f32::from(menu.scroll.bounds().size.height);
            match menu.spans.get(menu.hi).copied() {
                Some((top, h)) if view > 0. => {
                    let at = -f32::from(menu.scroll.offset().y);
                    let max = (menu.content_h - view).max(0.);
                    let next = if centre {
                        top + h / 2. - view / 2.
                    } else if top < at {
                        top
                    } else if top + h > at + view {
                        top + h - view
                    } else {
                        at
                    };
                    menu.scroll
                        .set_offset(point(px(0.), px(-next.clamp(0., max))));
                    menu.reveal = None;
                }
                // Not measured yet (the first frame): next frame.
                Some(_) => cx.notify(),
                None => menu.reveal = None,
            }
        }
        // A list arriving while the menu was open fades in.
        let fade = self
            .models_ready_at
            .get(&harness)
            .map(|at| at.elapsed().as_secs_f32() * 1000. / super::MODEL_FADE_MS as f32)
            .filter(|p| !reduced && *p < 1.)
            .map_or(1., |p| unit(ease_out_strong(unit(p))));
        items.push(
            div()
                .id("model-list")
                .track_scroll(&self.model_menu.scroll)
                .max_h(zpx(LIST_MAX))
                .py(zpx(LIST_PAD))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .opacity(fade)
                .children(lines)
                .into_any_element(),
        );
        (items, width)
    }
}

/// The model menu's surface: the shared menu panel, with its own entrance
/// and exit on the strong ease-out.
#[allow(clippy::too_many_arguments)]
pub(super) fn model_menu_shell(
    id: &'static str,
    t: Tokens,
    width: f32,
    anchor: gpui::Anchor,
    at: Option<gpui::Point<gpui::Pixels>>,
    up: bool,
    leaving: Option<u64>,
    reduced: bool,
    on_dismiss: impl Fn(&gpui::MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
    items: Vec<AnyElement>,
) -> AnyElement {
    use gpui::AnimationExt as _;
    let panel = super::menu_panel(id, t, width, up, items)
        .overflow_hidden()
        .when(leaving.is_none(), |d| d.on_mouse_down_out(on_dismiss));
    let rise = if up { 4. } else { -4. };
    let curve = |p: f32| ease_out_strong(p);
    let el = match leaving {
        Some(_) if reduced => gpui::Empty.into_any_element(),
        _ if reduced => panel.into_any_element(),
        None => panel
            .with_animation(
                ElementId::Name(format!("{id}-in").into()),
                gpui::Animation::new(MENU_IN).with_easing(curve),
                move |el, d| el.opacity(d).relative().top(zpx(rise * (1. - d))),
            )
            .into_any_element(),
        Some(n) => panel
            .with_animation(
                ElementId::Name(format!("{id}-out-{n}").into()),
                gpui::Animation::new(MENU_OUT).with_easing(curve),
                move |el, d| el.opacity(1. - d).relative().top(zpx(rise * d)),
            )
            .into_any_element(),
    };
    super::anchored_menu_positioned(anchor, at, el)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_amount_stays_a_fraction() {
        let now = Instant::now();
        assert_eq!(fold_amount(None, now), 0.);
        assert_eq!(fold_amount(Some(&(true, None)), now), 1.);
        assert_eq!(fold_amount(Some(&(false, None)), now), 0.);
        // A start in the future, mid-way and long done, both directions.
        let later = now + Duration::from_secs(1);
        for at in [later, now, now - EXPAND / 2, now - EXPAND * 10] {
            for open in [true, false] {
                let a = fold_amount(Some(&(open, Some(at))), now);
                assert!((0. ..=1.).contains(&a), "{a}");
            }
        }
    }

    #[test]
    fn chevron_turn_takes_any_amount() {
        // The old `percentage(-0.25 * (1 - amount))` panicked for every folded row.
        for a in [0., 0.5, 1., -3., 7., f32::NAN] {
            let _ = chevron_turn(a);
        }
    }

    #[test]
    fn older_turn_takes_any_amount() {
        for a in [0., 0.5, 1., -3., 7., f32::NAN] {
            let _ = older_turn(a);
        }
    }

    #[test]
    fn fold_keys_are_distinct_per_provider() {
        assert_ne!(provider_key("OpenAI"), older_key("OpenAI"));
        assert!(provider_key("xAI").starts_with("p:"));
    }
}

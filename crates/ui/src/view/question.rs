//! The approval / question card. It takes the composer's slot while the
//! agent waits, enters with a short staggered rise, and on an answer folds
//! into a one-line receipt before the composer comes back. The transcript
//! keeps the receipt (the engine writes it as a `keel:receipt` tool part).
//!
//! Motion is computed from timestamps each frame (no `with_animation`
//! wrappers whose ids change under animated children); frames are requested
//! only while something moves. Only the card itself re-lays out per frame.

use crate::hover_fade::HoverFadeExt;
use std::time::Instant;

use gpui::{
    AnimationExt, AnyElement, BoxShadow, ClickEvent, Context, ElementId, FontWeight, Hsla,
    IntoElement, Window, canvas, div, point, prelude::*, px,
};
use keel_proto::{QuestionKind, Receipt, UserInputQuestion};
use keel_theme::Tokens;

use super::KeelWindow;
use crate::approval::{
    self, ButtonStyle, CardButton, Exit, ExitFrame, Glyph, Look, Mark, ReceiptLine,
};
use crate::composer::QuestionPanelState;
use crate::icons::Icon;
use crate::style::{KeelStyled, MONO, c, zpx};

/// A card on its way out of the composer slot.
#[derive(Clone)]
pub(super) struct QuestionExit {
    pub chat: String,
    pub panel: QuestionPanelState,
    /// The chosen button (permission) or row (question).
    pub chosen: Option<usize>,
    pub receipt: Receipt,
    pub started: Instant,
    pub exit: Exit,
    /// The card's height when it started leaving (window px).
    pub height: f32,
}

/// How a row is being drawn: live (entering, then interactive) or leaving.
#[derive(Clone, Copy)]
enum Mode {
    Live {
        ms: f32,
    },
    Exit {
        frame: ExitFrame,
        chosen: Option<usize>,
    },
}

impl Mode {
    fn live(self) -> bool {
        matches!(self, Mode::Live { .. })
    }

    /// The look of a piece entering after `delay` ms (live) or leaving.
    fn part(self, delay: f32, reduced: bool) -> Look {
        match self {
            Mode::Live { ms } => approval::part_enter(ms, delay, reduced),
            Mode::Exit { frame, .. } => Look {
                opacity: frame.others,
                dy: frame.dy,
            },
        }
    }
}

fn place(el: impl IntoElement, look: Look) -> gpui::Div {
    div()
        .relative()
        .top(zpx(look.dy))
        .opacity(look.opacity)
        .child(el)
}

/// "White x%" in the dark theme; the text color at x% in either.
fn ink(t: Tokens, alpha: f32) -> Hsla {
    c(t.text).opacity(alpha)
}

fn card_shadow(t: Tokens, alpha: f32) -> Vec<BoxShadow> {
    vec![
        BoxShadow {
            color: c(t.shadow).opacity(alpha),
            offset: point(px(0.), zpx(8.)),
            blur_radius: zpx(24.),
            spread_radius: px(0.),
            inset: false,
        },
        // Top inner highlight.
        BoxShadow {
            color: ink(t, 0.06 * alpha),
            offset: point(px(0.), px(1.)),
            blur_radius: px(0.),
            spread_radius: px(0.),
            inset: true,
        },
    ]
}

fn glyph_icon(g: Glyph) -> Icon {
    match g {
        Glyph::Browser | Glyph::Web => Icon::Globe,
        Glyph::Command | Glyph::Tool => Icon::Tool,
        Glyph::File => Icon::FileText,
        Glyph::Image => Icon::Image,
    }
}

fn key_chip(label: impl IntoElement, bg: Hsla, fg: Hsla) -> gpui::Div {
    div()
        .flex_none()
        .size(zpx(18.))
        .ml(zpx(6.))
        .rounded(zpx(4.))
        .flex()
        .items_center()
        .justify_center()
        .bg(bg)
        .font_family(MONO)
        .text_size(zpx(11.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(fg)
        .child(label)
}

/// Focus ring: 1.5 px accent plus a soft 3 px halo, drawn as shadows so it
/// never shifts layout.
fn focus_ring(t: Tokens) -> Vec<BoxShadow> {
    vec![
        BoxShadow {
            color: c(t.accent).opacity(0.8),
            offset: point(px(0.), px(0.)),
            blur_radius: px(0.),
            spread_radius: zpx(1.5),
            inset: false,
        },
        BoxShadow {
            color: c(t.accent).opacity(0.22),
            offset: point(px(0.), px(0.)),
            blur_radius: px(0.),
            spread_radius: zpx(4.5),
            inset: false,
        },
    ]
}

/// One receipt line, styled like a tool row.
pub(crate) fn receipt_content(line: &ReceiptLine, t: Tokens, alpha: f32) -> gpui::Div {
    let (mark, color) = match line.mark {
        Mark::Check => (Some("✓"), c(t.success).opacity(0.8)),
        Mark::Cross => (Some("✕"), c(t.danger).opacity(0.7)),
        Mark::Stop => (Some("■"), ink(t, 0.45)),
        Mark::None => (None, ink(t, 0.45)),
    };
    let quiet = matches!(line.mark, Mark::Stop | Mark::None);
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(zpx(6.))
        .min_w_0()
        .text_size(zpx(12.5))
        .line_height(zpx(18.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(ink(t, if quiet { 0.45 } else { 0.55 } * alpha))
        .children(mark.map(|m| div().flex_none().text_color(color).child(m)))
        .child(div().flex_none().child(line.verb.clone()))
        .children(
            line.title
                .clone()
                .map(|title| div().flex_none().child(format!("· {title}"))),
        )
        .children(line.arg.clone().map(|arg| {
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .font_family(MONO)
                .font_weight(FontWeight::NORMAL)
                .text_color(ink(t, 0.4 * alpha))
                .child(format!("· {arg}"))
        }))
}

/// Read-only arguments: keys mono 11.5 at 40%, values mono 12.5 at 85%,
/// commands led by a faint `$`.
pub(crate) fn args_block(args: &[(String, String)], t: Tokens, clamp: Option<usize>) -> gpui::Div {
    let ordered = approval::ordered_args(args);
    div()
        .w_full()
        .rounded(zpx(8.))
        .px(zpx(12.))
        .py(zpx(10.))
        .bg(c(t.code).opacity(0.6))
        .border_1()
        .border_color(ink(t, 0.05))
        .flex()
        .flex_col()
        .gap(zpx(2.))
        .when_some(clamp, |d, lines| {
            d.max_h(zpx(20. * lines as f32 + 20.)).overflow_hidden()
        })
        .children(ordered.into_iter().map(|(key, value)| {
            let command = approval::is_command_key(&key);
            div()
                .flex()
                .flex_row()
                .items_start()
                .gap(zpx(12.))
                .font_family(MONO)
                .child(
                    div()
                        .flex_none()
                        .min_w(zpx(56.))
                        .text_size(zpx(11.5))
                        .line_height(zpx(20.))
                        .text_color(ink(t, 0.4))
                        .child(key),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(zpx(12.5))
                        .line_height(zpx(20.))
                        .text_color(ink(t, 0.85))
                        .whitespace_normal()
                        .when_some(clamp, |d, lines| d.line_clamp(lines))
                        .child(if command {
                            gpui::StyledText::new(format!("$ {value}"))
                                .with_highlights([(
                                    0..1,
                                    gpui::HighlightStyle {
                                        color: Some(ink(t, 0.35)),
                                        ..Default::default()
                                    },
                                )])
                                .into_any_element()
                        } else {
                            value.into_any_element()
                        }),
                )
        }))
}

impl KeelWindow {
    /// Start the card's exit: it holds the chosen button's check, folds into
    /// a receipt, then gives the slot back to the composer.
    pub(super) fn begin_question_exit(
        &mut self,
        chat: String,
        panel: QuestionPanelState,
        chosen: Option<usize>,
        receipt: Receipt,
        canceled: bool,
        cx: &mut Context<Self>,
    ) {
        let exit = Exit {
            canceled,
            reduced: self.reduced(),
        };
        let started = Instant::now();
        let height = self
            .card_height
            .get()
            .max(f32::from(zpx(approval::RECEIPT_H)));
        self.question_exit = Some(QuestionExit {
            chat,
            panel,
            chosen,
            receipt,
            started,
            exit,
            height,
        });
        let wait = exit.duration() + std::time::Duration::from_millis(16);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |this, cx| {
                if !this.demo_hold_exit
                    && this
                        .question_exit
                        .as_ref()
                        .is_some_and(|e| e.started == started)
                {
                    this.question_exit = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// The current chat's card is still leaving.
    pub(super) fn question_exit_showing(&self) -> bool {
        self.question_exit.as_ref().is_some_and(|e| {
            Some(&e.chat) == self.current_chat.as_ref()
                && (self.demo_hold_exit || e.started.elapsed() < e.exit.duration())
        })
    }

    pub(super) fn render_question_panel(
        &mut self,
        t: Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let panel = self
            .current_chat
            .as_ref()
            .and_then(|c| self.question_panels.get(c))
            .cloned();
        let card = match panel {
            Some(panel) => self.render_live_card(panel, t, window, cx),
            None if self.question_exit_showing() => self.render_exit_card(t, window, cx),
            None => self.render_orphan_card(t, cx),
        };
        div()
            .flex_none()
            .px_4()
            .pb_4()
            .pt_1()
            .flex()
            .justify_center()
            .child(card)
    }

    fn card_shell(&self, t: Tokens, id: impl Into<ElementId>) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .relative()
            .w_full()
            .max_w(self.column_max())
            .rounded(zpx(12.))
            .bg(ink(t, 0.04))
            .border_1()
            .border_color(ink(t, 0.09))
            .shadow(card_shadow(t, 1.))
    }

    fn card_padding(el: gpui::Div) -> gpui::Div {
        el.pt(zpx(14.))
            .px(zpx(16.))
            .pb(zpx(12.))
            .flex()
            .flex_col()
            .gap(zpx(10.))
    }

    /// Waiting with no question this window saw (it was asked earlier).
    fn render_orphan_card(&mut self, t: Tokens, cx: &mut Context<Self>) -> AnyElement {
        self.card_shell(t, "question-orphan")
            .child(
                Self::card_padding(div())
                    .child(self.status_row(t, false, None, Mode::Live { ms: 1e6 }, 0))
                    .child(
                        div()
                            .text_size(zpx(13.))
                            .text_color(ink(t, 0.6))
                            .child("This chat is waiting for input, but the question was asked before this window saw it. Stop the run to continue."),
                    )
                    .child(div().flex().flex_row().child(div().flex_1()).child(self.stop_button(t, 0, true, cx))),
            )
            .into_any_element()
    }

    fn status_row(
        &self,
        t: Tokens,
        question: bool,
        tag: Option<String>,
        mode: Mode,
        epoch: u64,
    ) -> gpui::Div {
        let color = if question { t.accent } else { t.warning };
        let dot = div().flex_none().size(zpx(6.)).rounded_full().bg(c(color));
        let reduced = self.reduced();
        let dot = if mode.live() && !reduced {
            dot.with_animation(
                ElementId::Name(format!("approval-dot-{epoch}").into()),
                gpui::Animation::new(std::time::Duration::from_millis(1800))
                    .repeat()
                    .with_easing(gpui::pulsating_between(0.45, 1.0)),
                |el, delta| el.opacity(delta),
            )
            .into_any_element()
        } else {
            dot.into_any_element()
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(8.))
            .child(dot)
            .child(
                div()
                    .text_size(zpx(12.))
                    .line_height(zpx(16.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(c(color).opacity(0.9))
                    .child(if question {
                        "Question"
                    } else {
                        "Needs approval"
                    }),
            )
            .child(div().flex_1())
            .children(tag.map(|tag| {
                div()
                    .flex_none()
                    .font_family(MONO)
                    .text_size(zpx(11.))
                    .text_color(ink(t, 0.4))
                    .child(tag)
            }))
    }

    fn title_row(t: Tokens, icon: Option<Icon>, title: String, max_lines: usize) -> gpui::Div {
        let tile = div()
            .flex_none()
            .size(zpx(24.))
            .rounded(zpx(6.))
            .bg(ink(t, 0.06))
            .flex()
            .items_center()
            .justify_center()
            .text_color(ink(t, 0.75))
            .child(match icon {
                Some(icon) => icon.el(zpx(14.)).into_any_element(),
                None => div()
                    .text_size(zpx(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("?")
                    .into_any_element(),
            });
        div()
            .flex()
            .flex_row()
            .items_start()
            .gap(zpx(10.))
            .child(tile)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .pt(zpx(2.))
                    .text_size(zpx(15.))
                    .line_height(zpx(20.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(ink(t, 0.94))
                    .line_clamp(max_lines)
                    .child(title),
            )
    }

    fn stop_button(
        &self,
        t: Tokens,
        epoch: u64,
        live: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(ElementId::Name(format!("approval-stop-{epoch}").into()))
            .flex_none()
            .h(zpx(28.))
            .px(zpx(8.))
            .rounded(zpx(6.))
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(6.))
            .text_size(zpx(12.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(ink(t, 0.4))
            .child(
                div()
                    .size(zpx(10.))
                    .rounded(zpx(2.))
                    .bg(gpui::transparent_black())
                    .border_1()
                    .border_color(ink(t, 0.4)),
            )
            .child("Stop run")
            .child(
                div()
                    .font_family(MONO)
                    .text_size(zpx(11.))
                    .text_color(ink(t, 0.25))
                    .child("⌘."),
            )
            .when(live, |d| {
                d.cursor_pointer()
                    .hover(|s| s.text_color(c(t.danger)))
                    .hover_fade(c(t.danger).opacity(0.08), None)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.handle_interrupt(cx)))
            })
    }

    fn render_live_card(
        &mut self,
        panel: QuestionPanelState,
        t: Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let reduced = self.reduced();
        let ms = panel.shown_at.elapsed().as_secs_f32() * 1000.;
        let settle = if reduced {
            approval::REDUCED_MS + 20.
        } else {
            approval::ENTER_MS + 120.
        };
        if ms < settle {
            window.request_animation_frame();
        }
        let enter = approval::card_enter(ms, reduced);
        let height = self.card_height.clone();
        let measure = canvas(
            move |bounds, _, _| height.set(f32::from(bounds.size.height)),
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        let mode = Mode::Live { ms };
        let rows = self.card_rows(&panel, t, mode, cx);
        self.card_shell(
            t,
            ElementId::Name(format!("approval-card-{}", panel.epoch).into()),
        )
        .top(zpx(enter.dy))
        .opacity(enter.opacity)
        .child(measure)
        .child(Self::card_padding(div()).children(rows))
        .into_any_element()
    }

    fn render_exit_card(
        &mut self,
        t: Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(exit) = self.question_exit.clone() else {
            return div().into_any_element();
        };
        let ms = exit.started.elapsed().as_secs_f32() * 1000.;
        if ms < exit.exit.duration().as_secs_f32() * 1000. + 32. {
            window.request_animation_frame();
        }
        let f = approval::exit_frame(ms, exit.exit);
        let small = f32::from(zpx(approval::RECEIPT_H));
        let h = exit.height + (small - exit.height) * f.collapse;
        let radius = 12. + (6. - 12.) * f.collapse;
        let rows = self.card_rows(
            &exit.panel,
            t,
            Mode::Exit {
                frame: f,
                chosen: exit.chosen,
            },
            cx,
        );
        let line = approval::receipt_line(&exit.receipt);
        div()
            .id(ElementId::Name(
                format!("approval-exit-{}", exit.panel.epoch).into(),
            ))
            .relative()
            .w_full()
            .max_w(self.column_max())
            .h(px(h))
            .overflow_hidden()
            .rounded(zpx(radius))
            .bg(ink(t, 0.04 * f.chrome))
            .border_1()
            .border_color(ink(t, 0.09 * f.chrome))
            .shadow(card_shadow(t, f.chrome))
            .child(Self::card_padding(div()).children(rows))
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(zpx(f.receipt_dy))
                    .h(zpx(approval::RECEIPT_H - 2.))
                    .px(zpx(10.))
                    .flex()
                    .items_center()
                    .opacity(f.receipt)
                    .child(receipt_content(&line, t, 1.)),
            )
            .into_any_element()
    }

    /// The card's rows, live or leaving.
    fn card_rows(
        &mut self,
        panel: &QuestionPanelState,
        t: Tokens,
        mode: Mode,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(question) = panel.current_question().cloned() else {
            return Vec::new();
        };
        if question.is_permission() {
            self.permission_rows(panel, &question, t, mode, cx)
        } else {
            self.question_rows(panel, &question, t, mode, cx)
        }
    }

    fn permission_rows(
        &mut self,
        panel: &QuestionPanelState,
        q: &UserInputQuestion,
        t: Tokens,
        mode: Mode,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let reduced = self.reduced();
        let epoch = panel.epoch;
        let live = mode.live();
        let tool = approval::tool_of(q);
        let title = approval::humanize_title(&tool.name);
        let head = mode.part(0., reduced);
        let mut rows: Vec<AnyElement> = vec![
            place(
                div()
                    .flex()
                    .flex_col()
                    .gap(zpx(10.))
                    .child(self.status_row(t, false, tool.source.clone(), mode, epoch))
                    .child(Self::title_row(
                        t,
                        Some(glyph_icon(approval::glyph(&tool))),
                        title,
                        2,
                    )),
                head,
            )
            .into_any_element(),
        ];
        if let Some(description) = tool.description.clone().filter(|d| !d.trim().is_empty()) {
            rows.push(
                place(
                    div()
                        .text_size(zpx(13.))
                        .line_height(zpx(18.))
                        .text_color(ink(t, 0.6))
                        .line_clamp(3)
                        .child(description),
                    head,
                )
                .into_any_element(),
            );
        }
        if !tool.args.is_empty() {
            let long = approval::arg_lines(&tool.args, 72) > approval::ARG_LINES;
            let clamp = (long && !panel.show_all).then_some(approval::ARG_LINES);
            let block = div()
                .flex()
                .flex_col()
                .gap(zpx(4.))
                .child(args_block(&tool.args, t, clamp))
                .when(long && live, |d| {
                    d.child(
                        div()
                            .id(ElementId::Name(format!("approval-showall-{epoch}").into()))
                            .text_size(zpx(12.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(ink(t, 0.55))
                            .cursor_pointer()
                            .hover(|s| s.text_color(ink(t, 0.8)))
                            .child(if panel.show_all {
                                "Show less"
                            } else {
                                "Show all"
                            })
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.with_panel(|p| p.show_all = !p.show_all);
                                cx.notify();
                            })),
                    )
                });
            rows.push(place(block, mode.part(40., reduced)).into_any_element());
        }
        let buttons = approval::permission_buttons(q);
        let mut actions = div().flex().flex_row().items_center().gap(zpx(8.));
        for (ix, button) in buttons.iter().enumerate() {
            let look = match mode {
                Mode::Live { ms } => {
                    approval::part_enter(ms, 40. + 20. * ix.min(4) as f32, reduced)
                }
                Mode::Exit { frame, chosen } if chosen == Some(ix) => Look {
                    opacity: frame.chosen,
                    dy: frame.dy,
                },
                Mode::Exit { frame, .. } => Look {
                    opacity: frame.others,
                    dy: frame.dy,
                },
            };
            let chosen =
                matches!(mode, Mode::Exit { frame, chosen } if chosen == Some(ix) && frame.check);
            let focused = live && panel.keyboard && panel.focus == ix;
            actions = actions.child(place(
                self.action_button(button, ix, epoch, t, live, chosen, focused, cx),
                look,
            ));
        }
        let stop_look = mode.part(40. + 20. * buttons.len().min(4) as f32, reduced);
        actions = actions
            .child(div().flex_1())
            .child(place(self.stop_button(t, epoch, live, cx), stop_look));
        rows.push(div().pt(zpx(2.)).child(actions).into_any_element());
        rows
    }

    #[allow(clippy::too_many_arguments)]
    fn action_button(
        &self,
        button: &CardButton,
        ix: usize,
        epoch: u64,
        t: Tokens,
        live: bool,
        chosen: bool,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let (bg, border, fg, chip_bg, chip_fg) = match button.style {
            ButtonStyle::Primary => (
                ink(t, 0.92),
                None,
                c(t.window).opacity(0.9),
                c(t.window).opacity(0.12),
                c(t.window).opacity(0.6),
            ),
            ButtonStyle::Secondary => (
                ink(t, 0.07),
                Some(ink(t, 0.10)),
                ink(t, 0.85),
                ink(t, 0.08),
                ink(t, 0.5),
            ),
            ButtonStyle::Ghost => (
                gpui::transparent_black(),
                None,
                ink(t, 0.65),
                ink(t, 0.06),
                ink(t, 0.45),
            ),
        };
        let danger = c(t.danger);
        let style = button.style;
        let chip: AnyElement = if chosen {
            key_chip(Icon::Check.el(zpx(12.)), chip_bg, chip_fg).into_any_element()
        } else if let Some(key) = button.key {
            key_chip(key.to_string(), chip_bg, chip_fg).into_any_element()
        } else {
            div().into_any_element()
        };
        let pick = button.option;
        div()
            .id(ElementId::Name(format!("approval-btn-{epoch}-{ix}").into()))
            .flex_none()
            .h(zpx(32.))
            .pl(zpx(12.))
            .pr(zpx(if button.key.is_some() || chosen {
                8.
            } else {
                12.
            }))
            .rounded(zpx(8.))
            .flex()
            .flex_row()
            .items_center()
            .bg(bg)
            .when_some(border, |d, b| d.border_1().border_color(b))
            .text_size(zpx(13.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(fg)
            .when(focused, |d| d.shadow(focus_ring(t)))
            .when(chosen && style == ButtonStyle::Ghost, |d| {
                d.bg(danger.opacity(0.12)).text_color(danger.opacity(0.95))
            })
            .child(button.label.clone())
            .child(chip)
            .when(live, |d| {
                d.cursor_pointer()
                    .hover(move |s| match style {
                        ButtonStyle::Primary => s.bg(ink(t, 1.0)),
                        ButtonStyle::Secondary => s.bg(ink(t, 0.11)),
                        ButtonStyle::Ghost => {
                            s.bg(danger.opacity(0.12)).text_color(danger.opacity(0.95))
                        }
                    })
                    // No scale transform on divs in gpui: pressing darkens instead.
                    .active(move |s| match style {
                        ButtonStyle::Primary => s.bg(ink(t, 0.8)),
                        ButtonStyle::Secondary => s.bg(ink(t, 0.14)),
                        ButtonStyle::Ghost => s.bg(danger.opacity(0.18)),
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.answer_option(pick, Some(ix), cx)
                    }))
            })
    }

    fn question_rows(
        &mut self,
        panel: &QuestionPanelState,
        q: &UserInputQuestion,
        t: Tokens,
        mode: Mode,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let reduced = self.reduced();
        let epoch = panel.epoch;
        let live = mode.live();
        let (title, context) = match q.prompt.trim().split_once('\n') {
            Some((first, rest)) => (
                first.trim().to_string(),
                Some(rest.trim().to_string()).filter(|r| !r.is_empty()),
            ),
            None => (q.prompt.trim().to_string(), None),
        };
        let total = panel.questions.len();
        let page = (total > 1).then(|| format!("{} of {total}", panel.current_page + 1));
        let head = mode.part(0., reduced);
        let mut rows: Vec<AnyElement> = vec![
            place(
                div()
                    .flex()
                    .flex_col()
                    .gap(zpx(10.))
                    .child(self.status_row(t, true, page, mode, epoch))
                    .child(Self::title_row(t, None, title, 4)),
                head,
            )
            .into_any_element(),
        ];
        if let Some(context) = context {
            rows.push(
                place(
                    div()
                        .text_size(zpx(13.))
                        .line_height(zpx(18.))
                        .text_color(ink(t, 0.6))
                        .line_clamp(6)
                        .child(context),
                    head,
                )
                .into_any_element(),
            );
        }
        let text_only =
            matches!(q.kind, QuestionKind::Input | QuestionKind::Editor) && q.options.is_empty();
        let mut list = div().flex().flex_col().gap(zpx(6.));
        for (ix, option) in q.options.iter().enumerate() {
            let look = match mode {
                Mode::Live { ms } => {
                    approval::part_enter(ms, 20. + 20. * ix.min(6) as f32, reduced)
                }
                Mode::Exit { frame, chosen } if chosen == Some(ix) => Look {
                    opacity: frame.chosen,
                    dy: frame.dy,
                },
                Mode::Exit { frame, .. } => Look {
                    opacity: frame.others,
                    dy: frame.dy,
                },
            };
            let recommended = option.label.to_lowercase().contains("(recommended)");
            let label = option
                .label
                .replace(" (Recommended)", "")
                .replace(" (recommended)", "");
            let checked = panel.checked.contains(&ix);
            let focused = live && panel.keyboard && panel.focus == ix;
            let picked =
                matches!(mode, Mode::Exit { frame, chosen } if chosen == Some(ix) && frame.check);
            let key = (ix < 9).then(|| (ix + 1).to_string());
            let multi = q.multi;
            let row = div()
                .id(ElementId::Name(format!("question-row-{epoch}-{ix}").into()))
                .min_h(zpx(40.))
                .px(zpx(12.))
                .py(zpx(10.))
                .rounded(zpx(8.))
                .bg(ink(t, if checked || picked { 0.07 } else { 0.03 }))
                .border_1()
                .border_color(ink(t, if checked { 0.14 } else { 0.06 }))
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(10.))
                .when(focused, |d| d.shadow(focus_ring(t)))
                .when(live, |d| d.hover_fade(ink(t, 0.06), None))
                .when(multi, |d| {
                    d.child(
                        div()
                            .flex_none()
                            .size(zpx(16.))
                            .rounded(zpx(4.))
                            .border_1()
                            .border_color(ink(t, if checked { 0.9 } else { 0.3 }))
                            .bg(if checked {
                                ink(t, 0.9)
                            } else {
                                gpui::transparent_black()
                            })
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(c(t.window))
                            .when(checked, |d| d.child(Icon::Check.el(zpx(11.)))),
                    )
                })
                .children(key.map(|k| {
                    let chip = if picked {
                        Icon::Check.el(zpx(12.)).into_any_element()
                    } else {
                        k.into_any_element()
                    };
                    key_chip(chip, ink(t, 0.08), ink(t, 0.5)).ml_0()
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_size(zpx(13.5))
                                .line_height(zpx(18.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(ink(t, 0.9))
                                .child(label),
                        )
                        .children(option.detail.clone().map(|d| {
                            div()
                                .text_size(zpx(12.))
                                .line_height(zpx(16.))
                                .text_color(ink(t, 0.5))
                                .child(d)
                        })),
                )
                .when(recommended, |d| {
                    d.child(
                        div()
                            .flex_none()
                            .text_size(zpx(11.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(c(t.accent).opacity(0.8))
                            .child("Recommended"),
                    )
                })
                .when(live, |d| {
                    d.cursor_pointer()
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            if multi {
                                this.with_panel(|p| {
                                    p.toggle(ix);
                                    p.focus = ix;
                                });
                                cx.notify();
                            } else {
                                this.answer_option(ix, Some(ix), cx);
                            }
                        }))
                });
            list = list.child(place(row, look));
        }
        let other_ix = q.options.len();
        let field = |this: &Self| {
            div()
                .min_h(zpx(36.))
                .px(zpx(10.))
                .py(zpx(6.))
                .rounded(zpx(8.))
                .bg(c(t.code).opacity(0.6))
                .border_1()
                .border_color(c(t.accent).opacity(0.5))
                .t_reading()
                .child(this.question_input.clone())
        };
        if q.other {
            let look = mode.part(20. + 20. * other_ix.min(6) as f32, reduced);
            let focused = live && panel.keyboard && panel.focus == other_ix;
            let el: AnyElement = if panel.other_open && live {
                field(self).into_any_element()
            } else {
                div()
                    .id(ElementId::Name(format!("question-other-{epoch}").into()))
                    .min_h(zpx(40.))
                    .px(zpx(12.))
                    .py(zpx(10.))
                    .rounded(zpx(8.))
                    .bg(ink(t, 0.03))
                    .border_1()
                    .border_color(ink(t, 0.06))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(10.))
                    .when(focused, |d| d.shadow(focus_ring(t)))
                    .when(live, |d| d.hover_fade(ink(t, 0.06), None))
                    .child(key_chip("O", ink(t, 0.08), ink(t, 0.5)).ml_0())
                    .child(
                        div()
                            .text_size(zpx(13.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(ink(t, 0.6))
                            .child("Other…"),
                    )
                    .when(live, |d| {
                        d.cursor_pointer().on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.open_other(cx)),
                        )
                    })
                    .into_any_element()
            };
            list = list.child(place(el, look));
        }
        if text_only && live {
            list = list.child(place(field(self), mode.part(40., reduced)));
        }
        rows.push(list.into_any_element());

        let hint = if text_only || panel.other_open {
            if q.required {
                "⏎ send"
            } else {
                "⏎ send · Esc skip"
            }
        } else if q.multi {
            if q.required {
                "↑↓ move · Space toggle · ⏎ submit"
            } else {
                "↑↓ move · Space toggle · ⏎ submit · Esc skip"
            }
        } else if q.required {
            "↑↓ move · ⏎ choose"
        } else {
            "↑↓ move · ⏎ choose · Esc skip"
        };
        let rows_n = approval::question_rows(q).min(6) as f32;
        let foot_look = mode.part(40. + 20. * rows_n, reduced);
        let can_submit = !panel.checked.is_empty();
        let submit = (q.multi || text_only || panel.other_open).then(|| {
            let enabled = if q.multi && !panel.other_open {
                can_submit
            } else {
                true
            };
            div()
                .id(ElementId::Name(format!("question-submit-{epoch}").into()))
                .flex_none()
                .h(zpx(28.))
                .pl(zpx(10.))
                .pr(zpx(6.))
                .rounded(zpx(8.))
                .flex()
                .flex_row()
                .items_center()
                .bg(ink(t, if enabled { 0.92 } else { 0.1 }))
                .text_size(zpx(12.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(if enabled {
                    c(t.window).opacity(0.9)
                } else {
                    ink(t, 0.35)
                })
                .when(live && enabled, |d| {
                    d.hover_fade(c(t.window).opacity(0.1), Some(c(t.window).opacity(0.18)))
                })
                .child("Submit")
                .child(key_chip(
                    "⏎",
                    c(t.window).opacity(if enabled { 0.12 } else { 0.0 }),
                    if enabled {
                        c(t.window).opacity(0.6)
                    } else {
                        ink(t, 0.3)
                    },
                ))
                .when(live && enabled, |d| {
                    d.cursor_pointer()
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            let text_mode = this.current_panel().is_some_and(|p| p.needs_text());
                            if text_mode {
                                this.submit_question_text(window, cx);
                            } else {
                                this.submit_checked(cx);
                            }
                        }))
                })
        });
        let footer = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(8.))
            .child(
                div()
                    .flex_1()
                    .text_size(zpx(11.5))
                    .text_color(ink(t, 0.4))
                    .child(hint),
            )
            .children(submit)
            .child(self.stop_button(t, epoch, live, cx));
        rows.push(place(footer, foot_look).into_any_element());
        rows
    }
}

//! Folding motion shared by the diff sections and the two trees: how open
//! something is on this frame, and where tree rows sit while a folder's
//! children slide in or out.
//!
//! Progress runs 0 (closed) to 1 (open). Opening is quick off the mark and
//! lands soft; closing is shorter. A reversal mid-way starts from where the
//! motion is, so nothing jumps. The clock starts on the first frame drawn
//! (so a frame spent measuring is not counted against the curve).

use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{AnyElement, div, prelude::*};

use crate::motion::{ease_out, ease_out_strong};
use crate::style::zpx;

pub(in crate::view) const OPEN: Duration = Duration::from_millis(220);
pub(in crate::view) const CLOSE: Duration = Duration::from_millis(170);
pub(super) const CHEVRON_OPEN: Duration = Duration::from_millis(180);
pub(super) const CHEVRON_CLOSE: Duration = Duration::from_millis(150);

#[derive(Clone, Copy, Debug)]
pub(super) struct Fold {
    at: Option<Instant>,
    from: f32,
    from_chevron: f32,
    to: f32,
}

impl Fold {
    pub(super) fn new(opening: bool) -> Self {
        let to = if opening { 1. } else { 0. };
        Self {
            at: None,
            from: 1. - to,
            from_chevron: 1. - to,
            to,
        }
    }

    pub(super) fn opening(&self) -> bool {
        self.to > 0.5
    }

    /// The clock starts on the first frame that draws the motion.
    pub(super) fn start(&mut self, now: Instant) {
        self.at.get_or_insert(now);
    }

    /// Back the other way, from wherever it is now.
    pub(super) fn reverse(&mut self, now: Instant) {
        let (p, ch) = (self.content(now), self.chevron(now));
        *self = Self {
            at: self.at.map(|_| now),
            from: p,
            from_chevron: ch,
            to: 1. - self.to,
        };
    }

    fn run(&self, now: Instant, from: f32, full: Duration) -> f32 {
        let Some(at) = self.at else { return 0. };
        // A part-way reversal covers less ground in less time.
        let span = full.as_secs_f32() * (self.to - from).abs().max(0.35);
        (now.saturating_duration_since(at).as_secs_f32() / span).clamp(0., 1.)
    }

    fn eased(&self, k: f32, from: f32) -> f32 {
        let e = if self.opening() {
            ease_out_strong(k)
        } else {
            ease_out(k)
        };
        from + (self.to - from) * e
    }

    /// How far open the content is.
    pub(super) fn content(&self, now: Instant) -> f32 {
        let k = self.run(now, self.from, if self.opening() { OPEN } else { CLOSE });
        self.eased(k, self.from)
    }

    /// How far open the chevron is (it settles a little sooner).
    pub(super) fn chevron(&self, now: Instant) -> f32 {
        let k = self.run(
            now,
            self.from_chevron,
            if self.opening() {
                CHEVRON_OPEN
            } else {
                CHEVRON_CLOSE
            },
        );
        self.eased(k, self.from_chevron)
    }

    pub(super) fn done(&self, now: Instant) -> bool {
        self.at.is_some()
            && self.run(now, self.from, if self.opening() { OPEN } else { CLOSE }) >= 1.
            && self.run(
                now,
                self.from_chevron,
                if self.opening() {
                    CHEVRON_OPEN
                } else {
                    CHEVRON_CLOSE
                },
            ) >= 1.
    }
}

/// A folder chevron's turn: pointing down when open, right when closed,
/// always by the short way: a quarter turn back from a full one (gpui
/// takes turns in 0..=1, and a full turn reads as none).
pub(in crate::view) fn chevron_turn(open: f32) -> f32 {
    1. - 0.25 * (1. - open.clamp(0., 1.))
}

/// A folder opening or closing in a tree of fixed-height rows.
pub(super) struct TreeFold<R> {
    pub(super) key: String,
    pub(super) motion: Fold,
    /// The first child row, in `rows`.
    pub(super) start: usize,
    pub(super) children: usize,
    /// How many rows there were before the toggle.
    before: usize,
    /// The rows with the folder open, drawn until the motion ends (none
    /// when there were no children to move yet).
    pub(super) rows: Option<Rc<Vec<R>>>,
}

impl<R> TreeFold<R> {
    /// A folder toggled: `old` and `new` are the rows before and after,
    /// `at` the folder's row. Toggling the moving folder again reverses it.
    pub(super) fn toggled(
        prev: Option<Self>,
        key: &str,
        at: Option<usize>,
        old: Rc<Vec<R>>,
        new: Rc<Vec<R>>,
        now: Instant,
        reduced: bool,
    ) -> Option<Self> {
        if reduced {
            return None;
        }
        if let Some(mut prev) = prev.filter(|p| p.key == key) {
            prev.motion.reverse(now);
            return Some(prev);
        }
        let opening = new.len() >= old.len();
        let children = new.len().abs_diff(old.len());
        let before = old.len();
        Some(Self {
            key: key.to_string(),
            motion: Fold::new(opening),
            start: at? + 1,
            children,
            before,
            rows: (children > 0).then(|| if opening { new } else { old }),
        })
    }

    /// The children arrived after the folder opened (a folder read from
    /// disk): reveal them from here.
    pub(super) fn arrived(&mut self, rows: Rc<Vec<R>>) {
        let children = rows.len().saturating_sub(self.before);
        if self.rows.is_none() && self.motion.opening() && children > 0 {
            self.children = children;
            self.rows = Some(rows);
            self.motion = Fold::new(true);
        }
    }

    /// The pixels of the children showing, at `row_h` a row.
    pub(super) fn reveal(&self, now: Instant, row_h: f32) -> f32 {
        self.motion.content(now) * self.children as f32 * row_h
    }
}

/// Rows of a fixed-height list while `children` rows from `start` are
/// `reveal` pixels open: those whose top lands in slot `slot`, each with
/// its visible height and its offset in the slot. Children are clipped
/// from the bottom and the rows after them follow the edge, so the list
/// keeps one row per slot without uneven heights.
pub(super) fn slot_rows(
    slot: usize,
    len: usize,
    start: usize,
    children: usize,
    reveal: f32,
    row_h: f32,
) -> Vec<(usize, f32, f32)> {
    const EPS: f32 = 0.01;
    if slot < start {
        return if slot < len {
            vec![(slot, row_h, 0.)]
        } else {
            Vec::new()
        };
    }
    let (lo, hi) = (slot as f32 * row_h, (slot + 1) as f32 * row_h);
    let base = start as f32 * row_h;
    let reveal = reveal.clamp(0., children as f32 * row_h);
    let mut out = Vec::new();
    let first = (((lo - base) / row_h).floor().max(0.)) as usize;
    for k in first..children {
        let top = base + k as f32 * row_h;
        if top >= hi - EPS {
            break;
        }
        let shown = (reveal - k as f32 * row_h).clamp(0., row_h);
        if shown <= EPS {
            break;
        }
        if top >= lo - EPS {
            out.push((start + k, shown, top - lo));
        }
    }
    let after = base + reveal;
    let j = ((lo - after) / row_h - EPS).ceil().max(0.) as usize;
    for j in j..j + 2 {
        let top = after + j as f32 * row_h;
        let ix = start + children + j;
        if top >= hi - EPS || ix >= len {
            break;
        }
        if top >= lo - EPS {
            out.push((ix, row_h, top - lo));
        }
    }
    out
}

/// One slot of a fixed-height list while a folder moves: the rows whose
/// top lands in it, the folder's children clipped, faded and drifting in.
pub(super) fn slot(
    id: usize,
    entries: Vec<(usize, f32, f32)>,
    children: std::ops::Range<usize>,
    open: f32,
    row_h: f32,
    row: &dyn Fn(usize) -> AnyElement,
) -> AnyElement {
    div()
        .id(("fold-slot", id))
        .relative()
        .h(zpx(row_h))
        .w_full()
        .children(entries.into_iter().map(|(ix, shown, dy)| {
            let child = children.contains(&ix);
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(zpx(dy))
                .h(zpx(shown))
                .overflow_hidden()
                .child(
                    div()
                        .w_full()
                        .when(child, |d| {
                            d.relative()
                                .top(zpx(-3. * (1. - open)))
                                .opacity(0.25 + 0.75 * open)
                        })
                        .child(row(ix)),
                )
        }))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(f: &Fold, base: Instant, ms: u64) -> f32 {
        f.content(base + Duration::from_millis(ms))
    }

    #[test]
    fn opening_rises_steadily_and_lands_on_open() {
        let base = Instant::now();
        let mut f = Fold::new(true);
        f.start(base);
        let mut last = 0.;
        for ms in (0..=240).step_by(8) {
            let p = at(&f, base, ms);
            assert!(p >= last && p <= 1., "{ms}ms: {p} after {last}");
            last = p;
        }
        assert_eq!(last, 1.);
        assert!(f.done(base + OPEN));
    }

    #[test]
    fn closing_is_shorter_than_opening() {
        let base = Instant::now();
        let mut f = Fold::new(false);
        f.start(base);
        assert!(f.done(base + CLOSE) && CLOSE < OPEN);
        assert_eq!(at(&f, base, 0), 1.);
        assert_eq!(at(&f, base, 400), 0.);
    }

    #[test]
    fn a_reversal_starts_where_the_motion_is() {
        let base = Instant::now();
        let mut f = Fold::new(true);
        f.start(base);
        let mid = base + Duration::from_millis(60);
        let p = f.content(mid);
        f.reverse(mid);
        assert!((f.content(mid) - p).abs() < 1e-4);
        assert!(f.content(mid + Duration::from_millis(30)) < p);
        assert!(f.done(mid + CLOSE));
    }

    #[test]
    fn nothing_moves_before_the_first_frame() {
        let f = Fold::new(true);
        assert_eq!(f.content(Instant::now()), 0.);
        assert!(!f.done(Instant::now()));
    }

    #[test]
    fn fully_open_children_sit_one_per_slot() {
        for s in 0..10 {
            assert_eq!(slot_rows(s, 10, 3, 4, 4. * 20., 20.), vec![(s, 20., 0.)]);
        }
    }

    #[test]
    fn closed_children_let_the_rows_after_them_rise() {
        assert_eq!(slot_rows(3, 10, 3, 4, 0., 20.), vec![(7, 20., 0.)]);
        assert_eq!(slot_rows(5, 10, 3, 4, 0., 20.), vec![(9, 20., 0.)]);
        assert!(slot_rows(6, 10, 3, 4, 0., 20.).is_empty());
    }

    #[test]
    fn half_open_children_are_clipped_and_the_rest_follow() {
        // 30px of 80 open: a full child, a 10px child, then the next row.
        assert_eq!(slot_rows(3, 10, 3, 4, 30., 20.), vec![(3, 20., 0.)]);
        assert_eq!(
            slot_rows(4, 10, 3, 4, 30., 20.),
            vec![(4, 10., 0.), (7, 20., 10.)]
        );
        assert_eq!(slot_rows(5, 10, 3, 4, 30., 20.), vec![(8, 20., 10.)]);
        // Every row drawn once.
        let mut seen: Vec<usize> = (0..10)
            .flat_map(|s| slot_rows(s, 10, 3, 4, 30., 20.))
            .map(|r| r.0)
            .collect();
        seen.sort();
        assert_eq!(seen, vec![0, 1, 2, 3, 4, 7, 8, 9]);
    }
}

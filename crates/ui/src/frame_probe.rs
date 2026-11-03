//! Frame timing while the sidebar animates, for tuning its motion. Off
//! unless `KEEL_FRAME_LOG` is set; then each frame during a toggle logs the
//! gap since the previous frame and how long render + layout took (to the
//! root's paint), under target `keel_ui::frames`.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::LazyLock;
use std::time::Instant;

static ENABLED: LazyLock<bool> = LazyLock::new(|| std::env::var_os("KEEL_FRAME_LOG").is_some());

pub fn enabled() -> bool {
    *ENABLED
}

#[derive(Default)]
pub struct FrameProbe {
    last: Cell<Option<Instant>>,
}

impl FrameProbe {
    /// Call at the top of render while measuring. Returns two zero-size
    /// canvases: the root's first child (its paint runs once the whole tree
    /// is built, laid out and prepainted) and its last (once everything else
    /// has painted). The last one logs the frame.
    pub fn frame(&self, label: &'static str) -> (gpui::AnyElement, gpui::AnyElement) {
        use gpui::{IntoElement, Styled};
        let start = Instant::now();
        let gap = self
            .last
            .replace(Some(start))
            .map(|at| start.duration_since(at).as_secs_f32() * 1e3);
        let laid_out = Rc::new(Cell::new(0f32));
        let mark = laid_out.clone();
        let first = gpui::canvas(
            |_, _, _| {},
            move |_, _, _, _| mark.set(start.elapsed().as_secs_f32() * 1e3),
        )
        .absolute()
        .size_0()
        .into_any_element();
        let logged = Rc::new(Cell::new(false));
        let last = gpui::canvas(
            |_, _, _| {},
            move |_, _, _, _| {
                if !logged.replace(true) {
                    tracing::info!(
                        target: "keel_ui::frames",
                        what = label,
                        gap_ms = gap.unwrap_or(0.),
                        layout_ms = laid_out.get(),
                        painted_ms = start.elapsed().as_secs_f32() * 1e3,
                        "frame"
                    );
                }
            },
        )
        .absolute()
        .size_0()
        .into_any_element();
        (first, last)
    }

    pub fn reset(&self) {
        self.last.set(None);
    }
}

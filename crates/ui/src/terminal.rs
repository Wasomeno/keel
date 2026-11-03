//! Terminal emulation for the panel: alacritty's grid and parser fed with the
//! PTY bytes the engine streams, keystrokes encoded back into terminal input,
//! and the visible screen flattened into styled lines for painting.

use std::cell::RefCell;
use std::rc::Rc;

use alacritty_terminal::Term;
use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, TermMode};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor, Rgb};

/// Rows kept above the screen.
const SCROLLBACK: usize = 5_000;

/// Replies the terminal owes the shell (cursor reports and the like), and title changes.
#[derive(Clone, Default)]
pub struct Listener {
    replies: Rc<RefCell<Vec<String>>>,
    title: Rc<RefCell<Option<String>>>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        match event {
            Event::PtyWrite(text) => self.replies.borrow_mut().push(text),
            Event::Title(title) => *self.title.borrow_mut() = Some(title),
            Event::ResetTitle => *self.title.borrow_mut() = None,
            _ => {}
        }
    }
}

struct Size {
    cols: usize,
    rows: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// One terminal's screen state.
pub struct Emulator {
    term: Term<Listener>,
    parser: Processor,
    listener: Listener,
    pub cols: usize,
    pub rows: usize,
}

/// A run of cells with one style, in a line.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    /// Byte range into the line's text.
    pub range: std::ops::Range<usize>,
    pub fg: u32,
    pub bg: Option<u32>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

/// One screen line ready to paint.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub text: String,
    pub runs: Vec<Run>,
}

/// The visible screen plus where to draw the cursor.
pub struct Screen {
    pub lines: Vec<Line>,
    /// (column, row) on screen, when the cursor is visible and not scrolled away.
    pub cursor: Option<(usize, usize)>,
}

/// Foreground, background, and the 16 ANSI colors, `0xRRGGBB`.
#[derive(Clone, Copy)]
pub struct Palette {
    pub fg: u32,
    pub bg: u32,
    pub ansi: [u32; 16],
}

impl Palette {
    pub const DARK: Palette = Palette {
        fg: 0xE4E4E7,
        bg: 0x161618,
        ansi: [
            0x1F1F22, 0xF2555A, 0x3FB950, 0xF0A63A, 0x6E7FF5, 0xC084FC, 0x2DD4BF, 0xD4D4D8, //
            0x52525B, 0xFF7B7F, 0x56D364, 0xF8C26B, 0x96A3FF, 0xD8B4FE, 0x5EEAD4, 0xFAFAFA,
        ],
    };
    pub const LIGHT: Palette = Palette {
        fg: 0x1E1D1B,
        bg: 0xF7F4EE,
        ansi: [
            0x1E1D1B, 0xBA3333, 0x1F7A3C, 0xA66408, 0x1D68BD, 0x7E22CE, 0x0F766E, 0x6E685E, //
            0x615C54, 0xCA3D3D, 0x28944B, 0xBF770E, 0x257CE2, 0x9333EA, 0x14988E, 0x35322E,
        ],
    };

    fn indexed(&self, ix: u8) -> u32 {
        match ix {
            0..=15 => self.ansi[ix as usize],
            // 6×6×6 color cube.
            16..=231 => {
                let i = ix - 16;
                let level = |v: u8| if v == 0 { 0 } else { 55 + 40 * v as u32 };
                (level(i / 36) << 16) | (level((i / 6) % 6) << 8) | level(i % 6)
            }
            // Grayscale ramp.
            232..=255 => {
                let v = 8 + 10 * (ix - 232) as u32;
                (v << 16) | (v << 8) | v
            }
        }
    }

    fn named(&self, named: NamedColor) -> u32 {
        match named {
            NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::Cursor => self.fg,
            NamedColor::DimForeground => self.ansi[8],
            NamedColor::Background => self.bg,
            other => {
                let ix = other as usize;
                if ix < 16 {
                    self.ansi[ix]
                } else {
                    // Dim variants (DimBlack..DimWhite) follow the normal eight.
                    self.ansi[(ix - NamedColor::DimBlack as usize) % 8]
                }
            }
        }
    }
}

fn rgb(c: Rgb) -> u32 {
    ((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32
}

impl Emulator {
    pub fn new(cols: usize, rows: usize) -> Self {
        let listener = Listener::default();
        let config = Config {
            scrolling_history: SCROLLBACK,
            ..Config::default()
        };
        let size = Size {
            cols: cols.max(2),
            rows: rows.max(2),
        };
        Self {
            term: Term::new(config, &size, listener.clone()),
            parser: Processor::new(),
            listener,
            cols: size.cols,
            rows: size.rows,
        }
    }

    /// Feed PTY output. Returns the replies to write back to the shell.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        self.parser.advance(&mut self.term, bytes);
        std::mem::take(&mut *self.listener.replies.borrow_mut())
    }

    /// The title the shell set, if any.
    pub fn title(&self) -> Option<String> {
        self.listener.title.borrow().clone()
    }

    /// True when the size changed (and the PTY needs a resize too).
    pub fn resize(&mut self, cols: usize, rows: usize) -> bool {
        let (cols, rows) = (cols.max(2), rows.max(2));
        if cols == self.cols && rows == self.rows {
            return false;
        }
        self.cols = cols;
        self.rows = rows;
        self.term.resize(Size { cols, rows });
        true
    }

    /// Scroll into history (positive: up), in lines.
    pub fn scroll(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
    }

    /// Back to the live screen, as any keystroke does.
    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    pub fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    /// The visible screen as styled lines.
    pub fn screen(&self, palette: &Palette) -> Screen {
        let content = self.term.renderable_content();
        let colors = content.colors;
        let resolve = |color: Color| -> u32 {
            match color {
                Color::Spec(c) => rgb(c),
                Color::Indexed(ix) => colors[ix as usize]
                    .map(rgb)
                    .unwrap_or_else(|| palette.indexed(ix)),
                Color::Named(named) => colors[named]
                    .map(rgb)
                    .unwrap_or_else(|| palette.named(named)),
            }
        };
        let offset = content.display_offset as i32;
        let mut lines: Vec<Line> = (0..self.rows)
            .map(|_| Line {
                text: String::new(),
                runs: Vec::new(),
            })
            .collect();
        for indexed in content.display_iter {
            let row = indexed.point.line.0 + offset;
            let Some(line) = usize::try_from(row).ok().and_then(|r| lines.get_mut(r)) else {
                continue;
            };
            let cell = indexed.cell;
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let (mut fg, mut bg) = (resolve(cell.fg), resolve(cell.bg));
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if cell.flags.contains(Flags::DIM) {
                fg = blend(fg, palette.bg);
            }
            let ch = if cell.flags.contains(Flags::HIDDEN) || cell.c == '\0' {
                ' '
            } else {
                cell.c
            };
            let start = line.text.len();
            line.text.push(ch);
            let run = Run {
                range: start..line.text.len(),
                fg,
                bg: (bg != palette.bg).then_some(bg),
                bold: cell.flags.contains(Flags::BOLD),
                italic: cell.flags.contains(Flags::ITALIC),
                underline: cell.flags.contains(Flags::UNDERLINE),
            };
            push_run(&mut line.runs, run);
        }
        let cursor_point = content.cursor.point;
        let cursor_row = cursor_point.line.0 + offset;
        let visible = content.mode.contains(TermMode::SHOW_CURSOR)
            && cursor_row >= 0
            && (cursor_row as usize) < self.rows;
        Screen {
            lines,
            cursor: visible.then_some((cursor_point.column.0, cursor_row as usize)),
        }
    }
}

/// Merge with the previous run when the style continues.
fn push_run(runs: &mut Vec<Run>, run: Run) {
    if let Some(last) = runs.last_mut()
        && last.range.end == run.range.start
        && (last.fg, last.bg, last.bold, last.italic, last.underline)
            == (run.fg, run.bg, run.bold, run.italic, run.underline)
    {
        last.range.end = run.range.end;
        return;
    }
    runs.push(run);
}

/// Halfway between two colors, for dim text.
fn blend(a: u32, b: u32) -> u32 {
    let mix = |shift: u32| ((((a >> shift) & 0xFF) + ((b >> shift) & 0xFF)) / 2) << shift;
    mix(16) | mix(8) | mix(0)
}

/// The bytes a keystroke sends to the shell. `None`: not for the terminal
/// (⌘ shortcuts stay with the app).
pub fn encode_key(
    key: &str,
    key_char: Option<&str>,
    ctrl: bool,
    alt: bool,
    shift: bool,
    cmd: bool,
    app_cursor: bool,
) -> Option<String> {
    if cmd {
        return None;
    }
    let arrow = |code: char| {
        Some(if app_cursor {
            format!("\x1bO{code}")
        } else {
            format!("\x1b[{code}")
        })
    };
    let seq = match key {
        "enter" => Some("\r".to_string()),
        "backspace" => Some(if alt { "\x1b\x7f" } else { "\x7f" }.to_string()),
        "tab" if shift => Some("\x1b[Z".to_string()),
        "tab" => Some("\t".to_string()),
        "escape" => Some("\x1b".to_string()),
        "up" => arrow('A'),
        "down" => arrow('B'),
        "right" => arrow('C'),
        "left" => arrow('D'),
        "home" => Some("\x1b[H".to_string()),
        "end" => Some("\x1b[F".to_string()),
        "pageup" => Some("\x1b[5~".to_string()),
        "pagedown" => Some("\x1b[6~".to_string()),
        "delete" => Some("\x1b[3~".to_string()),
        _ => None,
    };
    if seq.is_some() {
        return seq;
    }
    if ctrl {
        // Ctrl+letter is the letter's control code: Ctrl-C is 0x03.
        let c = key.chars().next().filter(|_| key.chars().count() == 1)?;
        return match c.to_ascii_lowercase() {
            c @ 'a'..='z' => Some(((c as u8 - b'a' + 1) as char).to_string()),
            '[' => Some("\x1b".to_string()),
            '\\' => Some("\x1c".to_string()),
            ']' => Some("\x1d".to_string()),
            ' ' | '@' => Some("\0".to_string()),
            _ => None,
        };
    }
    let text = key_char
        .map(str::to_string)
        .or_else(|| (key == "space").then(|| " ".to_string()))?;
    Some(if alt { format!("\x1b{text}") } else { text })
}

/// A paste: wrapped in bracketed-paste markers when the shell asked for them.
pub fn encode_paste(text: &str, bracketed: bool) -> String {
    // Line endings become carriage returns, as a typed Enter would.
    let text = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        format!("\x1b[200~{text}\x1b[201~")
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feeds_text_colors_and_cursor() {
        let mut emu = Emulator::new(20, 4);
        emu.feed(b"hi \x1b[31mred\x1b[0m\r\nnext");
        let screen = emu.screen(&Palette::DARK);
        assert!(screen.lines[0].text.starts_with("hi red"));
        assert!(screen.lines[1].text.starts_with("next"));
        let red = screen.lines[0]
            .runs
            .iter()
            .find(|r| &screen.lines[0].text[r.range.clone()] == "red")
            .expect("red run");
        assert_eq!(red.fg, Palette::DARK.ansi[1]);
        assert_eq!(screen.cursor, Some((4, 1)));
    }

    #[test]
    fn cursor_report_reply_and_resize() {
        let mut emu = Emulator::new(20, 4);
        let replies = emu.feed(b"\x1b[6n");
        assert_eq!(replies, vec!["\x1b[1;1R".to_string()]);
        assert!(emu.resize(40, 10));
        assert!(!emu.resize(40, 10));
        assert_eq!(emu.screen(&Palette::DARK).lines.len(), 10);
    }

    #[test]
    fn keys_encode_like_a_terminal() {
        assert_eq!(
            encode_key("c", Some("c"), true, false, false, false, false).as_deref(),
            Some("\x03")
        );
        assert_eq!(
            encode_key("enter", None, false, false, false, false, false).as_deref(),
            Some("\r")
        );
        assert_eq!(
            encode_key("up", None, false, false, false, false, false).as_deref(),
            Some("\x1b[A")
        );
        assert_eq!(
            encode_key("up", None, false, false, false, false, true).as_deref(),
            Some("\x1bOA")
        );
        assert_eq!(
            encode_key("a", Some("a"), false, true, false, false, false).as_deref(),
            Some("\x1ba")
        );
        assert_eq!(
            encode_key("A", Some("A"), false, false, true, false, false).as_deref(),
            Some("A")
        );
        assert_eq!(
            encode_key("tab", None, false, false, true, false, false).as_deref(),
            Some("\x1b[Z")
        );
        assert_eq!(
            encode_key("v", Some("v"), false, false, false, true, false),
            None
        );
        assert_eq!(encode_paste("a\nb", true), "\x1b[200~a\rb\x1b[201~");
    }

    #[test]
    fn palette_cube_and_grays() {
        assert_eq!(Palette::DARK.indexed(16), 0x000000);
        assert_eq!(Palette::DARK.indexed(231), 0xFFFFFF);
        assert_eq!(Palette::DARK.indexed(232), 0x080808);
    }
}

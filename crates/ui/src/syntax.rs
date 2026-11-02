//! Code highlighting. Syntect grammars give scopes; scopes map onto the six
//! paint roles of [`keel_theme::SyntaxPaintMap`], so every theme colors code
//! the same way. Runs inside the background markdown parse, never on paint.

use std::ops::Range;
use std::sync::LazyLock;

use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};

/// A paint role for a run of code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxKind {
    Keyword,
    String,
    Comment,
    Number,
    Function,
    Type,
}

/// Highlighted runs, by byte range into the block's content.
pub type Spans = Vec<(Range<usize>, SyntaxKind)>;

static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);

/// Blocks larger than this paint plain: highlighting is a nicety, not worth a stall.
const MAX_BYTES: usize = 64 * 1024;

fn syntax_for(language: Option<&str>) -> Option<&'static SyntaxReference> {
    let lang = language?.trim().to_ascii_lowercase();
    let lang = match lang.as_str() {
        "" => return None,
        "ts" | "tsx" | "typescript" => "js",
        "sh" | "shell" | "zsh" | "console" => "bash",
        "py" => "python",
        "rs" => "rust",
        "yml" => "yaml",
        "md" => "markdown",
        other => other,
    };
    SYNTAXES
        .find_syntax_by_token(lang)
        .filter(|syntax| syntax.name != "Plain Text")
}

/// The most specific scope on the stack decides the paint role.
fn classify(stack: &ScopeStack) -> Option<SyntaxKind> {
    stack.as_slice().iter().rev().find_map(|scope| kind_of(*scope))
}

fn kind_of(scope: Scope) -> Option<SyntaxKind> {
    let name = scope.build_string();
    let is = |prefix: &str| name.starts_with(prefix);
    if is("comment") {
        Some(SyntaxKind::Comment)
    } else if is("string") || is("constant.character") {
        Some(SyntaxKind::String)
    } else if is("constant.numeric") || is("constant.language") {
        Some(SyntaxKind::Number)
    } else if is("entity.name.function") || is("support.function") || is("meta.function-call") {
        Some(SyntaxKind::Function)
    } else if is("entity.name.type") || is("entity.name.class") || is("support.type") || is("support.class") || is("storage.type") {
        Some(SyntaxKind::Type)
    } else if is("keyword") || is("storage") {
        Some(SyntaxKind::Keyword)
    } else {
        None
    }
}

/// Highlight `code` written in `language` (a fence info string).
/// Unknown languages and oversized blocks return no spans.
pub fn highlight(code: &str, language: Option<&str>) -> Spans {
    let Some(syntax) = syntax_for(language) else {
        return Vec::new();
    };
    if code.len() > MAX_BYTES {
        return Vec::new();
    }
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut spans: Spans = Vec::new();
    let mut line_start = 0;
    for line in code.split_inclusive('\n') {
        let Ok(ops) = state.parse_line(line, &SYNTAXES) else {
            return spans;
        };
        let mut cursor = 0;
        for (offset, op) in ops.into_iter().chain(std::iter::once((line.len(), syntect::parsing::ScopeStackOp::Noop))) {
            if offset > cursor
                && let Some(kind) = classify(&stack)
            {
                push(&mut spans, line_start + cursor..line_start + offset, kind);
            }
            cursor = offset;
            let _ = stack.apply(&op);
        }
        line_start += line.len();
    }
    spans
}

/// A language's display name for a file extension ("JSON", "Rust").
pub fn language_name(ext: &str) -> Option<&'static str> {
    syntax_for(Some(ext)).map(|s| s.name.as_str())
}

/// The file editor's highlighter: the same grammars and paint roles as code
/// blocks, so a file reads the same in the editor, the diff and the chat.
/// syntect has no incremental mode, so each edit reparses the file.
pub struct EditorHighlighter {
    language: gpui::SharedString,
    syntax: &'static SyntaxReference,
    runs: Spans,
}

/// Files past this edit plain: a full reparse per keystroke would lag.
// ponytail: whole-file reparse per edit; parse only from the edited line on if big files matter.
const EDITOR_MAX_BYTES: usize = 256 * 1024;

impl EditorHighlighter {
    /// For a file extension or language name; None when there's no grammar.
    pub fn new(language: &str) -> Option<Self> {
        Some(Self { language: language.to_string().into(), syntax: syntax_for(Some(language))?, runs: Vec::new() })
    }

    /// Install on `editor`, for `path`'s language.
    pub fn attach(editor: &mut gpui_base::input::EditorState, cx: &mut gpui::Context<gpui_base::input::EditorState>) {
        let factory: gpui_base::input::InputHighlighterFactory =
            std::rc::Rc::new(|lang: &str| Self::new(lang).map(|h| Box::new(h) as Box<dyn gpui_base::input::InputHighlighter>));
        editor.set_highlighter_factory(factory, cx);
    }
}

impl gpui_base::input::InputHighlighter for EditorHighlighter {
    fn language(&self) -> gpui::SharedString {
        self.language.clone()
    }

    fn update(
        &mut self,
        _edit: Option<gpui_base::input::InputEdit>,
        text: &gpui_base::input::Rope,
        _folding: bool,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<gpui_base::input::EditorState>,
    ) {
        self.runs.clear();
        if text.len() > EDITOR_MAX_BYTES {
            return;
        }
        let text = text.to_string();
        let mut state = ParseState::new(self.syntax);
        let mut stack = ScopeStack::new();
        let mut line_start = 0;
        for line in text.split_inclusive('\n') {
            let Ok(ops) = state.parse_line(line, &SYNTAXES) else {
                return;
            };
            let mut cursor = 0;
            for (offset, op) in ops.into_iter().chain(std::iter::once((line.len(), syntect::parsing::ScopeStackOp::Noop))) {
                if offset > cursor
                    && let Some(kind) = classify(&stack)
                {
                    push(&mut self.runs, line_start + cursor..line_start + offset, kind);
                }
                cursor = offset;
                let _ = stack.apply(&op);
            }
            line_start += line.len();
        }
    }

    fn styles(&self, range: &Range<usize>, resolver: &dyn gpui_base::input::HighlightStyleResolver) -> Vec<(Range<usize>, gpui::HighlightStyle)> {
        // Gap-free runs over `range`, as the editor asks.
        let first = self.runs.partition_point(|(r, _)| r.end <= range.start);
        let (mut out, mut cursor) = (Vec::new(), range.start);
        for (r, kind) in &self.runs[first..] {
            if r.start >= range.end {
                break;
            }
            let (a, b) = (r.start.max(cursor), r.end.min(range.end));
            if a >= b {
                continue;
            }
            if cursor < a {
                out.push((cursor..a, gpui::HighlightStyle::default()));
            }
            out.push((a..b, resolver.style(kind_name(*kind)).unwrap_or_default()));
            cursor = b;
        }
        if cursor < range.end {
            out.push((cursor..range.end, gpui::HighlightStyle::default()));
        }
        out
    }

    fn fold_ranges(&self, _: &gpui_base::input::Rope) -> Vec<gpui_base::input::FoldRange> {
        Vec::new()
    }
}

fn kind_name(kind: SyntaxKind) -> &'static str {
    match kind {
        SyntaxKind::Keyword => "keyword",
        SyntaxKind::String => "string",
        SyntaxKind::Comment => "comment",
        SyntaxKind::Number => "number",
        SyntaxKind::Function => "function",
        SyntaxKind::Type => "type",
    }
}

/// Paint roles to a theme's syntax colours, for the editor.
pub struct EditorPaint(pub keel_theme::SyntaxPaintMap);

impl gpui_base::input::HighlightStyleResolver for EditorPaint {
    fn style(&self, name: &str) -> Option<gpui::HighlightStyle> {
        let p = &self.0;
        let color = match name {
            "keyword" => p.keyword,
            "string" => p.string,
            "comment" => p.comment,
            "number" => p.number,
            "function" => p.function,
            "type" => p.type_name,
            _ => return None,
        };
        Some(gpui::HighlightStyle {
            color: Some(crate::style::c(color << 8 | 0xFF)),
            font_style: (name == "comment").then_some(gpui::FontStyle::Italic),
            ..Default::default()
        })
    }
}

/// Append, merging with the previous run when it continues it.
fn push(spans: &mut Spans, range: Range<usize>, kind: SyntaxKind) {
    if let Some((last, last_kind)) = spans.last_mut()
        && *last_kind == kind
        && last.end == range.start
    {
        last.end = range.end;
        return;
    }
    spans.push((range, kind));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds<'a>(code: &'a str, spans: &Spans) -> Vec<(&'a str, SyntaxKind)> {
        spans.iter().map(|(r, k)| (&code[r.clone()], *k)).collect()
    }

    #[test]
    fn rust_keywords_strings_comments_and_numbers() {
        let code = "// hi\nfn main() { let x = \"s\"; 42 }\n";
        let spans = highlight(code, Some("rust"));
        let got = kinds(code, &spans);
        assert!(got.iter().any(|(t, k)| t.contains("// hi") && *k == SyntaxKind::Comment), "{got:?}");
        assert!(got.contains(&("fn", SyntaxKind::Keyword)) || got.iter().any(|(t, k)| *t == "fn" && *k == SyntaxKind::Type), "{got:?}");
        assert!(got.iter().any(|(t, k)| t.contains("\"s\"") && *k == SyntaxKind::String), "{got:?}");
        assert!(got.contains(&("42", SyntaxKind::Number)), "{got:?}");
        assert!(got.iter().any(|(t, k)| *t == "main" && *k == SyntaxKind::Function), "{got:?}");
    }

    #[test]
    fn aliases_unknown_languages_and_char_boundaries() {
        assert!(!highlight("const a = 1;", Some("ts")).is_empty());
        assert!(!highlight("echo hi # c", Some("sh")).is_empty());
        assert!(highlight("whatever", Some("no-such-lang")).is_empty());
        assert!(highlight("whatever", None).is_empty());
        assert!(EditorHighlighter::new("json").is_some() && EditorHighlighter::new("rs").is_some() && EditorHighlighter::new("tsx").is_some());
        let code = "s = \"héllo — ✓\"\n";
        for (range, _) in highlight(code, Some("python")) {
            assert!(code.is_char_boundary(range.start) && code.is_char_boundary(range.end));
        }
    }
}

//! Files an agent turn produced, shown as chips under its answer.
//!
//! Detection is conservative: a path counts only when a file-writing tool
//! reports it (`write`, `Write`, `create_file`, …) or an answer line says it
//! was created, saved, or written, and only when the file exists on disk.
//! A chip opens the file in the browser pane when the pane can show it
//! (images, SVG, HTML, Markdown), otherwise with the system default app.

use std::path::{Path, PathBuf};

use base64::Engine as _;

use crate::icons::Icon;

/// One file in a [`crate::markdown::TranscriptBlock::Files`] row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRef {
    pub name: String,
    /// Absolute path on this device.
    pub path: String,
}

impl FileRef {
    pub fn new(path: &Path) -> Self {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string_lossy().into_owned());
        Self { name, path: path.to_string_lossy().into_owned() }
    }

    pub fn kind(&self) -> FileKind {
        FileKind::of(Path::new(&self.path))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Image,
    Svg,
    Html,
    Markdown,
    Json,
    Code,
    Text,
    Other,
}

impl FileKind {
    pub fn of(path: &Path) -> Self {
        let ext = extension(path);
        match ext.as_str() {
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "avif" => Self::Image,
            "svg" => Self::Svg,
            "html" | "htm" => Self::Html,
            "md" | "markdown" => Self::Markdown,
            "json" | "jsonl" => Self::Json,
            "txt" | "log" | "csv" | "tsv" => Self::Text,
            "rs" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "py" | "go" | "rb" | "java" | "kt" | "swift" | "c" | "h" | "cpp" | "hpp"
            | "cs" | "css" | "scss" | "sh" | "bash" | "zsh" | "toml" | "yaml" | "yml" | "xml" | "sql" | "lua" | "php" | "vue"
            | "svelte" => Self::Code,
            _ => Self::Other,
        }
    }

    /// The browser pane can show it.
    pub fn previewable(self) -> bool {
        matches!(self, Self::Image | Self::Svg | Self::Html | Self::Markdown)
    }

    pub fn icon(self) -> Icon {
        match self {
            Self::Image | Self::Svg => Icon::Image,
            Self::Html => Icon::Globe,
            Self::Markdown | Self::Text => Icon::FileText,
            Self::Json => Icon::FileJson,
            Self::Code => Icon::FileCode,
            Self::Other => Icon::File,
        }
    }
}

fn extension(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase()
}

/// A tool whose job is producing or modifying a file.
fn writes_files(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["write", "create", "save", "export", "generate", "render", "edit", "patch", "modify", "update", "replace", "append"]
        .iter()
        .any(|w| name.contains(w))
}

/// Paths a finished file-writing tool names, in its title or result line
/// (pi: "Successfully wrote to /a/b.svg"; Claude: "File created successfully at: /a/b.svg").
pub fn candidates_in_tool(name: &str, state: &str, summary: &str) -> Vec<String> {
    if state != "done" || !writes_files(name) {
        return Vec::new();
    }
    let mut out = path_tokens(name);
    out.extend(path_tokens(summary));
    out
}

/// Paths on answer lines that say a file was made or edited ("Created pelican.svg", "Added dummy comments in:").
pub fn candidates_in_text(text: &str) -> Vec<String> {
    const VERBS: [&str; 12] = [
        "created", "wrote", "written", "saved", "generated", "exported", "rendered",
        "edited", "modified", "updated", "changed", "added",
    ];
    let mut in_list = false;
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        if VERBS.iter().any(|v| lower.contains(v)) {
            in_list = true;
            out.extend(path_tokens(line));
        } else if in_list && (trimmed.starts_with(['-', '*', '•']) || trimmed.chars().next().is_some_and(|c| c.is_ascii_digit())) {
            out.extend(path_tokens(line));
        } else if trimmed.is_empty() {
            // Keep in_list across blank lines
        } else {
            in_list = false;
        }
    }
    out
}

/// Words in `text` that look like file paths: a stem and a short extension
/// with a letter in it, not a URL.
fn path_tokens(text: &str) -> Vec<String> {
    text.split(|ch: char| ch.is_whitespace() || matches!(ch, '`' | '"' | '\'' | '(' | ')' | '[' | ']' | '<' | '>' | ',' | ';' | '*'))
        .map(|w| w.trim_end_matches(['.', ':', '!', '?']).trim_start_matches(':'))
        .filter(|w| !w.is_empty() && !w.contains("://") && !w.starts_with('-'))
        .filter(|w| {
            let file = w.rsplit('/').next().unwrap_or(w);
            match file.rsplit_once('.') {
                Some((stem, ext)) => {
                    !stem.is_empty()
                        && (1..=10).contains(&ext.len())
                        && ext.chars().all(|ch| ch.is_ascii_alphanumeric())
                        && ext.chars().any(|ch| ch.is_ascii_alphabetic())
                }
                None => false,
            }
        })
        .map(str::to_string)
        .collect()
}

/// An existing file for `candidate`: absolute as is, `~/…` from home, or
/// relative to one of `roots` (the chat's folder, its worktree).
pub fn resolve(candidate: &str, roots: &[PathBuf]) -> Option<PathBuf> {
    let path = Path::new(candidate);
    let found = if let Some(rest) = candidate.strip_prefix("~/") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(rest)).filter(|p| p.is_file())
    } else if path.is_absolute() {
        path.is_file().then(|| path.to_path_buf())
    } else {
        roots.iter().map(|r| r.join(path)).find(|p| p.is_file())
    }?;
    Some(found.canonicalize().unwrap_or(found))
}

/// Unique existing files for `candidates`, in order.
pub fn resolve_all(candidates: impl IntoIterator<Item = String>, roots: &[PathBuf]) -> Vec<FileRef> {
    let mut out: Vec<FileRef> = Vec::new();
    for candidate in candidates {
        if let Some(path) = resolve(&candidate, roots) {
            let file = FileRef::new(&path);
            if !out.iter().any(|f| f.path == file.path) {
                out.push(file);
            }
        }
    }
    out
}

/// Biggest file the pane previews inline; larger ones go to the system app.
const IMAGE_LIMIT: u64 = 24 * 1024 * 1024;
const TEXT_LIMIT: u64 = 4 * 1024 * 1024;

/// A self-contained page showing `path`, for the browser pane. `bg`/`fg`
/// are the theme's window and text colors (RGBA). `None`: not previewable
/// (wrong type, too big, unreadable).
pub fn preview_html(path: &Path, bg: u32, fg: u32) -> Option<String> {
    let kind = FileKind::of(path);
    if !kind.previewable() {
        return None;
    }
    let size = std::fs::metadata(path).ok()?.len();
    let name = escape(&path.file_name()?.to_string_lossy());
    let (bg, fg) = (css(bg), css(fg));
    match kind {
        FileKind::Image | FileKind::Svg => {
            if size > IMAGE_LIMIT {
                return None;
            }
            let bytes = std::fs::read(path).ok()?;
            let mime = match extension(path).as_str() {
                "svg" => "image/svg+xml",
                "jpg" | "jpeg" => "image/jpeg",
                "gif" => "image/gif",
                "webp" => "image/webp",
                "bmp" => "image/bmp",
                "ico" => "image/x-icon",
                "avif" => "image/avif",
                _ => "image/png",
            };
            let data = base64::engine::general_purpose::STANDARD.encode(bytes);
            // Vectors fill the frame; bitmaps keep their size unless too big.
            let fit = if kind == FileKind::Svg { "width:100%;height:100%;" } else { "" };
            Some(format!(
                "<!doctype html><html><head><meta charset=\"utf-8\"><title>{name}</title><style>\
                 html,body{{margin:0;height:100%;background:{bg};color:{fg};font:12px -apple-system,system-ui,sans-serif}}\
                 body{{display:flex;flex-direction:column;align-items:center;justify-content:center;box-sizing:border-box;padding:24px;gap:12px}}\
                 .frame{{flex:1;min-height:0;width:100%;display:flex;align-items:center;justify-content:center}}\
                 img{{{fit}max-width:100%;max-height:100%;object-fit:contain;border-radius:8px;\
                 background:repeating-conic-gradient(#e9e9ec 0 25%,#fff 0 50%) 0 0/16px 16px;box-shadow:0 1px 3px rgba(0,0,0,.25)}}\
                 .name{{opacity:.6}}</style></head><body><div class=\"frame\"><img src=\"data:{mime};base64,{data}\" alt=\"{name}\"></div>\
                 <div class=\"name\">{name}</div></body></html>"
            ))
        }
        FileKind::Html => {
            if size > TEXT_LIMIT {
                return None;
            }
            std::fs::read_to_string(path).ok()
        }
        FileKind::Markdown => {
            if size > TEXT_LIMIT {
                return None;
            }
            let text = std::fs::read_to_string(path).ok()?;
            let mut body = String::new();
            pulldown_cmark::html::push_html(&mut body, pulldown_cmark::Parser::new_ext(&text, pulldown_cmark::Options::all()));
            Some(format!(
                "<!doctype html><html><head><meta charset=\"utf-8\"><title>{name}</title><style>\
                 body{{margin:0;background:{bg};color:{fg};font:14px/1.6 -apple-system,system-ui,sans-serif}}\
                 main{{max-width:760px;margin:0 auto;padding:32px 28px}}\
                 a{{color:inherit}} img{{max-width:100%}}\
                 pre,code{{font:12.5px Menlo,monospace}} pre{{padding:12px;border-radius:6px;overflow:auto;background:rgba(127,127,127,.12)}}\
                 code{{background:rgba(127,127,127,.12);padding:1px 4px;border-radius:4px}} pre code{{background:none;padding:0}}\
                 table{{border-collapse:collapse}} td,th{{border:1px solid rgba(127,127,127,.3);padding:4px 8px}}\
                 blockquote{{margin:0;padding-left:12px;border-left:2px solid rgba(127,127,127,.4);opacity:.85}}\
                 </style></head><body><main>{body}</main></body></html>"
            ))
        }
        _ => None,
    }
}

/// `file://` URL for the address bar.
pub fn file_url(path: &Path) -> String {
    let mut url = String::from("file://");
    for b in path.to_string_lossy().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => url.push(b as char),
            b => url.push_str(&format!("%{b:02X}")),
        }
    }
    url
}

fn css(rgba: u32) -> String {
    format!("#{rgba:08x}")
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_results_name_written_files() {
        assert_eq!(
            candidates_in_tool("write", "done", "Successfully wrote to /Users/k/codetest/pelican-bike.svg"),
            vec!["/Users/k/codetest/pelican-bike.svg"]
        );
        assert_eq!(candidates_in_tool("Write", "done", "File created successfully at: /a/b.png"), vec!["/a/b.png"]);
        assert_eq!(candidates_in_tool("edit_file", "done", "edit src/lib/cms-shared.ts"), vec!["src/lib/cms-shared.ts"]);
        assert!(candidates_in_tool("write", "running", "Successfully wrote to /a/b.svg").is_empty());
        assert!(candidates_in_tool("bash", "done", "/a/b.svg").is_empty());
        assert!(candidates_in_tool("read", "done", "/a/b.svg").is_empty());
    }

    #[test]
    fn answer_lines_that_create_files() {
        assert_eq!(candidates_in_text("Created pelican-bike.svg\nOpen it in a browser."), vec!["pelican-bike.svg"]);
        assert_eq!(candidates_in_text("I saved it to `out/chart.png`."), vec!["out/chart.png"]);
        assert_eq!(
            candidates_in_text("Added dummy comments in:\n- src/lib/cms-shared.ts\n- src/app/route.ts\n\nNo behavior changes."),
            vec!["src/lib/cms-shared.ts", "src/app/route.ts"]
        );
        assert!(candidates_in_text("See src/main.rs for details.").is_empty());
        assert!(candidates_in_text("Created https://example.com/x.svg and v0.56").is_empty());
    }

    #[test]
    fn resolves_existing_files_only() {
        let dir = std::env::temp_dir().join(format!("keel-files-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.svg"), "<svg/>").unwrap();
        let roots = vec![dir.clone()];
        let files = resolve_all(["a.svg".to_string(), "missing.svg".to_string(), "a.svg".to_string()], &roots);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "a.svg");
        assert!(preview_html(Path::new(&files[0].path), 0x111111ff, 0xeeeeeeff).unwrap().contains("data:image/svg+xml;base64,"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn kinds_pick_preview_or_system() {
        assert!(FileKind::of(Path::new("a.SVG")).previewable());
        assert!(FileKind::of(Path::new("a.html")).previewable());
        assert!(!FileKind::of(Path::new("a.pdf")).previewable());
        assert_eq!(file_url(Path::new("/a b/c.svg")), "file:///a%20b/c.svg");
    }
}

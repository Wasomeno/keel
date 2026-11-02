//! Markdown parsing into stable block rows.
//!
//! Pulldown-cmark splits a message body into top-level block rows.
//! Row id is formatted as `{message_id}#{block_id}` and is stable across commits.
//! Code block height is calculated as `lines * line_height` independent of syntax highlighting.
//! The streaming tail is re-parsed from the start of the last top-level block
//! (the last completed block boundary), using pulldown-cmark source offsets.

use std::ops::Range;

use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

/// One list item: marker ("•" or "3."), nesting depth, and text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListItem {
    pub marker: String,
    pub depth: usize,
    pub text: String,
    /// Inline styles over `text`.
    pub spans: Vec<InlineSpan>,
    /// A task item: checked or not.
    pub task: Option<bool>,
}

/// Inline emphasis on a run of text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InlineStyle {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
}

/// A styled or linked byte range of a block's text. Spans are sorted and
/// never overlap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineSpan {
    pub range: Range<usize>,
    pub style: InlineStyle,
    pub link: Option<String>,
}

/// Text with its inline spans (a table cell).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Inline {
    pub text: String,
    pub spans: Vec<InlineSpan>,
}

/// Horizontal alignment of a table column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnAlign {
    None,
    Left,
    Center,
    Right,
}

/// Text being collected for one block, with the spans over it.
#[derive(Debug, Default)]
struct InlineBuf {
    text: String,
    spans: Vec<InlineSpan>,
}

impl InlineBuf {
    fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    fn clear(&mut self) {
        self.text.clear();
        self.spans.clear();
    }

    /// Unstyled text (code block bodies, separators).
    fn raw(&mut self, s: &str) {
        self.text.push_str(s);
    }

    fn push(&mut self, s: &str, style: InlineStyle, link: Option<&String>) {
        let start = self.text.len();
        self.text.push_str(s);
        if style == InlineStyle::default() && link.is_none() {
            return;
        }
        let end = self.text.len();
        if let Some(last) = self.spans.last_mut()
            && last.range.end == start
            && last.style == style
            && last.link.as_ref() == link
        {
            last.range.end = end;
            return;
        }
        self.spans.push(InlineSpan { range: start..end, style, link: link.cloned() });
    }

    /// The text without surrounding whitespace, spans clipped to it.
    fn take_trimmed(&mut self) -> (String, Vec<InlineSpan>) {
        let lead = self.text.len() - self.text.trim_start().len();
        let end = self.text.trim_end().len().max(lead);
        let text = self.text[lead..end].to_string();
        let mut spans: Vec<InlineSpan> = self
            .spans
            .drain(..)
            .filter_map(|mut span| {
                span.range.start = span.range.start.clamp(lead, end) - lead;
                span.range.end = span.range.end.clamp(lead, end) - lead;
                (span.range.start < span.range.end).then_some(span)
            })
            .collect();
        self.text.clear();
        autolink(&text, &mut spans);
        (text, spans)
    }

    fn take_inline(&mut self) -> Inline {
        let (text, spans) = self.take_trimmed();
        Inline { text, spans }
    }
}

/// Bare `http(s)://` URLs in plain text become links, as GitHub does: the
/// URL runs to whitespace, minus trailing punctuation (and a `)` it didn't
/// open). Text already styled or linked is left alone.
fn autolink(text: &str, spans: &mut Vec<InlineSpan>) {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find("http").map(|i| from + i) {
        let rest = &text[at..];
        // A word that merely ends in "http" (`xhttp://`) is not a URL.
        let boundary = text[..at].chars().next_back().is_none_or(|ch| !ch.is_alphanumeric());
        if !boundary || !(rest.starts_with("https://") || rest.starts_with("http://")) {
            from = at + 4;
            continue;
        }
        let mut url = &rest[..rest.find(|ch: char| ch.is_whitespace() || matches!(ch, '<' | '>' | '"' | '`')).unwrap_or(rest.len())];
        loop {
            let trimmed = url.trim_end_matches(['.', ',', ';', ':', '!', '?', '\'', '"', ']', '}', '*', '_']);
            let trimmed = match trimmed.strip_suffix(')') {
                Some(t) if trimmed.matches(')').count() > trimmed.matches('(').count() => t,
                _ => trimmed,
            };
            if trimmed.len() == url.len() {
                break;
            }
            url = trimmed;
        }
        let range = at..at + url.len();
        from = range.end.max(at + 4);
        if url.split_once("://").is_some_and(|(_, host)| !host.is_empty())
            && !spans.iter().any(|s| s.range.start < range.end && range.start < s.range.end)
        {
            found.push(InlineSpan { range, style: InlineStyle::default(), link: Some(url.to_string()) });
        }
    }
    if !found.is_empty() {
        spans.extend(found);
        spans.sort_by_key(|s| s.range.start);
    }
}

/// A parsed transcript row.
/// One image in a [`TranscriptBlock::Images`] row. `path` is set when the
/// file is on this device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef {
    pub name: String,
    pub path: Option<String>,
}

impl ImageRef {
    /// The `attachments` array of a session message view.
    pub fn parse_list(view: &serde_json::Value) -> Vec<Self> {
        view.get("attachments")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .map(|a| ImageRef {
                        name: a.get("name").and_then(|v| v.as_str()).unwrap_or("image").to_string(),
                        path: a.get("path").and_then(|v| v.as_str()).map(str::to_string),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// One file a tool edited. `old_text` is absent when the harness sent only
/// the new body, or a unified diff in `new_text`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolDiffView {
    pub path: String,
    pub old_text: Option<String>,
    pub new_text: String,
}

/// One tool call inside a [`TranscriptBlock::ToolGroup`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolEntry {
    pub tool_id: String,
    pub name: String,
    /// `running`, `done`, `error`, `pending`, or `cancelled`.
    pub state: String,
    pub summary: String,
    /// What the call works on (file, command, pattern, URL); may be empty.
    pub detail: String,
    /// Images the tool returned (absolute paths that exist here), shown
    /// under the row as [`TranscriptBlock::Shots`].
    pub images: Vec<String>,
    /// ACP kind (`read`, `execute`, …), stored as `tool_kind`. Empty on older sessions.
    pub kind: String,
    /// When the call started and finished, unix millis. Absent on older rows.
    pub started_ms: Option<i64>,
    pub ended_ms: Option<i64>,
    pub diff: Option<ToolDiffView>,
    /// Last streamed output. Empty when the session didn't store one.
    pub output_tail: String,
}

impl ToolEntry {
    /// One `tools` / `parts` entry of a session message view.
    pub fn from_json(t: &serde_json::Value) -> Self {
        let s = |k: &str| t.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let images = t
            .get("images")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str())
                    .filter(|p| std::path::Path::new(p).is_absolute() && std::path::Path::new(p).is_file())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let ms = |k: &str| t.get(k).and_then(|v| v.as_i64());
        // A part's `kind` is its type (`tool`); the ACP kind is `tool_kind`.
        // A legacy `kind` that isn't a part type is still accepted.
        let kind = match s("tool_kind") {
            k if !k.is_empty() => k,
            _ => Some(s("kind")).filter(|k| !matches!(k.as_str(), "" | "tool" | "text" | "reasoning" | "subagent" | "ui" | "plan")).unwrap_or_default(),
        };
        ToolEntry {
            tool_id: s("tool_id"),
            name: s("name"),
            state: s("state"),
            summary: s("summary"),
            detail: s("detail"),
            images,
            kind,
            started_ms: ms("started_ms"),
            ended_ms: ms("ended_ms"),
            diff: parse_diff(t.get("diff")),
            output_tail: s("output_tail"),
        }
    }

    /// Is the call still going (no terminal state yet)?
    pub fn is_live(&self) -> bool {
        matches!(self.state.as_str(), "running" | "pending" | "in_progress")
    }

    /// A question's receipt (`keel:receipt`), not a tool call.
    pub fn is_receipt(&self) -> bool {
        self.name == keel_proto::RECEIPT_TOOL
    }

    /// The `tools` array of a session message view.
    pub fn parse_list(view: &serde_json::Value) -> Vec<Self> {
        view.get("tools")
            .and_then(|v| v.as_array())
            .map(|tools| {
                tools
                    .iter()
                    .map(ToolEntry::from_json)
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// `diff` is an object, or a JSON string of one. A missing path is no diff.
fn parse_diff(value: Option<&serde_json::Value>) -> Option<ToolDiffView> {
    let owned;
    let value = match value? {
        serde_json::Value::String(text) => {
            owned = serde_json::from_str::<serde_json::Value>(text).ok()?;
            &owned
        }
        other => other,
    };
    let text = |k: &str| value.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let path = text("path").filter(|p| !p.is_empty())?;
    Some(ToolDiffView { path, old_text: text("old_text"), new_text: text("new_text").unwrap_or_default() })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptBlock {
    /// Paragraph text with its inline styles and links.
    Paragraph { id: String, content: String, spans: Vec<InlineSpan> },
    /// Fenced or indented code block
    Code {
        id: String,
        language: Option<String>,
        content: String,
        line_count: usize,
        /// Highlight runs, computed with the parse (background); paint only.
        spans: crate::syntax::Spans,
        /// The closing fence has arrived. A diagram waits for it rather than
        /// re-laying out on every streamed token.
        closed: bool,
        /// A shell block the engine marked runnable: its header gets Run.
        run: Option<crate::runnable::Run>,
    },
    /// Heading (h1-h6)
    Heading { id: String, level: u8, content: String, spans: Vec<InlineSpan> },
    /// Blockquote: its paragraphs as lines.
    Quote { id: String, content: String, spans: Vec<InlineSpan> },
    /// Horizontal rule.
    Rule { id: String },
    /// GFM table. `header` cells may all be empty (`| | |`).
    Table { id: String, align: Vec<ColumnAlign>, header: Vec<Inline>, rows: Vec<Vec<Inline>> },
    /// Ordered or bullet list; each item renders as its own line with a marker.
    List { id: String, items: Vec<ListItem> },
    /// The images a user message carries, as one row of thumbnails.
    Images { id: String, items: Vec<ImageRef> },
    /// Images shown large (screenshots a tool returned, markdown images in
    /// assistant text): absolute paths, each a clickable preview.
    Shots { id: String, paths: Vec<String> },
    /// Files an assistant turn produced, as one row of clickable chips.
    Files { id: String, items: Vec<crate::files::FileRef> },
    /// One message's thinking, folded into a single row.
    Reasoning { id: String, content: String, expanded: bool },
    /// One message's tool calls, folded into a single row.
    ToolGroup {
        id: String,
        tools: Vec<ToolEntry>,
        expanded: bool,
        /// Trace lines the user opened (`true`) or closed (`false`), by line
        /// id; the rest follow their status (open while streaming, on error).
        overrides: Vec<(String, bool)>,
    },
    /// The agent's plan, as a line with its entries nested.
    Plan { id: String, entries: Vec<crate::trace::PlanItem>, live: bool, expanded: Option<bool> },
    /// A turn's steps folded into one expandable line ("Show works" / "Hide works").
    TurnSummary { id: String, label: String, failed: bool, expanded: bool },
    /// A spawned subagent: one clickable indicator row.
    Subagent {
        id: String,
        subagent_id: String,
        name: String,
        state: String,
        summary: String,
    },
    /// A chart the engine's generative UI chose for the message.
    /// `replaces` is the table it stands in for, still what Copy copies.
    Chart { id: String, chart: crate::chart::Chart, replaces: Option<Box<TranscriptBlock>> },
    /// Buttons for the options the agent asked the user to pick from.
    Choices { id: String, choices: crate::choices::Choices },
    /// A test run's result card.
    Tests { id: String, run: crate::test_run::TestRun },
    /// A merge request card; `message_id` and `part_id` name its `ui` part
    /// (a merge rewrites it).
    MrCard { id: String, card: crate::mr_card::MrCard, message_id: String, part_id: String },
    /// Before → after figures as tiles.
    Stats { id: String, items: Vec<crate::stats::Stat> },
    /// The files a turn changed, with line counts.
    FileChanges { id: String, card: crate::file_changes::FileChanges, expanded: bool },
    /// Numbered steps, with the one in progress marked.
    Steps { id: String, steps: crate::steps::Steps },
    /// One sentence pulled out of the reply.
    Callout { id: String, callout: crate::callout::Callout },
    /// Side-by-side claims.
    Compare { id: String, columns: Vec<crate::compare::Column> },
}

impl TranscriptBlock {
    pub fn id(&self) -> &str {
        match self {
            Self::Paragraph { id, .. }
            | Self::Code { id, .. }
            | Self::Heading { id, .. }
            | Self::Quote { id, .. }
            | Self::Rule { id }
            | Self::Table { id, .. }
            | Self::List { id, .. }
            | Self::ToolGroup { id, .. }
            | Self::Plan { id, .. }
            | Self::TurnSummary { id, .. }
            | Self::Reasoning { id, .. }
            | Self::Images { id, .. }
            | Self::Shots { id, .. }
            | Self::Files { id, .. }
            | Self::Subagent { id, .. }
            | Self::Chart { id, .. }
            | Self::Choices { id, .. }
            | Self::Tests { id, .. }
            | Self::MrCard { id, .. }
            | Self::Stats { id, .. }
            | Self::FileChanges { id, .. }
            | Self::Steps { id, .. }
            | Self::Callout { id, .. }
            | Self::Compare { id, .. } => id,
        }
    }

    pub(crate) fn set_id(&mut self, new_id: String) {
        match self {
            Self::Paragraph { id, .. }
            | Self::Code { id, .. }
            | Self::Heading { id, .. }
            | Self::Quote { id, .. }
            | Self::Rule { id }
            | Self::Table { id, .. }
            | Self::List { id, .. }
            | Self::ToolGroup { id, .. }
            | Self::Plan { id, .. }
            | Self::TurnSummary { id, .. }
            | Self::Reasoning { id, .. }
            | Self::Images { id, .. }
            | Self::Shots { id, .. }
            | Self::Files { id, .. }
            | Self::Subagent { id, .. }
            | Self::Chart { id, .. }
            | Self::Choices { id, .. }
            | Self::Tests { id, .. }
            | Self::MrCard { id, .. }
            | Self::Stats { id, .. }
            | Self::FileChanges { id, .. }
            | Self::Steps { id, .. }
            | Self::Callout { id, .. }
            | Self::Compare { id, .. } => *id = new_id,
        }
    }

    pub fn content_len(&self) -> usize {
        match self {
            Self::Code { content, run, .. } => content.len() + usize::from(run.is_some()),
            Self::Paragraph { content, spans, .. }
            | Self::Heading { content, spans, .. }
            | Self::Quote { content, spans, .. } => content.len() + spans.len(),
            Self::Rule { .. } => 1,
            Self::Table { header, rows, .. } => {
                header.iter().chain(rows.iter().flatten()).map(|cell| cell.text.len() + cell.spans.len() + 1).sum()
            }
            Self::List { items, .. } => items.iter().map(|i| i.text.len() + i.spans.len() + 1).sum(),
            Self::Images { items, .. } => items.len(),
            Self::Shots { paths, .. } => paths.iter().map(|p| p.len() + 1).sum(),
            Self::Files { items, .. } => items.iter().map(|f| f.path.len() + 1).sum(),
            Self::Reasoning { content, expanded, .. } => content.len() + usize::from(*expanded),
            Self::ToolGroup { tools, expanded, overrides, .. } => {
                tools.iter().map(|t| t.name.len() + t.summary.len() + t.state.len() + t.output_tail.len() + t.kind.len()).sum::<usize>()
                    + usize::from(*expanded)
                    + overrides.iter().map(|(id, open)| id.len() + usize::from(*open) + 1).sum::<usize>()
            }
            Self::Plan { entries, expanded, .. } => {
                entries.iter().map(|e| e.content.len() + e.status.len() + 1).sum::<usize>() + expanded.map_or(0, |o| 1 + usize::from(o))
            }
            Self::TurnSummary { label, expanded, .. } => label.len() + usize::from(*expanded),
            Self::Subagent { name, summary, state, .. } => name.len() + summary.len() + state.len(),
            Self::Chart { chart, .. } => chart.content_len(),
            Self::Choices { choices, .. } => choices.content_len(),
            Self::Tests { run, .. } => run.content_len(),
            Self::MrCard { card, .. } => card.content_len(),
            Self::Stats { items, .. } => items.iter().map(|s| s.label.len() + s.after.len() + 1).sum(),
            Self::FileChanges { card, expanded, .. } => card.content_len() + usize::from(*expanded),
            Self::Steps { steps, .. } => steps.content_len(),
            Self::Callout { callout, .. } => callout.content_len(),
            Self::Compare { columns, .. } => columns.iter().map(|c| c.title.len() + c.points.iter().map(|p| p.len() + 1).sum::<usize>()).sum(),
        }
    }
}

/// Markdown extensions the transcript renders.
fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_MATH
}

/// The next list marker: `3.` for an ordered level, `•` for a bullet one.
fn next_marker(lists: &mut [Option<u64>]) -> String {
    match lists.last_mut() {
        Some(Some(n)) => {
            *n += 1;
            format!("{}.", *n - 1)
        }
        _ => "•".to_string(),
    }
}

/// Parse `text` into top-level blocks, each with its byte range in `text`.
/// Block ids are left empty; callers assign them.
fn parse_top_level(text: &str) -> Vec<(Range<usize>, TranscriptBlock)> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    let mut buf = InlineBuf::default();
    let mut lang: Option<String> = None;
    let mut in_code = false;
    // List state: stack of next ordinal (None = bullet) per nesting level.
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut items: Vec<ListItem> = Vec::new();
    // Inline state.
    let (mut bold, mut italic, mut strike) = (0u32, 0u32, 0u32);
    let mut links: Vec<String> = Vec::new();
    // Blockquote nesting; lists inside a quote become lines of its text.
    let mut quote = 0usize;
    // Table state.
    let mut align: Vec<ColumnAlign> = Vec::new();
    let mut header: Vec<Inline> = Vec::new();
    let mut rows: Vec<Vec<Inline>> = Vec::new();
    let mut row: Vec<Inline> = Vec::new();
    // Local images (`![alt](/abs.png)`, `file://`, `~/`) in the current
    // block; a paragraph of nothing else renders as a Shots row.
    let mut images: Vec<String> = Vec::new();
    let mut in_image = 0usize;
    let mut other_text = false;

    let flush_item = |items: &mut Vec<ListItem>, buf: &mut InlineBuf| {
        if let Some(last) = items.last_mut()
            && last.text.is_empty()
        {
            let (text, spans) = buf.take_trimmed();
            last.text = text;
            last.spans = spans;
        }
        buf.clear();
    };

    for (event, range) in Parser::new_ext(text, options()).into_offset_iter() {
        let style = InlineStyle { bold: bold > 0, italic: italic > 0, strike: strike > 0, code: false };
        match event {
            Event::Start(tag) => {
                if depth == 0 {
                    start = range.start;
                    buf.clear();
                    items.clear();
                    lists.clear();
                    (bold, italic, strike, quote) = (0, 0, 0, 0);
                    links.clear();
                    images.clear();
                    (in_image, other_text) = (0, false);
                }
                depth += 1;
                match tag {
                    Tag::CodeBlock(kind) => {
                        in_code = true;
                        lang = match kind {
                            CodeBlockKind::Fenced(l) if !l.trim().is_empty() => {
                                Some(l.trim().to_string())
                            }
                            _ => None,
                        };
                    }
                    Tag::BlockQuote(_) => quote += 1,
                    Tag::List(first) => {
                        // Text before a nested list belongs to the parent item.
                        if quote == 0 {
                            flush_item(&mut items, &mut buf);
                        }
                        lists.push(first);
                    }
                    Tag::Item if quote == 0 => {
                        flush_item(&mut items, &mut buf);
                        let marker = next_marker(&mut lists);
                        items.push(ListItem {
                            marker,
                            depth: lists.len().saturating_sub(1),
                            text: String::new(),
                            spans: Vec::new(),
                            task: None,
                        });
                    }
                    Tag::Item => {
                        if !buf.is_empty() {
                            buf.raw("\n");
                        }
                        buf.raw(&"  ".repeat(lists.len().saturating_sub(1)));
                        buf.raw(&next_marker(&mut lists));
                        buf.raw(" ");
                    }
                    Tag::FootnoteDefinition(label) => buf.raw(&format!("[^{label}] ")),
                    Tag::Paragraph if (!lists.is_empty() || quote > 0) && !buf.is_empty() => buf.raw("\n"),
                    Tag::Emphasis => italic += 1,
                    Tag::Strong => bold += 1,
                    Tag::Strikethrough => strike += 1,
                    Tag::Link { dest_url, .. } => links.push(dest_url.to_string()),
                    Tag::Image { dest_url, .. } => {
                        in_image += 1;
                        // Inline (mixed with text) it stays a link to the file.
                        match local_image_path(&dest_url) {
                            Some(path) => {
                                links.push(format!("file://{}", path.replace(' ', "%20")));
                                images.push(path);
                            }
                            None => {
                                links.push(dest_url.to_string());
                                other_text = true;
                            }
                        }
                    }
                    Tag::Table(alignments) => {
                        align = alignments
                            .iter()
                            .map(|a| match a {
                                Alignment::None => ColumnAlign::None,
                                Alignment::Left => ColumnAlign::Left,
                                Alignment::Center => ColumnAlign::Center,
                                Alignment::Right => ColumnAlign::Right,
                            })
                            .collect();
                        header.clear();
                        rows.clear();
                    }
                    Tag::TableHead | Tag::TableRow => row.clear(),
                    Tag::TableCell => buf.clear(),
                    _ => {}
                }
            }
            Event::End(tag_end) => {
                depth = depth.saturating_sub(1);
                match tag_end {
                    TagEnd::Item if quote == 0 => flush_item(&mut items, &mut buf),
                    TagEnd::List(_) => {
                        lists.pop();
                    }
                    TagEnd::BlockQuote(_) => quote = quote.saturating_sub(1),
                    TagEnd::CodeBlock => in_code = false,
                    TagEnd::Emphasis => italic = italic.saturating_sub(1),
                    TagEnd::Strong => bold = bold.saturating_sub(1),
                    TagEnd::Strikethrough => strike = strike.saturating_sub(1),
                    TagEnd::Link => {
                        links.pop();
                    }
                    TagEnd::Image => {
                        links.pop();
                        in_image = in_image.saturating_sub(1);
                    }
                    TagEnd::TableCell => row.push(buf.take_inline()),
                    TagEnd::TableHead => header = std::mem::take(&mut row),
                    TagEnd::TableRow => rows.push(std::mem::take(&mut row)),
                    _ => {}
                }
                if depth == 0 {
                    let block = match tag_end {
                        TagEnd::CodeBlock => {
                            let content = buf.text.strip_suffix('\n').unwrap_or(&buf.text).to_string();
                            let raw = text[start..range.end].trim_end();
                            let fence = raw.ends_with("```") || raw.ends_with("~~~");
                            code_block(lang.take(), content, fence && raw.lines().count() > 1)
                        }
                        TagEnd::Heading(level) => {
                            let (content, spans) = buf.take_trimmed();
                            TranscriptBlock::Heading { id: String::new(), level: level as u8, content, spans }
                        }
                        TagEnd::List(_) => TranscriptBlock::List {
                            id: String::new(),
                            items: std::mem::take(&mut items),
                        },
                        TagEnd::BlockQuote(_) => {
                            let (content, spans) = buf.take_trimmed();
                            TranscriptBlock::Quote { id: String::new(), content, spans }
                        }
                        TagEnd::Table => TranscriptBlock::Table {
                            id: String::new(),
                            align: std::mem::take(&mut align),
                            header: std::mem::take(&mut header),
                            rows: std::mem::take(&mut rows),
                        },
                        TagEnd::Paragraph if !images.is_empty() && !other_text => TranscriptBlock::Shots {
                            id: String::new(),
                            paths: std::mem::take(&mut images),
                        },
                        _ => {
                            let (content, spans) = buf.take_trimmed();
                            TranscriptBlock::Paragraph { id: String::new(), content, spans }
                        }
                    };
                    buf.clear();
                    out.push((start..range.end, block));
                }
            }
            Event::Text(t) if in_code => buf.raw(&t),
            Event::Text(t) => {
                if in_image == 0 && !t.trim().is_empty() {
                    other_text = true;
                }
                buf.push(&t, style, links.last())
            }
            Event::Html(t) | Event::InlineHtml(t) => buf.raw(&html_text(&t)),
            // Kept as written: a streamed reference parses before its definition
            // arrives and stays `[^a]` text, so the full parse must match it.
            Event::FootnoteReference(label) => buf.push(&format!("[^{label}]"), style, links.last()),
            Event::InlineMath(t) => {
                other_text = true;
                buf.push(&t, InlineStyle { code: true, ..style }, links.last())
            }
            Event::DisplayMath(t) => {
                other_text = true;
                if !buf.is_empty() {
                    buf.raw("\n");
                }
                buf.push(t.trim(), InlineStyle { code: true, ..style }, links.last());
            }
            Event::Code(code) => {
                other_text = true;
                buf.push(&code, InlineStyle { code: true, ..style }, links.last())
            }
            Event::SoftBreak | Event::HardBreak => buf.raw("\n"),
            Event::TaskListMarker(checked) => {
                if quote > 0 {
                    buf.raw(if checked { "☑ " } else { "☐ " });
                } else if let Some(last) = items.last_mut() {
                    last.task = Some(checked);
                }
            }
            Event::Rule if depth == 0 => out.push((range, TranscriptBlock::Rule { id: String::new() })),
            _ => {}
        }
    }

    // A streaming tail can end mid-block only in theory: pulldown closes
    // every open tag at end of input, so `depth` is 0 here.
    out
}

/// Raw HTML as the transcript shows it: `<br>` breaks the line, comments
/// vanish, any other tag stays literal (an agent's `<Composer>` is prose).
fn html_text(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(at) = rest.find('<') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        if let Some(end) = rest.starts_with("<!--").then(|| rest.find("-->")).flatten() {
            rest = &rest[end + 3..];
        } else if let Some(end) = rest.find('>')
            && matches!(rest[1..end].trim_end_matches('/').trim().to_ascii_lowercase().as_str(), "br")
        {
            out.push('\n');
            rest = &rest[end + 1..];
        } else {
            out.push('<');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

/// The file a markdown image points at, when it is on this device:
/// an absolute path, a `file://` URL (percent-decoded), or `~/…`.
pub fn local_image_path(dest: &str) -> Option<String> {
    let dest = dest.trim();
    let path = if let Some(rest) = dest.strip_prefix("file://") {
        let rest = rest.strip_prefix("localhost").unwrap_or(rest);
        percent_decode(rest)
    } else if let Some(rest) = dest.strip_prefix("~/") {
        let home = std::env::var("HOME").ok()?;
        format!("{}/{rest}", home.trim_end_matches('/'))
    } else if dest.starts_with('/') {
        percent_decode(dest)
    } else {
        return None;
    };
    path.starts_with('/').then_some(path)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len()
            && let Some(b) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(b);
                i += 3;
                continue;
            }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub(crate) fn code_block(language: Option<String>, content: String, closed: bool) -> TranscriptBlock {
    let line_count = content.lines().count().max(1);
    let spans = crate::syntax::highlight(&content, language.as_deref());
    TranscriptBlock::Code {
        id: String::new(),
        language,
        content,
        line_count,
        spans,
        closed,
        run: None,
    }
}

/// Parse full message text into blocks with stable IDs: `{message_id}#{block_id}`.
pub fn parse_markdown_blocks(message_id: &str, text: &str) -> Vec<TranscriptBlock> {
    parse_top_level(text)
        .into_iter()
        .enumerate()
        .map(|(ix, (_, mut b))| {
            b.set_id(format!("{message_id}#{ix}"));
            b
        })
        .collect()
}

/// Incremental streaming message blocks state.
///
/// Keeps stable block IDs `{message_id}#{block_id}`.
/// Re-parses the streaming tail from the last completed block boundary without
/// re-splitting earlier blocks. `completed_text_len` is always the byte offset
/// where the current tail block starts, so completed blocks are never re-emitted.
#[derive(Debug, Clone)]
pub struct MessageBlockState {
    pub message_id: String,
    pub completed_blocks: Vec<TranscriptBlock>,
    pub completed_text_len: usize,
    pub full_text: String,
    /// Parsed but not final blocks between the completed ones and the tail
    /// (after a table that is still open, see [`Self::reparse_tail`]).
    pub pending_blocks: Vec<TranscriptBlock>,
    pub current_tail_block: Option<TranscriptBlock>,
    /// `full_text.len()` at the last parse: equal means the blocks are
    /// current and the UI skips sending this text to the parser.
    pub parsed_len: usize,
}

impl MessageBlockState {
    pub fn new(message_id: String) -> Self {
        Self {
            message_id,
            completed_blocks: Vec::new(),
            completed_text_len: 0,
            full_text: String::new(),
            pending_blocks: Vec::new(),
            current_tail_block: None,
            parsed_len: 0,
        }
    }

    /// Text arrived since the last parse.
    pub fn needs_parse(&self) -> bool {
        self.parsed_len != self.full_text.len()
    }

    /// Append a streaming delta and re-parse the tail synchronously.
    /// Returns the row index of the tail block.
    pub fn append_delta(&mut self, delta: &str) -> usize {
        self.full_text.push_str(delta);
        self.reparse_tail();
        self.completed_blocks.len() + self.pending_blocks.len()
    }

    /// Replace the text (e.g. the persisted message arrived). Keeps the parse
    /// progress when the new text extends the old one; otherwise starts over.
    pub fn set_text(&mut self, text: &str) {
        if self.full_text.starts_with(text) {
            // Persisted fold lags the live stream: keep the live text.
        } else if text.starts_with(self.full_text.as_str()) {
            self.full_text.push_str(&text[self.full_text.len()..]);
        } else {
            *self = Self::new(std::mem::take(&mut self.message_id));
            self.full_text = text.to_string();
        }
    }

    /// Re-parse from the last completed block boundary. This is the expensive
    /// step; the UI runs it on a background task.
    pub fn reparse_tail(&mut self) {
        self.parsed_len = self.full_text.len();
        let tail = &self.full_text[self.completed_text_len..];
        let parsed = parse_top_level(tail);
        self.pending_blocks.clear();
        if parsed.is_empty() {
            self.current_tail_block = None;
            return;
        }
        // Blocks before the last are final, except a table no blank line
        // has closed yet: its next row, half streamed, can parse as a
        // separate paragraph. Such a table and everything after it stay open.
        let mut commit = parsed.len() - 1;
        for ix in 0..parsed.len() - 1 {
            if let (range, TranscriptBlock::Table { .. }) = &parsed[ix] {
                let next = parsed[ix + 1].0.start;
                let from = range.end.saturating_sub(1).max(range.start).min(next);
                if tail[from..next].matches('\n').count() < 2 {
                    commit = ix;
                    break;
                }
            }
        }
        let base = self.completed_text_len;
        let mut open: Vec<TranscriptBlock> = Vec::new();
        let mut open_start = None;
        for (ix, (range, mut b)) in parsed.into_iter().enumerate() {
            if ix < commit {
                b.set_id(format!("{}#{}", self.message_id, self.completed_blocks.len()));
                self.completed_blocks.push(b);
            } else {
                open_start.get_or_insert(range.start);
                open.push(b);
            }
        }
        self.completed_text_len = base + open_start.unwrap_or(0);
        let first = self.completed_blocks.len();
        for (offset, b) in open.iter_mut().enumerate() {
            b.set_id(format!("{}#{}", self.message_id, first + offset));
        }
        self.current_tail_block = open.pop();
        self.pending_blocks = open;
    }

    /// Seal this message (turn complete / part sealed).
    pub fn seal(&mut self) {
        self.reparse_tail();
        self.completed_blocks.append(&mut self.pending_blocks);
        if let Some(tail) = self.current_tail_block.take() {
            self.completed_blocks.push(tail);
        }
        self.completed_text_len = self.full_text.len();
    }

    /// Get all current blocks in order.
    pub fn all_blocks(&self) -> Vec<TranscriptBlock> {
        let mut list = self.completed_blocks.clone();
        list.extend(self.pending_blocks.iter().cloned());
        if let Some(tail) = &self.current_tail_block {
            list.push(tail.clone());
        }
        list
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_urls_become_links() {
        let links = |text: &str| {
            let mut spans = Vec::new();
            autolink(text, &mut spans);
            spans.into_iter().map(|s| (text[s.range.clone()].to_string(), s.link.unwrap())).collect::<Vec<_>>()
        };
        let one = |u: &str| vec![(u.to_string(), u.to_string())];
        assert_eq!(links("see https://example.com/a?b=1."), one("https://example.com/a?b=1"));
        assert_eq!(links("(http://localhost:3000)"), one("http://localhost:3000"));
        assert_eq!(links("https://en.wikipedia.org/wiki/Rust_(language), ok"), one("https://en.wikipedia.org/wiki/Rust_(language)"));
        assert_eq!(links("a https://x.dev b http://y.io").len(), 2);
        assert!(links("xhttp://nope and https:// and plain http").is_empty());
        // Inside a markdown link or code it stays as parsed.
        let blocks = parse_markdown_blocks("m", "[docs](https://a.dev) and `https://b.dev` and https://c.dev");
        let TranscriptBlock::Paragraph { content, spans, .. } = &blocks[0] else { panic!("{:?}", blocks[0]) };
        let linked: Vec<_> = spans.iter().filter_map(|s| s.link.as_deref().map(|l| (&content[s.range.clone()], l))).collect();
        assert_eq!(linked, vec![("docs", "https://a.dev"), ("https://c.dev", "https://c.dev")]);
    }

    #[test]
    fn code_block_knows_when_its_fence_closed() {
        let closed = |text: &str| match &parse_markdown_blocks("m", text)[0] {
            TranscriptBlock::Code { closed, .. } => *closed,
            other => panic!("{other:?}"),
        };
        assert!(closed("```mermaid\ngraph TD\n  A --> B\n```"));
        assert!(closed("~~~mermaid\ngraph TD\n~~~\n\nafter"));
        assert!(!closed("```mermaid\ngraph TD\n  A --> B"), "still streaming");
        assert!(!closed("```mermaid"));
    }

    #[test]
    fn local_markdown_images_become_shot_rows() {
        let text = "Here it is:\n\n![Google](/Users/k/screenshots/a.png)\n\n![b](file:///tmp/my%20shot.png) ![c](~/x.jpg)\n\nSee ![inline](/tmp/i.png) above.\n\n![web](https://e.com/x.png)";
        let blocks = parse_markdown_blocks("m", text);
        assert!(matches!(&blocks[0], TranscriptBlock::Paragraph { content, .. } if content == "Here it is:"));
        assert_eq!(blocks[1], TranscriptBlock::Shots { id: "m#1".into(), paths: vec!["/Users/k/screenshots/a.png".into()] });
        let home = std::env::var("HOME").unwrap();
        assert_eq!(
            blocks[2],
            TranscriptBlock::Shots { id: "m#2".into(), paths: vec!["/tmp/my shot.png".into(), format!("{home}/x.jpg")] }
        );
        // Mixed with text: a paragraph whose alt text links to the file.
        let TranscriptBlock::Paragraph { content, spans, .. } = &blocks[3] else { panic!("{:?}", blocks[3]) };
        assert_eq!(content, "See inline above.");
        assert!(spans.iter().any(|s| s.link.as_deref() == Some("file:///tmp/i.png")));
        // Remote images are not local files: a plain paragraph.
        assert!(matches!(&blocks[4], TranscriptBlock::Paragraph { .. }));
        assert_eq!(blocks.len(), 5);
    }

    #[test]
    fn parse_markdown_produces_stable_block_ids() {
        let text = "# Heading 1\n\nThis is paragraph one.\n\n```rust\nfn main() {}\n```\n\nAnother paragraph.";
        let blocks = parse_markdown_blocks("m1", text);
        assert_eq!(blocks.len(), 4);
        assert_eq!(blocks[0].id(), "m1#0");
        assert_eq!(blocks[1].id(), "m1#1");
        assert_eq!(blocks[2].id(), "m1#2");
        assert_eq!(blocks[3].id(), "m1#3");

        if let TranscriptBlock::Code { line_count, language, .. } = &blocks[2] {
            assert_eq!(*line_count, 1);
            assert_eq!(language.as_deref(), Some("rust"));
        } else {
            panic!("Expected code block");
        }
    }

    #[test]
    fn code_block_height_independent_of_highlight() {
        let code = "line 1\nline 2\nline 3\nline 4";
        let blocks = parse_markdown_blocks("m_code", &format!("```\n{code}\n```"));
        assert_eq!(blocks.len(), 1);
        if let TranscriptBlock::Code { line_count, .. } = &blocks[0] {
            assert_eq!(*line_count, 4);
        } else {
            panic!("Expected code block");
        }
    }

    #[test]
    fn streaming_three_deltas_within_block_updates_same_row() {
        let mut state = MessageBlockState::new("msg_stream".to_string());
        assert_eq!(state.append_delta("Hello"), 0);
        assert_eq!(state.append_delta(", how are"), 0);
        assert_eq!(state.append_delta(" you doing?"), 0);
        assert_eq!(state.completed_blocks.len(), 0);
        assert_eq!(
            state.all_blocks(),
            vec![TranscriptBlock::Paragraph {
                id: "msg_stream#0".to_string(),
                content: "Hello, how are you doing?".to_string(),
                spans: Vec::new(),
            }]
        );
    }

    /// Streaming any split of the text must yield the same blocks as one full parse.
    fn assert_stream_matches_full(text: &str) {
        let full = parse_markdown_blocks("m", text);
        for step in 1..=text.len() {
            let mut state = MessageBlockState::new("m".into());
            let mut ix = 0;
            while ix < text.len() {
                let mut end = (ix + step).min(text.len());
                while !text.is_char_boundary(end) {
                    end += 1;
                }
                state.append_delta(&text[ix..end]);
                ix = end;
            }
            assert_eq!(state.all_blocks(), full, "step {step} for {text:?}");
        }
    }

    #[test]
    fn one_delta_with_paragraph_heading_paragraph_does_not_duplicate() {
        let mut state = MessageBlockState::new("m".into());
        state.append_delta("p1\n\n# H\np2");
        let blocks = state.all_blocks();
        assert_eq!(blocks.len(), 3, "{blocks:?}");
        assert!(matches!(&blocks[1], TranscriptBlock::Heading { content, .. } if content == "H"));
        assert_stream_matches_full("p1\n\n# H\np2");
    }

    #[test]
    fn code_fence_with_blank_lines_keeps_content() {
        let text = "intro\n\n```py\na = 1\n\n\nb = 2\n```\n\nafter";
        assert_stream_matches_full(text);
        let blocks = parse_markdown_blocks("m", text);
        match &blocks[1] {
            TranscriptBlock::Code { content, line_count, .. } => {
                assert_eq!(content, "a = 1\n\n\nb = 2");
                assert_eq!(*line_count, 4);
            }
            other => panic!("expected code, got {other:?}"),
        }
    }

    #[test]
    fn list_items_are_separate_with_markers() {
        let blocks = parse_markdown_blocks("m", "- a\n- b\n  - c\n\n3. x\n4. y");
        assert_eq!(blocks.len(), 2);
        let TranscriptBlock::List { items, .. } = &blocks[0] else { panic!() };
        let got: Vec<_> = items.iter().map(|i| (i.marker.as_str(), i.depth, i.text.as_str())).collect();
        assert_eq!(got, vec![("•", 0, "a"), ("•", 0, "b"), ("•", 1, "c")]);
        let TranscriptBlock::List { items, .. } = &blocks[1] else { panic!() };
        assert_eq!(items[0].marker, "3.");
        assert_eq!(items[1].marker, "4.");
        assert_stream_matches_full("- a\n- b\n\npara");
    }

    #[test]
    fn tables_parse_with_alignment_inline_styles_and_empty_header() {
        let text = "| | |\n|---|:-:|\n| MR | [!12](https://gitlab.com/a/b/-/merge_requests/12) |\n| Branch | `feat/x` |\n\nafter";
        let blocks = parse_markdown_blocks("m", text);
        assert_eq!(blocks.len(), 2, "{blocks:?}");
        let TranscriptBlock::Table { align, header, rows, .. } = &blocks[0] else { panic!("{blocks:?}") };
        assert_eq!(align, &[ColumnAlign::None, ColumnAlign::Center]);
        assert!(header.iter().all(|c| c.text.is_empty()));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0].text, "MR");
        assert_eq!(rows[0][1].text, "!12");
        assert_eq!(rows[0][1].spans[0].link.as_deref(), Some("https://gitlab.com/a/b/-/merge_requests/12"));
        assert_eq!(rows[1][1].text, "feat/x");
        assert!(rows[1][1].spans[0].style.code);
        assert_stream_matches_full(text);
    }

    #[test]
    fn inline_styles_become_spans_without_markers() {
        let blocks = parse_markdown_blocks("m", "Use **bold**, *it*, ~~old~~ and `code` [here](https://x.dev).");
        let TranscriptBlock::Paragraph { content, spans, .. } = &blocks[0] else { panic!() };
        assert_eq!(content, "Use bold, it, old and code here.");
        let styled: Vec<_> = spans.iter().map(|s| (&content[s.range.clone()], s.style, s.link.is_some())).collect();
        assert_eq!(styled[0], ("bold", InlineStyle { bold: true, ..Default::default() }, false));
        assert_eq!(styled[1], ("it", InlineStyle { italic: true, ..Default::default() }, false));
        assert_eq!(styled[2], ("old", InlineStyle { strike: true, ..Default::default() }, false));
        assert_eq!(styled[3], ("code", InlineStyle { code: true, ..Default::default() }, false));
        assert_eq!(styled[4], ("here", InlineStyle::default(), true));
    }

    #[test]
    fn quotes_rules_headings_and_tasks() {
        let blocks = parse_markdown_blocks("m", "> quoted **line**\n> next\n\n---\n\n###### small\n\n- [x] done\n- [ ] todo");
        assert!(matches!(&blocks[0], TranscriptBlock::Quote { content, spans, .. } if content == "quoted line\nnext" && spans.len() == 1));
        assert!(matches!(&blocks[1], TranscriptBlock::Rule { .. }));
        assert!(matches!(&blocks[2], TranscriptBlock::Heading { level: 6, content, .. } if content == "small"));
        let TranscriptBlock::List { items, .. } = &blocks[3] else { panic!() };
        assert_eq!((items[0].task, items[0].text.as_str()), (Some(true), "done"));
        assert_eq!(items[1].task, Some(false));
    }

    #[test]
    fn quoted_lists_footnotes_math_and_html() {
        let blocks = parse_markdown_blocks("m", "> 1. one\n> 2. two\n>    - sub\n\nline<br>next<!-- gone --> <Composer>\n\nSee[^a]. Costs $5 and $10, $x^2$.\n\n$$\na+b\n$$\n\n[^a]: The note.");
        let TranscriptBlock::Quote { content, .. } = &blocks[0] else { panic!() };
        assert_eq!(content, "1. one\n2. two\n  • sub");
        let TranscriptBlock::Paragraph { content, .. } = &blocks[1] else { panic!() };
        assert_eq!(content, "line\nnext <Composer>");
        let TranscriptBlock::Paragraph { content, spans, .. } = &blocks[2] else { panic!() };
        assert_eq!(content, "See[^a]. Costs $5 and $10, x^2.");
        assert!(spans.iter().any(|s| s.style.code && &content[s.range.clone()] == "x^2"));
        assert!(matches!(&blocks[3], TranscriptBlock::Paragraph { content, .. } if content == "a+b"));
        assert!(matches!(&blocks[4], TranscriptBlock::Paragraph { content, .. } if content == "[^a] The note."));
        assert_stream_matches_full("> 1. one\n> 2. two\n\nSee[^a] $x$.\n\n[^a]: note");
    }

    #[test]
    fn set_text_extends_or_resets() {
        let mut s = MessageBlockState::new("m".into());
        s.append_delta("hello");
        s.set_text("hello world\n\nnext");
        s.reparse_tail();
        assert_eq!(s.all_blocks(), parse_markdown_blocks("m", "hello world\n\nnext"));
        // A lagging persisted prefix does not roll the live text back.
        s.set_text("hello wor");
        assert_eq!(s.full_text, "hello world\n\nnext");
        s.set_text("different");
        s.reparse_tail();
        assert_eq!(s.all_blocks(), parse_markdown_blocks("m", "different"));
    }
}

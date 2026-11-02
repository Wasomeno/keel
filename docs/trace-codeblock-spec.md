# Trace, Code Block & AI Elements for the Keel chat: spec (2026-10-15)

Status: build-ready draft. Owner: Grok Bot (no @uiux). Code was read, not changed.
House style follows `docs/permission-card-spec.md` / `docs/dotify-ui-spec.md`: exact tokens, timestamp-driven motion, reduced-motion fallbacks.

---

## 0. Research summary (sources)

**shadcn/ui, Oct 2026** ([changelog](https://ui.shadcn.com/docs/changelog/2026-10-ai-elements), [Trace](https://ui.shadcn.com/docs/components/base/trace.md), [Code Block](https://ui.shadcn.com/docs/components/base/code-block.md), [AI Elements index](https://ui.shadcn.com/docs/ai-elements.md), [Subagent](https://ui.shadcn.com/docs/ai-elements/base/subagent.md))
- **One shape for every step.** Reasoning, tool calls, sources, plans and subagent runs are each a `TraceLine`: an icon, a one-line title (truncates) and an optional description (a count, a duration, `+48 −12`; doesn't truncate). There's also an optional trailing indicator icon (chevron, or ↗ for external links).
- **Status is `none | running | error | pending`.** The title tense carries the status, so it isn't conveyed by color alone: "Running pnpm build", "Ran pnpm install", "Failed to run pnpm lint", "Run pnpm test" (pending). The title **shimmers while running**, and the shimmer stops under reduced motion.
- **The line is inert by default.** Behaviour comes from its wrapper: a collapsible trigger (adds a chevron and `aria-expanded`), a link, or a dialog or sheet trigger. Rows **start closed**. A streaming row uses `open ?? running`: it opens while running and auto-closes when done, unless the user has toggled it, in which case the user's choice wins.
- **Nesting.** A summary expands into steps, and a step into its output. The "Whole Turn" pattern has 3 levels: "Worked for 2m 4s, 1 command failed" → "Ran 2 commands, 1 failed" → each command. The parent's title and status are derived from its children. `keepMounted` keeps the open state of nested rows.
- **Text between steps splits the Trace.** Consecutive step parts are grouped into one Trace, and prose sits between Trace lists (`groupParts`).
- **Commit pattern.** A collapsible row with one line per file ("Edited x", `+a −d` in mono). Each file opens its diff in a Sheet.
- **CodeBlock** has a header (title with icon, actions: wrap toggle, copy, download), floating actions on hover or focus when there's no header, line numbers (not copied), `startLine`, wrap (off by default, on for tool output), line variants `added/removed/highlighted` (+/− markers, so meaning doesn't depend on tint), a height cap with an edge fade, a "Show more" footer, `variant=destructive`, and **`size=sm` for output inside a Trace** (tighter padding, xs text, `max-h-64` = 256px, wrap on). For streaming it tokenizes only the new lines, and copy stays disabled until the fence closes.
- **The 21 patterns.** Chain of Thought, Chat, Checkpoint, Confirmation, Context, File Tree, Inline Citation, Message, Model Selector, Plan, Prompt Input, Queue, Reasoning, Sources, Stack Trace, Subagent, Suggestion, Task, Terminal, Test Results, Tool.
- **Subagent.** "Running the research agent" while it runs, with its own steps nested live. When done: "Ran …" plus a duration description. The content shows the task, model badges, the nested Trace and the result.

**ACP** ([tool-calls](https://agentclientprotocol.com/protocol/tool-calls))
- `tool_call` carries `kind`: `read | edit | delete | move | search | execute | think | fetch | switch_mode | other`. The spec says kinds exist to "help Clients choose appropriate icons".
- `status` is `pending` (input streaming or awaiting approval) → `in_progress` → `completed | failed`.
- Content types are `content` (text/image), `diff` (`path, oldText, newText`) and `terminal` (live output). `locations` lists the files touched.

**Cursor** ([forum: "Explored N tools"](https://forum.cursor.com/t/new-version-hides-agent-tool-call-details-in-defiance-of-setting/165292), [elicitation hidden in collapsed rows](https://forum.cursor.com/t/mcp-elicitation-allow-deny-is-often-not-visible-unless-mcp-tool-row-is-expanded/172829/7), [release notes](https://prod.cursor.com/docs/release-notes/ide))
- Reads, searches, fetches and MCP calls fold into "Exploring · N tools" / "Explored N tools". Edits and terminal commands can sit inline at the top level ("Conversation Density: Detailed"). Finished turns collapse into "Worked for Ns".
- **Lesson:** users complained when everything was buried. Cursor had to surface approvals on collapsed subagent cards ("Approve … from collapsed cards") because Allow/Deny was hidden inside a collapsed group. Keel rule: **anything needing the user (an approval or a question) is never folded away.**

**Zed agent panel** ([docs](https://zed.dev/docs/ai/agent-panel); Zed is gpui + ACP like Keel)
- Responses stream with per-tool indicators.
- Edited files are summarised in an accordion above the editor (files, counts, lines), with hunk-level review.
- There are checkpoints per edit message and keyboard thread navigation (arrows, PgUp/PgDn, jump between prompts).

**Not separately verified in this pass:** Claude Code/claude.ai, ChatGPT/Codex and v0 internals. I make no claims about them here.

---

## 1. Where Keel is today (read from code)

| Area | Current | File |
|---|---|---|
| Tool calls | One `ToolGroup` row per message. The header is "Running X" / "Ran X" / "Ran 3 tools" + names, with a spinner icon, h28, `FOLD_INSET` 8. Expanding shows each call's detail or its first output line. | `crates/ui/src/transcript.rs` `render_tool_group` (~2618), `tool_group_labels` |
| Browser tools | Pill chips ("Took screenshot ×2"), h24 | `render_browser_chips` |
| Reasoning | "Thought process" + one-line preview, expanding to a left rail (`border_l_1`, ml14) | `render_reasoning` (~2854) |
| Subagent | One indicator row; clicking opens the subagent session | `render_subagent` (~2716), `RowAction::OpenSubagent` |
| Fold motion | `fold_body`: `max_h` and opacity, open `SLOW` 200ms ease_out, close `BASE` 160ms ease_in; id `fold-{row}-{n}` per toggle; height estimated | `transcript.rs` ~2786–2850 |
| Code block | `bg t.code`, `border`, `RADIUS_MD`, header h28 with language + Run + Copy, syntax spans precomputed in the background | `transcript.rs` ~1784, `markdown.rs::code_block`, `syntax.rs` |
| Diagrams | Mermaid / diagram / chart routed from Code fences | `mermaid.rs`, `mermaid_viewer.rs`, `diagram` |
| Receipts | Permission receipts styled like tool rows (h28) | `render_receipt`, `approval.rs` |
| Shimmer | `motion::shimmer` = `with_animation(...).repeat()` gradient band, 1400ms (skeletons only) | `motion.rs:174` |
| Scroll | gpui `ListState`, `FollowMode::Tail` while following; wheel-up unsticks, within 40px re-sticks; `HeightMemo` | `transcript.rs:115–230`, `view/main_pane.rs:870–925` |
| ACP mapping | `tool_call` → `ToolStarted{name=title, detail}`; `tool_call_update` completed/failed → `ToolFinished{summary=first line, output_tail, images}`; spawn tools → `SubagentStarted/Done`; thought chunks → `Reasoning` | `crates/harness/src/acp.rs:193–330` |
| Gaps | **`kind` is dropped** (only used for approvals); `in_progress` content is ignored; `diff` and `terminal` content aren't modelled; **`plan` updates aren't handled**; `ToolEntry.state` is a string (`running/done/error`) with no pending or cancelled; there are no per-tool durations. | `acp.rs`, `markdown.rs:193 ToolEntry` |

Motion tokens available in `motion.rs`: FAST 120, BASE 160, SLOW 200, SHEET_OPEN 360, SHEET_CLOSE 300, SPRING 380, SPATIAL 420, SHIMMER 1400, PULSE 1600. Curves: `ease_out`, `ease_out_strong` (0.23,1,0.32,1), sheet `glide_curve` (0.32,0.72,0,1), `ease_in`. `approval.rs` also has out-quint and in-out.

Theme (`crates/theme/src/schema.rs`), dark / light:
- text `#EDEDEF` / `#18181B`; muted `#A1A1AA` / `#60606A`; faint `#71717A` / `#8E8E98`
- code `#1B1B1E` / `#F6F6F8`; surface `#1F1F22`; hover `#FFFFFF0D` / `#0000000A`; border `#FFFFFF14` / `#00000017`
- danger `#F2555A` / `#D12F36`; success `#3FB950` / `#1A7F37`; warning `#F0A63A` / `#A8650F`
- accent is a neutral (`#D4D4D8` dark)

---

## 2. Component model

### 2.1 Data (ui crate, no gpui)
New `crates/ui/src/trace.rs`:
```rust
pub enum StepStatus { Pending, Running, Done, Error, Cancelled }
pub enum StepKind { Read, Edit, Delete, Move, Search, Execute, Think, Fetch, SwitchMode, Browser, Subagent, Plan, Sources, Other }
pub struct TraceStep {
    pub id: String,            // tool_call_id / "{msg}-thought" / plan id
    pub kind: StepKind,
    pub status: StepStatus,
    pub verb: Verb,            // present/past/failed/imperative forms, see 2.4
    pub subject: String,       // path, command, query, URL (was ToolEntry.detail)
    pub description: Option<String>, // "14s", "+48 −12", "8 results"
    pub started: Option<Instant>, pub ended: Option<Instant>,
    pub body: StepBody,
    pub children: Vec<TraceStep>,    // subagent steps, grouped calls
    pub needs_user: bool,            // pending approval/question: never folded
}
pub enum StepBody { None, Text(String) /*reasoning*/, Output{ tail: String, error: bool }, Diff{ path, old, new }, Terminal{ id }, Plan(Vec<PlanEntry>), Links(Vec<Link>), Shots(Vec<String>) }
```
`TraceStep::from(ToolEntry)` keeps the old sessions rendering. Persistence keeps the existing `tools` JSON, with additional optional fields `kind`, `started_ms`, `ended_ms`, `diff` (serde default).

### 2.2 Elements (gpui)
- **`TraceLine`**: a row, not focusable by itself.
- **`TraceRow`**: a TraceLine plus optional expandable body. It's the only interactive wrapper: it toggles, links, or opens the side panel.
- **`TraceGroup`**: consecutive steps between two prose blocks (shadcn's `groupParts`). It replaces `TranscriptBlock::ToolGroup`. With 3 or more finished non-edit, non-execute steps it folds into a summary line (see 2.5).
- **`TurnSummary`**: after a turn ends, a single top line, "Worked for 1m 12s · 1 failed", over all its groups. It's collapsed for past turns and absent while the turn is live (Cursor's pattern, but the latest turn is never collapsed).
- **`CodeBlock { size: Full | Compact, variant: Default | Destructive }`**: the full block replaces the markdown fence renderer; the compact one is used inside rows.

### 2.3 ACP update → line

| ACP / AgentEvent | Line | Icon | Body |
|---|---|---|---|
| `agent_thought_chunk` (Reasoning) | "Thinking" → "Thought" + duration | Brain | Text, muted, wrapped, rail |
| `tool_call` kind `read` | "Reading {path}" / "Read {path}" | FileText | Output (compact) or none |
| `search` | "Searching for {q}" / "Searched for {q}" + "N results" | Search | Output |
| `execute` | "Running {cmd}" / "Ran {cmd}" + duration | Terminal | CodeBlock compact (`$ cmd` + tail), live if `terminal` content |
| `edit` / `delete` / `move` | "Editing {path}" / "Edited {path}" + `+a −d` | FilePen / Trash / Move | Diff (compact, diff lines) |
| `fetch` | "Fetching {host}" / "Fetched {host}" | Globe | Output |
| `think` | treated as Reasoning | Brain | Text |
| `switch_mode` | "Switched to {mode}" (static line, no body) | Mode | – |
| keel-browser tools | keep the chips (existing `browser_step`) or a line, see Open decision D3 | per `browser_step` | Shots |
| spawn tool (`SubagentStarted/Done`) | "Running {name} agent" / "Ran {name} agent" + duration | Agent | nested Trace of its steps + result text, and "Open session ↗" |
| `plan` (new; handle `sessionUpdate:"plan"`) | "Planning" / "Plan · 3 of 5 done" | ListChecks | entries as pending/running/done lines |
| `request_permission` pending | **no line**: the permission card stays inline and unfoldable; afterwards the receipt line becomes a TraceLine in the group (also fixes the open "receipt at bottom" bug, see Phase 3) | Shield | args |
| `status: failed` | "Failed to {verb} {subject}" | same, danger tint | CodeBlock compact destructive, **auto-open** |
| turn cancelled while a step is running | "Stopped {verb-ing} {subject}" | same, faint | – |
| unknown kind (`other`) | `name` humanised (title already from harness) | Tool | Output |

Kind fallback when a harness omits `kind`: infer it from `name` (`bash|shell|exec*` → Execute, `read*|view*|cat` → Read, `grep|glob|search*|find` → Search, `edit|write|apply_patch|str_replace*` → Edit, `web*|fetch` → Fetch). Put this table in `harness/src/lib.rs` next to `tool_detail`.

### 2.4 Copy rules
Each verb has four forms: imperative (pending), present participle (running), past (done), and "Failed to …" (error). "Stopped …" is used for cancelled. The subject is shown in mono only when it's a path or command. Titles are truncated with an ellipsis in the middle for paths (the filename is kept) and at the end for commands.

### 2.5 Grouping
- Edits and executes always show as top-level lines (as in Cursor's Detailed density, which users wanted).
- 3 or more consecutive Read/Search/Fetch steps fold into "Explored 5 files, 2 searches". It's a running line while any child runs, and its title names the live child: "Exploring · Reading src/lib.rs".
- An error in a folded group lifts the parent to error ("Explored 4 files · 1 failed") and auto-opens the group.
- `needs_user` steps break out of any fold.

---

## 3. States

| State | Icon | Title | Description | Body default |
|---|---|---|---|---|
| Pending | 13px hollow circle, `text_faint` | imperative, `text_faint` | – | closed |
| Running | kind icon at 100%, plus a 6px pulsing dot (see perf) | present tense, **shimmer**, `text_muted` | live elapsed "4s" (updated once per second, from 3s on) | **open while running if it has a streaming body** (reasoning, terminal); otherwise closed. `open ?? running` rule |
| Done | kind icon `text_faint` | past tense `text_muted` | duration / counts / diffstat, `text_faint` | closed, auto-closes 600ms after done unless the user toggled it |
| Error | kind icon `danger` | "Failed to …" `text` (full contrast) with the leading verb in `danger` | exit code or "1 failed" | **open**, destructive compact block |
| Cancelled | kind icon `text_faint` @60% | "Stopped …" `text_faint` | – | closed |

---

## 4. Layout tokens (all through `zpx`, at zoom 1.0)

**TraceLine**
- Height 26 (existing rows are 28; a group of lines reads tighter). Horizontal padding is `FOLD_INSET` 8, and the row sits at `ml(-8)` so the icon column aligns with the prose left edge (same as today).
- Gaps: icon 14 in a 16 box, then 8 to the title; title-to-description 6; description-to-chevron 6. The chevron is 12, sits right after the content (not pinned right) and is `text_faint`, rotating 0→90°.
- Title: `t_small` (12/16), weight MEDIUM for the verb and NORMAL for the subject. A mono subject uses `MONO` at 11.5.
- Description: `t_caption` (11/14) `text_faint`, tabular numerals; diffstat in mono with `+` success and `−` danger at 85%.
- Hover: `bg t.hover`, `RADIUS_MD`, title → `t.text`. Pressed: `t.selected`.
- Vertical spacing: lines stack with 0 gap inside a group. A group has 6 above and below when next to prose, and 2 between consecutive groups.

**Nested body (rail)**
- `ml(15)` (the icon center), `pl(12)`, `border_l_1` `t.border`, `py(4)`. This matches today's reasoning rail, which is now generalised.
- Nested lines use the same metrics. Up to 3 levels: turn summary → group → step. Below that, steps render flat.

**CodeBlock compact (in a row)**
- `bg t.code`, `border_1 t.border`, `RADIUS_MD`, `px 10 py 8`, `t_code` reduced to 11.5/16. Wrap on. Max height 256 with a 16px fade at the bottom edge (a gradient from `code`@0 to `code`@100), scrolling inside.
- No header. Floating Copy (22×22, top-right, 6 inset) appears on hover or focus.
- The first line is `$ {cmd}` with `$` in `text_faint` (same as the permission card args).
- The destructive variant: border `danger`@35%, bg `danger`@6% over code, and text `t.text`.
- Diff lines: added gets bg `success`@12% and a `+` gutter in success; removed gets `danger`@12% and `−`; a hunk header is `text_faint`. The gutter is 14 wide and isn't copied.

**CodeBlock full (markdown fence)**
- Same surface. Header h28 with `pl 12 pr 4`, a bottom border, title = language or filename in `t_caption` `text_faint`, and an optional file icon 12. Actions are 22×22 ghost buttons (gap 2): Wrap (toggled shows `t.text`), Run (existing), Copy (success flash, existing).
- Body: `px 12 py 10`, `t_code` 12.5/18.
- Line numbers are optional (on when the line count is over 8 or a `startLine` is known). The gutter is `text_faint`@70%, right-aligned, its width fits the widest number, with 12 after. It's not copied.
- Cap at 480 collapsed, with a footer "Show N more lines" (h26, `t_caption`, centered, chevron) and a fade above it. Expanded caps at 70% of the viewport and scrolls.
- Diagram fences keep routing to mermaid/diagram renderers. Leave a header action slot "Open in viewer" for the next-phase diagram viewer.

---

## 5. Motion (timestamp-driven; one animation per element, ids keyed by `row_id` + toggle epoch, never nested)

All motion reads `elapsed = now - stamp` from a stored `Instant`, the pattern `approval.rs::phase()` already uses. Request a frame only while some stamp is inside its window.

| Element | Spec |
|---|---|
| Line entry (new step arrives) | opacity 0→1 + rise 4→0, **240ms** `ease_out_strong`. Several lines arriving together stagger by 30ms each, capped at 4 lines (120ms total). No height animation for the first line of a group; the list just lays out. |
| Expand | height 0→measured + opacity 0→1 (opacity on the first 60% of the time), **320ms** `glide_curve` (0.32,0.72,0,1). Content rises 4px. Chevron 0→90° over 200ms `ease_out`. |
| Collapse | height→0 **280ms** `ease_in_out` (0.65,0,0.35,1); opacity fades in the first 160ms; chevron back over 160ms. |
| Status change running→done | the icon crossfades over 160ms; the shimmer stops (fades 1→0 over 200ms, no snap); the title text swaps with a 120ms crossfade. The description slides in from 4px over 200ms. |
| →error | same crossfade, then the body auto-opens with the expand motion after an 80ms beat. |
| Auto-close after done | starts 600ms after done and uses the collapse motion. Skipped if the user toggled the row, or if the row isn't in view (it closes without animation off-screen). |
| Group fold (3rd read arrives) | the existing lines cross-fade into the summary line: 280ms, height via collapse curve. |
| Turn summary appear (turn end) | after 400ms idle, the groups above collapse under "Worked for …" with the collapse motion. This applies only to **past** turns when the next user message is sent, never to the turn the user is reading. |
| CodeBlock "Show more" | the cap animates 480→full over 360ms `glide_curve`. |
| Shimmer | a band 60% of the title width sweeps L→R over `SHIMMER` 1400ms linear, with 400ms rest (period 1800). Highlight is `text`@55% over `text_muted`. Rows' phases are offset by `(index*0.17).fract()`. |
| Reduced motion | no rise, height or stagger; 120ms opacity only; no shimmer (title static in `text_muted`); spinner → static dot. |

Height measurement: replace `fold_body`'s estimates (`lines*22+28`) with a measured height cached in `HeightMemo` per `(row_id, width)`. Until a measurement exists, use the estimate and then correct it within the same frame, so there's no visible jump (gpui `list` re-measures items each layout anyway).

---

## 6. Interactions & keyboard
- **Click a line** to toggle it. Click a subagent line's "↗" (trailing icon, appears on hover) to open its session (existing `OpenSubagent`). Click an edit line's path to open the file in the Files panel at the hunk. ⌘-click a path to reveal it in Finder.
- **Right-click** a line for Copy title, Copy output, Copy command, and Open in Terminal panel (execute only).
- **Keyboard** (transcript focused; this mirrors Zed's thread navigation):
  - `↑/↓` move a focus ring between lines. The ring is 1.5px `t.focus` and inset, shown only after keyboard use (same rule as the permission card).
  - `→`/`Space`/`Enter` open, `←` closes or moves to the parent.
  - `⌥↑/⌥↓` jump between turns.
  - `E` expands every line in the turn, `⇧E` collapses them.
  - `C` copies the focused output.
  - `Esc` returns focus to the composer.
- Each line has an accessible label of title + description, and an expanded state.
- **Never fold `needs_user`**: a pending approval stays as its card. If it's scrolled away, the existing "Needs approval" chip applies.
- Code block: Wrap toggle, Copy (disabled until `closed == true` while streaming, matching shadcn), and Show more. Text inside is selectable; line numbers and diff markers are excluded from copy.

## 7. Streaming & scroll anchoring
- Steps upsert by id. A text delta never re-creates a line, and a line's element id is stable for its life.
- **Following (Tail):** new lines and expanding bodies grow at the bottom and the list's Tail mode keeps the end in view. That's already how it works; no change.
- **Not following:** any height change *above* the viewport (an auto-close, a fold, the turn summary collapsing) must not move what the user is reading. Keep the anchor at `logical_scroll_top` (item_ix + offset). When an item above the anchor changes height by Δ, adjust `offset_in_item` (or the item index) to compensate, in the same frame. This is shadcn's "loads history without jumping". Add `TranscriptState::preserve_anchor(changed_ix, old_h, new_h)`.
- Auto-close and fold animations run **only for rows intersecting the viewport**. Off-screen rows snap, so there's never a long reflow far above the reader.
- User-initiated expand near the bottom while following: keep following, since the list grows down. User expand while not following: the line stays put and the body grows below it. If the body would end below the fold, don't scroll; this avoids surprise jumps.
- Live terminal output: append to `output_tail` and repaint only that block (`cx.notify` on the row's entity, if rows become entities; see Phase 4). The tail is coalesced to ≤ 10 updates per second.
- Reasoning while streaming: open with the rail, and the body follows its last line. Max height while running is 160 (about 8 lines) with a top fade, so it doesn't push the reply around. When done it auto-closes to "Thought for 6s".

## 8. Performance (keep CPU low)
- **The shimmer is the main risk.** `motion::shimmer` uses `.repeat()`, which requests a frame every vsync for as long as it's mounted. Rules:
  1. Shimmer **only** titles of `Running` lines that are **visible** (the gpui list renders visible items plus overdraw only; also gate on window active and visible). Window inactive or occluded means a static title.
  2. **One clock for all shimmers.** No per-row animation: a single `TraceClock { epoch: Instant }` in `TranscriptState`, with each row computing `phase = ((now - epoch).as_secs_f32() / 1.8 + row_phase).fract()` in render, painting a gradient-filled `div` behind text clipped with `overflow_hidden`. One `request_animation_frame` per frame while `any_visible_running`.
  3. **Throttle to 30fps.** A shimmer at 30fps is visually identical. Use `cx.background_executor().timer(33ms)` → `cx.notify()` instead of rAF when only shimmers are animating; switch to rAF only while a height or opacity transition is active.
  4. Cap at **3** shimmering lines; the rest show the static running dot.
  5. Implementation note: gpui can't clip a gradient to glyphs. The "shimmer" is a soft highlight band painted *over* the title with low alpha (`text`@18% band on the row background). That's cheaper than per-glyph color runs and reads as the same effect. Alternative (D1): two `StyledText` runs whose highlight range moves, which costs a text re-layout per frame, so don't.
- Spinners: replace the rotating `Spinner` on lines with the static kind icon + a 6px status dot pulsing on the shared clock. That's one less transform animation per row. Keep the spinner only on the turn's main loader.
- Elapsed "4s" labels update on a 1s timer, not per frame.
- Syntax highlighting stays on the background parse (`markdown.rs`). For streaming fences, highlight only from the last completed line (shadcn's approach); `syntax::Spans` gets an `extend_from(line)` path.
- Diff bodies are computed lazily on first expand (the `similar` crate, or the line diff already used by `file_changes.rs`), then cached by `(tool_id)`.
- Compact block output is capped at `OUTPUT_TAIL` (existing) and 400 rendered lines; beyond that, "Show full output" opens it in the Files/Terminal panel.
- Budget: idle transcript 0% CPU. With one running line visible, ≤ 3% on M-series at 30fps. Measure with Instruments' Time Profiler, as in the optimization audit.

## 9. Pattern adoption

| Now (Phases 1–3) | Next (Phase 4) | Later / skip |
|---|---|---|
| Tool, Reasoning, Task (as TraceGroup), Terminal (compact live block), Subagent (nested, ties into the next-phase subagents indicator), Plan, Chain of Thought (turn summary), Code Block (full + compact), Confirmation (keep the permission card; receipt becomes a TraceLine) | Test Results (Keel already has `test_run.rs` cards; re-skin as a Trace), Stack Trace (parse failed output frames → clickable path:line), File Tree / Commit (re-skin `file_changes.rs`; per-file diff opens in the side panel), Sources (fetch/web tools → "Used N sources"), Queue (todo lists from plan/TodoWrite) | Checkpoint (needs engine snapshots), Context (usage meter exists), Inline Citation, Suggestion (`prompt_suggestion.rs` exists), Model Selector / Prompt Input / Chat / Message (Keel has its own) |

The **diagram viewer** gets a slot in two places: the full CodeBlock header action, and a `StepBody::Artifact{kind: Diagram}` so a tool that produces a diagram shows a line "Drew architecture diagram ↗" that opens the viewer (`mermaid_viewer.rs`).
The **subagents indicator** (next phase) reads from the same `TraceStep` tree: count the `Subagent` steps with `Running` status per session and show them in the header or sidebar dot.

## 10. Phased plan & touch points

**Phase 1: data and mapping (no visual change)**
- `crates/proto/src/event.rs`: add optional `kind: Option<String>` to `ToolStarted`; add `ToolProgress{tool_id, output_tail}` and `Plan{entries}` events; `ToolFinished.diff: Option<Diff>`.
- `crates/harness/src/acp.rs`: read `kind`; map `in_progress` content to `ToolProgress` (throttled); extract `diff` content; handle `sessionUpdate: "plan"`; record `cancelled` on turn cancel. Mirror this in `claude.rs`, `codex.rs`, `pi.rs`, `grok.rs`, `command_code.rs` and `antigravity.rs` wherever they synthesize tool events. Add `kind_from_name` in `harness/src/lib.rs`.
- `crates/engine/src/lib.rs` + `crates/doc/src/session.rs`: persist `kind`, start/end timestamps, `diff` and plan in the message `tools`/`parts` JSON (serde default for old sessions).
- `crates/ui/src/markdown.rs` (`ToolEntry`): add fields; new `crates/ui/src/trace.rs` with `TraceStep`, `StepStatus`, `StepKind`, verbs, grouping (pure, unit-tested like `tool_group_labels`).

**Phase 2: TraceLine, TraceGroup and compact CodeBlock**
- New `crates/ui/src/view/trace_view.rs`: `render_trace_line`, `render_trace_group`, rail body, compact code block.
- `transcript.rs`: `TranscriptBlock::ToolGroup` → `TranscriptBlock::Trace{ id, steps }` (keep the old variant decoding into it); `render_reasoning`, `render_tool_group` and `render_subagent` delegate to it; keep `render_browser_chips` behind D3.
- `motion.rs`: add `GLIDE_OPEN 320`, `GLIDE_CLOSE 280`, `ENTER 240`, `ease_in_out`, `TraceClock`, 30fps ticker; deprecate the `.repeat()` shimmer for lines.
- `transcript.rs` `fold_body` → measured heights via `HeightMemo`; `preserve_anchor`.
- `icons`: add FileText, Search, FilePen, Trash, Move, Brain, ListChecks and Agent if missing (`Icon` enum).

**Phase 3: states, streaming, permission receipt, turn summary**
- Auto-open on error, auto-close on done, the `open ?? running` rule (store `user_toggled: HashSet<row_id>` in `TranscriptState`).
- Live terminal body; streaming reasoning cap; elapsed ticker.
- Fix the receipt placement: render the receipt as a `TraceLine` at the position of its `tool_call_id` inside the group (not appended at the bottom). Touches `render_receipt`, `approval.rs` receipt flow and `view/mod.rs`.
- `TurnSummary` for past turns.
- Keyboard navigation: `view/main_pane.rs` key handlers + focus ring.

**Phase 4: full CodeBlock and pattern re-skins**
- The `transcript.rs` Code branch → `CodeBlock::Full` (line numbers, wrap toggle, show more, streaming copy-disable). `syntax.rs` incremental highlight.
- Diff body plus "open in Files panel" (`view/files_panel.rs`). Re-skin `test_run`, `file_changes` and Sources. Hooks for the subagents indicator and diagram viewer.

**Testing:** unit tests in `trace.rs` (grouping, verbs, kind inference, status transitions); `motion.rs` curve and phase tests (pattern exists); a transcript test asserting that anchor offsets are preserved when a row above collapses; fixture ACP streams from each harness in `crates/harness` tests. Visual check: screenshots in light and dark, reduced motion on and off.

## 11. Open decisions
- **D1 Shimmer technique:** an overlay highlight band (recommended, cheap) or true per-glyph gradient text via moving text runs (costlier).
- **D2 Auto-collapse past turns** into "Worked for …": on (Cursor-like, compact) or off (everything stays visible; users complained when Cursor buried steps). The recommendation is on for past turns only, with `E` to expand all.
- **D3 Browser chips:** keep the pill chips for keel-browser runs, or unify them into TraceLines with Shots bodies. The recommendation is to unify, keeping "×2" folding as a description.
- **D4 Line height 26 vs today's 28:** denser, but it changes the established rhythm.
- **D5 Plan placement:** inline in the turn (shadcn) or pinned above the composer (Zed-style changes accordion). The recommendation is inline, plus a compact "3/5" in the header chip later.
- **D6 Harness coverage:** which harnesses actually send `kind`, `diff` and `plan` needs a quick live, read-only capture per harness before Phase 1 is finalised.

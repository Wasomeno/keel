# Keel memory

Curated markdown notes the agent writes for later sessions. Two scopes, both outside the user's repo. A short index is injected every turn. Topic files are read on demand. One Rust module owns the files. A generated Pi extension is the only way the model touches them.

This is option 1 from the memory research. It does not add a vector store, an extraction model call, or a second agent runtime.

## What this is for

A new Keel chat starts blank except for what Pi already loads: `AGENTS.md` / `CLAUDE.md`, skills, and the appended system prompt in `KEEL_SYSTEM_PROMPT`. Pi compaction keeps a long chat inside the context window, and the Loro document keeps the transcript. Neither of those is a note the agent can reuse in the next chat.

Memory is that note. A correction, a preference, or a decision that is not in the repo and not in `AGENTS.md`.

## What already exists and stays

| Piece | Where | Role after this work |
|---|---|---|
| Context files | Pi loads `AGENTS.md`, `CLAUDE.md`, `.pi/APPEND_SYSTEM.md` | Instructions a human writes. Not memory. |
| Compaction | Pi, automatic and `/compact` | Within one session. Memory is what survives a new session. |
| Transcript | Loro session doc, SQLite snapshot | The chat record. Not injected into later chats. |
| Journals | `journals/{chat_id}/{run_id}.jsonl` | Full tool IO. Not embedded and not scanned for memories. |
| Skills | `crates/engine/src/skills.rs`, Settings → Skills | Procedures. The Memory page follows this drill-in, it does not replace it. |
| Prompt sections | `crates/harness/src/pi.rs` sets `sections.keel_cards` and `sections.keel_project` on `before_agent_start` | Same hook for `sections.keel_memory`. |
| Permission gate | `permission_extension_source` in `pi.rs` | Must exempt the memory tools. See below. |
| `KEEL_*` env | Stripped from the child in `crates/harness/src/proc.rs` | Paths and the RPC token are baked into the extension file, the way the browser extension bakes its socket. |

Do not move the Bahasa Indonesia rules out of `KEEL_SYSTEM_PROMPT` in this work. A person can later save that preference as a user note. The prompt stays the default until then.

## Layout

Both trees live under Keel's data dir (`~/Library/Application Support/Keel/` in the app). Nothing is written into the project the chat has open.

```
{data_dir}/harnesses/keel/agent/memory/          user, every project
{data_dir}/harnesses/keel/projects/<id>/memory/  this checkout, this machine
{data_dir}/harnesses/keel/projects/<id>/root     one line, the identity path
```

`<id>` is the first 32 hex characters of SHA-256 over the identity path. The directory is created on the first write, not on the first read.

User memory sits next to Pi's agent folder (`KeelProfile::home` → `agent/`). `set_pi_agent_dir` already creates that folder.

### Which checkout

`crates/engine/src/skills.rs` walks parents until it sees a `.git` entry. That returns the worktree directory for a linked worktree, so two worktrees of one repo would not share notes. Memory uses a stronger identity:

1. From the chat's `cwd`, run `git rev-parse --path-format=absolute --git-common-dir`.
2. Canonicalize that path. Every worktree of the repo shares it, so they share one memory directory.
3. If that command fails, the folder is not a git checkout. Canonicalize `cwd` and use that.
4. If the chat has no folder, there is no project scope. User memory still loads.

`crates/engine/src/repo.rs` already shells out to git this way. Reuse that helper rather than a second git invocation style.

On first use, write `root` with the identity path. If a directory exists and `root` names a different path, refuse to use it and return an error. That is a hash collision or a hand-edited folder. Do not merge two checkouts into one memory.

### Files in a scope

```
memory/
  MEMORY.md          index, injected every turn, capped
  <slug>.md          one topic, read on demand
  archive/<slug>.md  retired topics, never injected
  memory.lock        held only during a write
```

`MEMORY.md` shape:

```markdown
# Memory

- [pnpm](pnpm.md) — This repo uses pnpm, not npm.
- [reply-tone](reply-tone.md) — Reply in casual Bahasa Indonesia unless the user is formal.
```

A bullet is one line matching:

```text
- [<slug>](<slug>.md) — <description>
```

The separator is space, em dash, space. The description is one line, at most 200 characters, and contains no newline. Other lines in the file are kept at the top as the header and ignored as index entries. The parser does not accept a bullet whose link target is not `<slug>.md`.

A topic file:

```markdown
---
type: project
description: This repo uses pnpm, not npm.
---

Use pnpm for install and scripts. The lockfile is pnpm-lock.yaml.
```

`type` is one of `user`, `feedback`, `project`, `reference`.

| Type | What belongs there |
|---|---|
| `user` | Who the user is and how they want replies, across projects. |
| `feedback` | A correction the user gave, or an approach they confirmed. |
| `project` | A fact about this checkout that the code and `AGENTS.md` do not state. |
| `reference` | Where something lives outside the repo: a tracker, a dashboard, a doc. |

The description in the frontmatter and the description on the index line are the same string. The body is optional and at most 32 KiB.

Slug: `^[a-z0-9][a-z0-9-]{0,63}$`. Reject empty, uppercase, dots, slashes, `..`, and the names `memory`, `archive`, and `MEMORY`. The model picks the slug. The tool does not invent one from the description.

## Caps

Per scope, the injected index is the first 200 lines of `MEMORY.md` or the first 25_000 bytes, whichever comes first. The cut is on a line boundary. Topic files do not count toward this cap. They are not injected.

A write that pushes the index over the cap still saves. The tool result says `over_cap: true` and tells the model to merge or archive until the index fits, because the next session drops the tail. This matches Claude Code's behavior and avoids a failed save that leaves the model retrying blindly.

The injected block also says, when a cut happened, that the index was truncated.

## Injection

On every `before_agent_start`, set `event.systemPromptOptions.sections.keel_memory`. Pi rebuilds sections each turn and treats a missing key as a removal, which is why `keel_cards` and `keel_project` assign the section on every turn. Do the same. Do not return a replacement `systemPrompt`. That drops Pi's prompt cache for the whole prompt. A section is patched when it changes.

The block is built in Rust (`memory_prompt`) so the cap and the wording live in one place. Shape:

```text
Keel memory is notes you write for a later session. It is not a transcript.

Save a correction, a preference, or a decision that is not in the repo and not in AGENTS.md.
Do not save file layout, architecture, or commands you could read from the code.
Types: user, feedback, project, reference.
User notes apply to every project. Project notes apply to this checkout.
Read a topic with memory_read before you rely on it. Write with memory_write.
If a write reports the index is over the cap, merge or archive lines until it fits.

## User
<capped MEMORY.md, or "None yet.">

## Project
<capped MEMORY.md, or "None yet.", or "This chat has no folder.">
```

An empty scope still gets the preamble, so the model knows the tools exist before the first note.

The section is part of the system prompt, so Pi compaction does not drop it. Do not also inject the index as a user message. That would copy it into the transcript on every turn.

## Tools

Two tools, registered by the extension, implemented by RPC against the running engine. The extension does not parse or write the files itself.

### `memory_read`

| Param | Required | |
|---|---|---|
| `scope` | yes | `user` or `project` |
| `slug` | no | Omit to return the full index, including lines past the cap, plus the topic slugs. |

A slug returns that topic's type, description, and body. Missing file is an error string, not an empty success. `archive/` is not readable through this tool.

`readOnlyHint: true`. The permission gate already lets a tool through when `annotations.readOnlyHint === true`.

### `memory_write`

| Param | Required | |
|---|---|---|
| `scope` | yes | `user` or `project` |
| `action` | yes | `upsert` or `archive` |
| `slug` | yes | |
| `type` | upsert | one of the four types |
| `description` | upsert | one line, ≤ 200 characters |
| `body` | no | markdown, ≤ 32 KiB |

`upsert` writes the topic file and inserts or replaces that slug's index line. `archive` moves the topic to `archive/<slug>.md`, overwriting an older archive of the same slug, and removes the index line. Archiving a missing slug is an error.

`promptSnippet` is one line: "Read and update Keel's notes for later sessions."

`promptGuidelines` names the tool, which Pi requires:

- Use `memory_write` for a correction, preference, or decision that a later session cannot get from the repo or from AGENTS.md.
- Use `memory_read` before relying on a topic. The index line is only a pointer.
- Do not use `memory_write` for task progress in the current chat.

### Writes

Hold `memory.lock` with `create_new` and a short retry (about 20 tries, 10 ms apart). Two chats in one repo can write project memory at the same time. Release the lock on drop, including on panic via a guard.

Under the lock:

1. Reject the slug and the scope.
2. Resolve the scope directory. Canonicalize. If `MEMORY.md` or the target topic path is a symlink, or the canonical path is not inside the scope directory, refuse.
3. Write the topic to `<slug>.md.tmp` and the new index to `MEMORY.md.tmp` in the same directory.
4. Rename the topic into place, then the index. Rename is atomic on the same volume.
5. For `archive`, rename the topic into `archive/` and then rename the index.

A crash between the two renames can leave a topic file newer than its index line. The next upsert of that slug rewrites both. `memory_list` reports topic files that have no index line so Settings can show them instead of hiding them.

No `fs` calls from the TypeScript except the unix-socket client.

## The extension

Generate `{data_dir}/harnesses/keel/extensions/keel-memory.ts` once the engine has a data dir and a token. Rewrite it only when the generated text changes (`write_extension` in `pi.rs` already does this compare). Mode `0600`. It contains the unix socket path (`{data_dir}/keel.sock`) and the engine token. `KEEL_*` never reaches the child, so baking them in is the same approach as `browser_extension_source`.

`folder_extensions` loads every `.ts` and `.js` directly in that directory, so the file is picked up with no new `--extension` flag. `KeelProfile::run_args` already creates the directory.

The file is small:

- `before_agent_start` calls `memory_prompt` with `event.systemPromptOptions.cwd` and assigns `sections.keel_memory`.
- `registerTool` for `memory_read` and `memory_write`. `execute` calls the matching RPC method and returns `{ content: [{ type: "text", text }] }`.
- One helper opens `net.createConnection(sockPath)`, writes a 4-byte big-endian length plus one JSON value, and reads one frame back. First request is `hello` with the baked token and the current proto version (`hello_request` in `crates/rpc/src/dispatch.rs`). Then the method call. Unix socket peers must present the token. An empty token is accepted only on the in-process duplex.

If `keel.sock` is missing or the call fails, the tool returns that error as text and the turn continues. Injection failure leaves the section as the static preamble plus "Memory is unavailable this turn." so the model does not invent notes.

Do not install `@josephakern/pi-memory` or any other Pi memory package. Those write under `~/.pi`, and Keel points `PI_CODING_AGENT_DIR` at its own agent folder.

### Permission gate

`permission_extension_source` confirms every tool that is not in `READ_ONLY` and does not advertise `readOnlyHint`. Under Auto-edit it also allows `edit` and `write`. `memory_write` would otherwise ask on every save, including in Auto-edit, which makes the feature feel broken.

Change the gate:

- `memory_read` relies on `readOnlyHint` and needs no special case, but add its name next to `read`, `grep`, `find`, and `ls` anyway so a future hint change does not start prompting.
- Allow `memory_write` in every permission mode, with a comment that the Rust handler refuses every path outside the two memory directories. The gate protects the user's repo. These tools never receive a repo path.

Full access already skips the extension.

## RPC

Add methods on the engine next to `list_skills` in `crates/engine/src/lib.rs`. They are local operations, same class as `list_skills` and `read_workspace_file`.

| Method | Params | Result |
|---|---|---|
| `memory_prompt` | `cwd` optional absolute directory | `{ text }` the section body |
| `memory_list` | `cwd` optional | `{ user: Scope, project: Scope or null }` |
| `memory_read` | `scope`, `slug` optional, `cwd` optional | index, or one topic |
| `memory_write` | `scope`, `action`, `slug`, `type`, `description`, `body`, `cwd` | `{ over_cap }` |
| `memory_delete` | `scope`, `slug`, `cwd` | `{}` |

`memory_delete` removes the index line and the topic file. It does not touch `archive/`. Settings uses it for an explicit delete. The model does not get this method. The extension never calls it. Archive is the model's retire action, so a retired note can be inspected later.

`project` is `null` when `cwd` is missing or not an absolute directory. A relative path is `BadParams`.

`Scope` is `{ id, root, notes: [{ slug, type, description, archived: false }], truncated }`. `root` is the identity path for a project and empty for the user scope. `truncated` is true when the index exceeds the inject cap. Unindexed topic files are included with an `unindexed` flag.

Implement the module as `crates/engine/src/memory.rs`. Keep parsing, the lock, the jail, and the cap there. The RPC methods are thin wrappers, as `rpc_list_skills` is a wrapper over `skills::list_skills`.

## Settings

Add a Memory section on the settings home, directly under the Skills section in `crates/ui/src/view/settings.rs`. Same row pattern as `render_skills_entry`: title, one-line blurb, count, chevron.

- Title: Memory
- Blurb: Notes the agent keeps across chats
- Count: number of user notes, for example "3". Empty is "None yet".

The drill-in replaces the settings column the way Skills does (`SkillsPage` is the pattern: `open`, `epoch`, `transition`, `load_gen`).

Contents:

- A two-way control, You and This project.
- You lists the user scope. It does not need a chat folder.
- This project uses the open chat's folder from `mention_root()`. No folder: a line that says to open a chat that has a folder. Do not fall back to the process working directory.
- Each row shows the description, the type, and the slug.
- Opening a row shows the body in the same kind of modal Skills uses for instructions, plus a way to edit the description and the body and save.
- Archive and Delete each ask for a confirm, then call `memory_write` with `archive` or `memory_delete`.
- Saving calls `memory_write` with `upsert` and the existing slug and type. The slug and type are not edited in v1. Changing them is archive plus a new note.
- A line at the top of a truncated scope says the index is over the cap and later chats will not see every line.
- Notes with no index line appear at the bottom, marked unindexed, and can be deleted.

Reload the list after a successful write. Match the `load_gen` ticket used by `load_skills` so a slow reply cannot overwrite a newer one.

No new composer affordance. The model writes notes. The person edits them in Settings.

## Module boundaries

```
crates/engine/src/memory.rs     format, paths, lock, jail, cap, inject text
crates/engine/src/lib.rs        five RPC methods
crates/harness/src/pi.rs        generate keel-memory.ts; exempt memory_write
crates/harness/src/keel.rs      no change to KEEL_SYSTEM_PROMPT
crates/ui/src/view/settings.rs  Memory section and drill-in
```

No proto types. Skills already cross the RPC as JSON objects parsed in the UI (`SkillRow::parse`). Do the same for notes.

No change to the Loro document, the journal, or the store schema.

## Tests

`memory.rs`, in-process, temp directories:

- Slug rejection: `../x`, `/tmp/x`, `Foo`, `a/b`, empty, `archive`, 64 characters after the first.
- Upsert creates the index line and the topic file. A second upsert replaces that line and does not duplicate it.
- Description with an em dash survives a round trip. The split is the first ` — ` only.
- Archive removes the bullet and leaves the body under `archive/`. A second archive of another slug does not drop the first archive file.
- Delete removes the bullet and the topic and leaves the archive.
- Cap at 200 lines, and cap at 25_000 bytes, both cut on a line boundary and set the truncated flag.
- `over_cap` is true after a write that crosses the cap, and the file is still saved.
- Inject text contains the preamble, both headings, and omits `archive/` content.
- A topic path that resolves outside the scope directory is refused. A symlink `MEMORY.md` is refused.
- Two sequential writes through the lock produce two intact index lines.
- Worktree identity: a repo and a linked worktree resolve to the same `<id>`. Two repos do not. A non-git folder uses its canonical path. Build the repo with `git init` and `git worktree add`, the same style as `repo.rs` tests.
- Missing `cwd` yields a user block and "This chat has no folder." for the project block.

Harness:

- Generated extension source contains `before_agent_start`, `keel_memory`, `memory_read`, `memory_write`, the socket path, and `readOnlyHint`.
- Permission source allows `memory_read` and `memory_write` without a confirm.

Engine:

- One RPC test: write a topic through `memory_write`, then `memory_list` and `memory_prompt` return it.

UI tests are not required for v1 if the settings code follows the Skills drill-in and the RPC test covers the data. Run the app once and walk the path below before calling it done. Keel is a GPUI app. There is no browser page to verify.

## Walkthrough

1. Launch Keel against a data dir.
2. Open Settings. Memory shows "None yet".
3. Start a chat in a git repo. Ask the agent to remember that this repo uses pnpm.
4. Confirm a `memory_write` tool call whose result is the saved line, and that the tool did not ask for a file-write approval.
5. Confirm the project `MEMORY.md` exists under the data dir and the repo's `git status` is clean.
6. Open Settings → Memory → This project. The pnpm line is there. Edit the body, save, reopen.
7. New chat, same repo. The first turn's context includes the pnpm line. A chat in a different repo does not include it.
8. A user-scoped note shows up in both repos.
9. Archive the project note. The next chat does not include it. The file is under `archive/`.

## Order of work

1. `memory.rs` and its unit tests, including git identity. No RPC yet.
2. The five engine methods, plus the one RPC test.
3. Extension generator and the permission-gate change, plus harness tests.
4. Settings section and drill-in.
5. The walkthrough above.

## Out of scope

- Embeddings, sqlite-vec, Mem0, Graphiti, Cognee, or a search tool. The model reads a topic by slug. Add `memory_search` with ripgrep only after a real scope no longer fits the index.
- A consolidation model pass at the end of a chat.
- Writing notes into the user's repo, or a committed `.keel/memory`.
- Sharing memory across machines. Keel is local-only.
- Editing slugs and types in the UI.
- An archive browser beyond the file sitting on disk.
- Subagent memory. Confirm during implementation whether Pi 1.3.0 child agents inherit `--extension`. If they do not, v1 still covers the main agent. Do not build a second injection path for subagents in this change.
- Recall across old transcripts. That is a different feature over the Loro documents.

# Keel optimization audit (2026-10-09, read-only)

> Note (2026-10-19): Keel is now local-only. `crates/sync` (`keel-sync`) became `crates/store` (`keel-store`) and the sync code it mentions was removed.

Scope: the running dev build (`target/debug/keel -v`, pid 93022, launchd `com.keel.dev`) plus a source review of `crates/*` and `apps/keel`.
No source was edited and Keel was not restarted. Clippy ran with `CARGO_TARGET_DIR=/tmp/keel-audit-target` (1.8 GB, safe to delete).
Times are WIB.

## 0. Baseline numbers (12:05–12:20 WIB)

| Metric | Value |
|---|---|
| Keel CPU, idle (window not frontmost; Claude was the front app) | 0.0–0.3 % (`top`), POWER 0.2–0.5 |
| Idle wakeups | about 4/s (IDLEW +15–23 per 5 s) |
| Threads | 16–20 |
| RSS / phys footprint | 20.8 MB / 70–71 MB (IOAccelerator 26 MB, IOSurface 12 MB dirty plus 196 MB reclaimable, Malloc 14 MB) |
| `sample 5s` while idle | every thread parked (cvwait/kevent/mach_msg); about 22 samples in tokio `Driver::turn`, 21 in gpui `flush_effects` |
| WebKit WebContent (the parked agent page) | **310 MB footprint, peak 659 MB**, 0 % CPU. GPU process 15 MB, Networking 16 MB |
| Machine | 8 GB class; 162 MB free, 3 GB in the compressor (memory pressure is real) |
| Debug binary | 88,053,832 B (`target/debug/keel`) |
| `target/` | **19 GB**: `debug/deps` 16 GB, of which **12.45 GB is 39,642 stale `*.rcgu.o`**; `incremental` 2.6 GB (45 dirs) |
| Free disk | 14 GiB |
| Data dir `~/Library/Application Support/Keel` | 42 MB: screenshots 26 MB (15 files, no cleanup), attachments 8.6 MB, journals 1.4 MB (38 chats, largest 380 KB jsonl holding a base64 image), `store.sqlite` 356 KB plus **WAL 5.7 MB**, `logs/keel.log` 538 KB |
| sqlite snapshots | 42 docs, 216 KB total. **All 42 rows share the same `updated_at`, so every commit rewrites every doc** |
| `keel.log` makeup | 2,403 of 5,055 lines are `loro_internal … Diagnosing EncodedBlock` at INFO (from `-v`); 7 `metal_renderer: scene too large … 14379 mono` |
| Harness children / zombies | none owned by Keel (2 system zombies belong to pid 710, not Keel) |
| `/tmp/keel-gui.{log,err}` | 0 B. Leftover `/tmp/keel-chip.log` is 848 KB |
| Backups `target/*_backup` | about 4.5 MB total (negligible) |
| Antigravity | `~/.gemini/antigravity-cli` 72 MB (conversations 38 MB, brain 21 MB), `~/.gemini/antigravity` 905 MB (not Keel's) |

Not measured: CPU while streaming. No run was active, and starting one would have meant driving the app.
How to get it: start a long turn, then `top -l 20 -s 1 -pid <pid> -stats cpu,idlew,power` and `sample <pid> 5 -file /tmp/keel-stream.txt`.

## 1. Architecture fact behind most UI findings

All UI lives in **one gpui view, `KeelWindow`** (sidebar, transcript, header, timer, hero). There are no `.cached()` views (`rg "\.cached\(" crates/ui` finds nothing).
gpui rebuilds the whole element tree of every uncached view each frame. Any repeating `with_animation` therefore re-runs `KeelWindow::render` at display rate (60/120 Hz). That covers the loader `dot_grid` (9 animations), the sidebar `pulse` for running chats, the `breathe` timer dot, the `spin` tool spinner, the `shimmer` skeletons, and `hero-drift`.
So any work done in `render` is paid once per frame while something animates.

## 2. Findings, ranked by impact/risk

### Quick safe wins

**Q1. Transcript items deep-cloned every frame.** `crates/ui/src/view/main_pane.rs:718`: `let items = self.transcript.items.clone();` (feeds `list(...)` at :754).
- Evidence: `TranscriptRowItem` holds owned `String`s, span `Vec`s, tool lists, and code text (`transcript.rs:70-79`, `markdown.rs:197-234`). The clone covers all rows, not just visible ones, and runs every frame while the loader animates.
- Change: keep `items: Rc<Vec<TranscriptRowItem>>`, using `Rc::make_mut` in `upsert_message`, or `Vec<Rc<TranscriptRowItem>>`. Clone the `Rc` in render.
- Gain: per-frame cost drops from O(transcript bytes) to O(1). For a 500 KB chat at 120 Hz that avoids about 60 MB/s of alloc+memcpy and thousands of mallocs per frame.
- Risk: low. Data and order are unchanged and only ownership changes; `list_state` splice/remeasure logic stays as it is.
- Verify: UI tests (101); `sample` during a long turn should show no `<TranscriptRowItem as Clone>::clone` under `render_transcript`.

**Q2. Hero screen redraws 14k glyphs every frame, plus stat() calls per frame.**
- Locations:
  - `main_pane.rs:345` `hero-drift` repeat animation.
  - `hero_ascii.rs:86-87`: `shape_line` and paint for each row, each frame.
  - `main_pane.rs:308` and `:81→265`: `hero_image_path()` makes up to 4 `is_file()` calls, twice per render.
- Evidence: `keel.log` shows `scene too large: … 14379 mono` (the glyph sprites). The drift is a 40 s loop, so the welcome screen never goes idle.
- Change, pick one:
  - (a) Drive the drift from a 15 fps `timer` instead of an animation frame. The position is already computed from `hero_since`, so the motion looks the same.
  - (b) Pause the drift when `!window.is_window_active()`.
  - (c) Cache `hero_image_path` in a field, refreshed only by `ensure_hero_ascii`'s background job or on settings change.
- Gain: on the welcome screen, roughly 8× fewer frames with (a), and 0 frames in the background with (b). About 1k stat/s removed.
- Risk: low. Visual cadence only; reduced motion already disables it.
- Verify: open the welcome screen and compare `top -pid` CPU before and after; the `scene too large` log line should stop.

**Q3. `publish_rows` does file-system and whole-turn text work on the UI thread, per delta.** `view/mod.rs:1007-1070`.
- Per call it:
  - lowercases every line of every message in the turn (`files.rs:102-111`);
  - runs `resolve_all`, i.e. `is_file`+`canonicalize` per candidate (`files.rs:137-160`);
  - reads `read_dir(data_dir/worktrees)` (`mod.rs:1074-1081`);
  - concatenates the full turn text for `mr_scan`.
- It runs on **every Reasoning delta** (`mod.rs:979-984`, no coalescing), every parse completion (`:1187`), and every message push (`:931`). That is O(turn) per update, so O(n²) over a turn, with syscalls.
- Change: compute `written`/`mr_text` only when a tool finishes, on `Done`, or when the turn is loaded. Cache the `Files` row per head id and reuse it otherwise. Coalesce reasoning through `mark_dirty`/`kick_parse` like text.
- Gain: large on long or reasoning-heavy turns; removes most UI-thread syscalls while streaming.
- Risk: low/medium. File chips would appear when the tool finishes or the turn ends rather than mid-sentence (they already appear only for existing files).
- Verify: the turn-row tests in `turn.rs`/`view` tests; check that chips still appear after a `Created foo.svg` answer.

**Q4. Engine snapshot every 5 s rewrites and compacts every loaded doc.**
- Locations: `crates/engine/src/lib.rs:1463-1471` calls `save(.., maintenance=true)`. `save` (`:3471`) runs `collapse_before` on every session, `delete_older(journals)` (a dir walk), and `compact_sessions` (`:3496`, a full `export_snapshot` of every session). Then `Store::commit` (`crates/sync/src/store.rs:179-207`) exports and UPSERTs every loaded session plus the registry.
- All of this runs under the global engine `Mutex`, so RPCs wait.
- Evidence: the identical `updated_at` on all 42 rows, the 5.7 MB WAL, and the 2,403 loro export log lines.
- Change:
  - Track dirty doc ids, or compare `oplog_vv()` with the last-committed version, and export only changed docs.
  - Run maintenance (collapse, compaction, journal cleanup) at most every 10 min or at idle, not on every 5 s flush.
  - Optionally `PRAGMA journal_size_limit=1048576`.
- Gain: per-flush export work goes from all docs ×2 to the changed doc ×1 (today about 216 KB → about 20 KB written per flush; much more as history grows). Shorter lock holds.
- Risk: low if the dirty set is also marked by sync imports and `replace_session`. Durability stays the same, since changed docs still flush every 5 s.
- Verify: engine tests (85); `sqlite3 'file:…store.sqlite?mode=ro' "select doc_id,updated_at from snapshots"` should show only the active chat's row advancing.

**Q5. `-v` turns on global `debug` logging.**
- Locations: `apps/keel/src/main.rs:298-305`; the plist runs `keel -v`.
- Evidence: 48 % of `keel.log` is loro INFO diagnostics; rustls/ureq debug lines too. Every `tracing::debug!` in hot paths gets evaluated and formatted.
- Change: make verbose `"debug,loro_internal=warn,loro=warn,rustls=info,ureq=info"`, or set `KEEL_LOG` in the plist.
- Risk: none (logging only). Verify: `keel.log` growth while streaming.

**Q6. `message_json` sends everything twice.**
- Location: `crates/engine/src/lib.rs:3413-3469`. `text`, `reasoning` and `tools` are repeated inside `parts`.
- It is serialized for every fold flush (120 ms) and every tool event (`push_message` `:2078`, called at `:1160/1206/1400/1531`), then parsed again in the UI (`mod.rs:910-932`).
- Oversize pushes are **silently dropped** (`crates/rpc/src/serve.rs:266-269`, cap 1 MiB at `rpc/src/lib.rs:44`). Because of the duplication, a message of about 500 KB stops updating.
- Change: drop the legacy `tools`/`reasoning` fields once the UI reads `parts` only (check `turn::doc_steps` and `ToolEntry::parse_list`). Longer term, push only the changed part.
- Gain: about 2× less JSON per push, and the frame-cap failure point doubles.
- Risk: medium-low. Any consumer still reading `tools`/`reasoning` (old transcripts, tests) must switch to `parts` first.

**Q7. Text-step parse jobs clone every step.**
- Location: `view/mod.rs:1152-1157` clones every `Step::Text` `MessageBlockState` (full text plus completed blocks) of a dirty message on each kick. `markdown.rs:687` `all_blocks` clones all blocks again.
- Change: send only the last (open) text step, or track dirty step indices.
- Risk: low. Verify: markdown tests.

**Q8. Screenshots kept forever and decoded at full size.**
- Locations: `view/browser_panel.rs:1320` saves `data_dir/screenshots/*.png`; there is no retention. Full-page shots are 4 MB PNGs at up to 1280×16000. `transcript.rs:778` `shot()` renders them through `gpui::img` at 360×225, which decodes the full bitmap (1280×5600 is about 28 MB RGBA) into the image cache.
- Change:
  - Delete screenshots older than `RETAIN_DAYS` (30) during the maintenance pass from Q4.
  - Save a ≤720 px-wide JPEG thumbnail next to each shot for the row; keep the original for click/lightbox.
- Risk: low. Missing files are already filtered (`markdown.rs:174`, `files::resolve`), so old rows just lose the image.

**Q9. Dev-profile hygiene (disk and link time).**
- Location: `Cargo.toml` has `[profile.dev.package."*"] opt-level=2` (good) but full debuginfo for all deps. The rlibs are huge: gpui 151 MB and objc2-app-kit 163 MB, each built twice (two feature/profile hashes).
- Change:
  - Add `[profile.dev.package."*"] debug = "line-tables-only"`, optionally also for `[profile.dev]`.
  - Periodically `cargo clean -p keel -p keel-ui -p keel-engine -p keel-harness -p keel-sync -p keel-rpc`, which removes the 12.45 GB of stale `*.rcgu.o` from split-debuginfo plus incremental churn.
  - Delete `/tmp/keel-audit-target` after this audit.
- Gain: many GB of disk; faster links; a smaller debug binary (estimated 88 MB → 40–50 MB).
- Risk: none to features; deleting files needs your OK.

**Q10. Small items.**
- Clippy perf findings are modest: 20 `redundant_clone`, 2 `needless_collect`, 2 `Iterator::last` on a double-ended iterator (`engine/lib.rs:3403`, `harness/pi.rs:2325`), and one dead test fn (`harness/codex.rs:1059`, plus a duplicated attribute at :1050). The 68 `clone_on_ref_ptr` hits are style only.
- **The `keel-sync` lib tests no longer compile**: `crates/sync/src/layout.rs:353` is missing the `hidden_harnesses` and `zoom` fields on `UiSettings`.
- `KEEL_DEBUG_CHIP` helper (`composer_view.rs:1675-1687`) is gated and cheap. It can go, along with `/tmp/keel-chip.log`.
- The engine `ticker` runs at 50 ms forever (`lib.rs:950`). Measured idle wakeups are only about 4/s, so this is low priority; it could skip work when `live.is_empty() && folds.is_empty()`.

### Bigger refactors

**R1. Split `KeelWindow` into child entities rendered with `.cached()`** (sidebar, header/timer pill, composer area, transcript, loader). An animation then repaints only its own view. This is the structural fix behind Q1/Q2; expect the biggest CPU drop while running. Risk: medium (focus and event plumbing).

**R2. Unload the parked agent webview after idle.** After N minutes with no agent op and the pane closed, `load_url("about:blank")` while keeping `web_url` (the restore path already exists, `browser_panel.rs:372`). Frees about 300 MB of WebContent. Risk: the page state is lost; the next agent op re-navigates (the restore code covers this).

**R3. Repo watcher.** `crates/engine/src/repo.rs:399-405, 429` watches every space **recursively** (including `node_modules`; `resona-finance-cms` is 3.9 GB). Any event sends `"all"`, which rescans **all** spaces (4–5 `git` spawns plus a full `git diff HEAD` patch each). The debounce at `:330-343` sleeps inside the loop, so a long build means continuous rescans.
- Note: no `profiles/local/patches/` directory exists, which suggests the watcher task may never have started (`RepoWatcher::new` spawns only if `Handle::try_current()` succeeds). Worth checking, because repo status may also not be refreshing.
- Fix: per-space keys, ignore `.git/objects`, `target`, `node_modules`, trailing-edge debounce, and only rescan the touched space.

**R4. `block_on_timeout` builds a new thread and a new tokio runtime per call** (`engine/lib.rs:3205-3232`; used for remote calls and checks). Reuse the engine runtime with `block_in_place` + `Handle::block_on` instead.

**R5. Release profile for daily use.** Add `[profile.release] codegen-units = 1` (keep `lto="thin"`, `strip`) and run the launchd agent from a release build. Workspace crates currently run at opt-level 0 (markdown, list layout, JSON).

**R6. Incremental message pushes** (part-level diffs instead of the whole message JSON). This also removes the 1 MiB push ceiling for long messages.

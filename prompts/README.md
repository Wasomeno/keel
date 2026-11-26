# Keel Prompts

All of Keel's agent prompts live in this folder and are embedded at compile time via `include_str!`. Editing any `.md` file here updates the prompt on the next build.

## Files

| File | Constant / Helper | When it applies | Where it is loaded |
| :--- | :--- | :--- | :--- |
| [`system.md`](system.md) | `KEEL_SYSTEM_PROMPT` | Every Keel run (replaces `keel-core`'s coding-agent identity while keeping its working rules; also passed to legacy Pi as `--append-system-prompt`) | `crates/harness/src/keel.rs`, `crates/harness/src/native.rs` |
| [`indonesian.md`](indonesian.md) | `INDONESIAN_SKILL` | Bundled as the `indonesian-natural` skill; read on demand when writing Bahasa Indonesia | `crates/harness/src/native.rs`, `crates/harness/src/pi.rs` |
| [`cards.md`](cards.md) | `UI_CARDS_PROMPT` | Every turn with UI card tools enabled | `crates/harness/src/lib.rs`, `crates/harness/src/native.rs` |
| [`memory.md`](memory.md) | `MEMORY_PREAMBLE` | Every session (`<keel_memory>`, pinned in the session header on start; changes on resume sent downstream as `<keel_memory_update>` to preserve KV cache) | `crates/harness/src/native.rs`, `crates/harness/src/pi.rs` |
| [`plan-mode.md`](plan-mode.md) | `PLAN_MODE_PROMPT` | Passed to `keel-core` via `with_plan_mode_prompt` when UI card tools are enabled (overrides the harness's default plan-mode text so only one prompt is sent) | `crates/harness/src/lib.rs`, `crates/harness/src/native.rs` |
| [`scratch.md`](scratch.md) | `SCRATCH_PROMPT` | Only when chatting in a no-project scratch folder | `crates/harness/src/lib.rs`, `crates/harness/src/native.rs` |
| [`project-roots.md`](project-roots.md) | `roots_prompt` | Only when a project is attached (`{project}` is replaced with the project name, followed by the root list) | `crates/proto/src/projects.rs`, `crates/harness/src/native.rs` |

## Verifying changes

```sh
cargo test -p keel-harness -p keel-engine -p keel-proto
```

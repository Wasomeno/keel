# Keel Prompts

All of Keel's agent prompts live in this folder and are embedded at compile time via `include_str!`. Editing any `.md` file here updates the prompt on the next build.

## Files

| File | Constant / Helper | When it applies | Where it is loaded |
| :--- | :--- | :--- | :--- |
| [`system.md`](system.md) | `KEEL_SYSTEM_PROMPT` | Every Keel run (passed to Pi as `--append-system-prompt`) | `crates/harness/src/keel.rs` |
| [`indonesian.md`](indonesian.md) | `INDONESIAN_SKILL` | Discovered via `resources_discover` as the `indonesian-natural` skill; read on demand when writing Bahasa Indonesia | `crates/harness/src/pi.rs` |
| [`cards.md`](cards.md) | `UI_CARDS_PROMPT` | Every turn with UI card tools enabled (`sections.keel_cards`) | `crates/harness/src/lib.rs` |
| [`memory.md`](memory.md) | `MEMORY_PREAMBLE` | Every turn (`sections.keel_memory`, pinned per session; mid-session updates sent downstream to preserve KV cache) | `crates/harness/src/pi.rs` |
| [`plan-mode.md`](plan-mode.md) | `PLAN_MODE_PROMPT` | Injected as a turn message when Plan mode is enabled (`keel-plan-mode`), preserving the cached system prefix | `crates/harness/src/lib.rs` |
| [`scratch.md`](scratch.md) | `SCRATCH_PROMPT` | Only when chatting in a no-project scratch folder (`sections.keel_scratch`) | `crates/harness/src/lib.rs` |
| [`project-roots.md`](project-roots.md) | `roots_prompt` | Only when a project is attached (`sections.keel_project`; `{project}` is replaced with the project name, followed by the root list) | `crates/proto/src/projects.rs` |

## Verifying changes

```sh
cargo test -p keel-harness -p keel-engine -p keel-proto
```

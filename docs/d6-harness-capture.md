# D6 harness capture

What each harness puts on the wire that Phase 1 could map onto a trace step: ACP `kind`, `diff` content, `plan` updates, in-progress tool output, terminal output, tool timing, and cancellation.

Read-only. No source files were changed. Live turns used the prompt `Read notes.txt and list the files here. Do not modify anything.` in a fresh scratch directory, with Keel's own handshake (`clientCapabilities.terminal: false`, no filesystem). Each process was killed within 90 seconds. Credential-shaped fields in `/tmp/keel-d6/<harness>.jsonl` are redacted. Command Code is not installed (`command-code`, `commandcode`, `cmdc`, and `cmd` are absent), so it has no live sample.

| Harness | Version | Live result |
|---|---|---|
| Grok | 1.0.52 | Completed. `read_file` and `list_dir`. |
| Claude Code | 2.1.280 | Stream opened. Turn ended `api_error` 402 before any `tool_use`. |
| Codex | 0.154.0 | App-server spoke. Turn `failed` (401) before any tool item. |
| Pi | 1.1.0 | Completed. `read` and `bash`. |
| Command Code | fixture initialize says 1.79.1 | CLI not installed. No tool fixture in the repo. |
| Antigravity ACP | 1.3.0 | Completed (`stopReason: end_turn`). Two `ls` runs and two `view_file` reads. |

Checked-in fixtures cover the cases a read-only prompt cannot: Grok bash streaming (`crates/harness/fixtures/grok/turn_v1.jsonl`) and an Antigravity edit with a diff (`crates/harness/fixtures/antigravity/turn_edit.jsonl`). Codex tool, diff, and plan shapes are the 0.154.0 schema in `crates/harness/fixtures/codex/ServerNotification.json`, the same CLI version as this machine. That schema was not exercised by a successful turn here.

## Summary

| Harness | kind | diff | plan | in-progress output | terminal | durations | cancel |
|---|---|---|---|---|---|---|---|
| Grok | Sends it, on a later frame than the tool start. Vendor kind `list` is not an ACP kind. | Not in the bash fixture or the live read. | Not sent. | Bash fixture streams `status: in_progress` text. Live reads do not. Argument text streams as `tool_call_delta_chunk`. | Inline text plus `rawOutput`. No `type: "terminal"`. | Turn `elapsed_ms` and `apiDurationMs`. Per-update `agentTimestampMs`. No per-tool duration. | Client sends `session/cancel`. No tool status `cancelled` on these samples. |
| Claude | No ACP kind. Init advertises tool names (`Read`, `Bash`, `Edit`, `Write`, `WebFetch`, `WebSearch`, `EnterPlanMode`). | Not observed. The 402 turn never sent `tool_use`. | Not an ACP plan. `EnterPlanMode` / `ExitPlanMode` are advertised tools. `TodoWrite` is not in the tool list. | Not observed. | Not observed. Result carries `terminal_reason`. | Turn `duration_ms` and `duration_api_ms` on `result`. | Capabilities include `interrupt_receipt_v1`. Receipt not captured. Driver sends `control_request` / `interrupt`. |
| Codex | `item.type` is the kind (`commandExecution`, `fileChange`, `webSearch`, …). Schema only; this turn had no tool item. | Schema: `changes[].diff` string, plus `turn/diff/updated`. Not in this turn. | Schema: `turn/plan/updated` steps, plus a `plan` item. Not in this turn. | Schema: `item/commandExecution/outputDelta`. Not in this turn. | Schema: `aggregatedOutput` and that delta. Not in this turn. | This failed turn sent `turn.durationMs` and `startedAt`. Item `durationMs` is schema only. | Client sends `turn/interrupt`. Status enum includes `interrupted`. This turn was `failed`. |
| Pi | No kind field. `toolName` is `read`, `bash`, `edit`, `write`, `web_search`, `fetch_content`. | Not sent. | Not sent. | `tool_execution_update.partialResult` for `bash`. The `read` had none. | `result.content[]` text. Bash also has `structuredContent.output` and `exit_code`. | `tool_execution_end.durationMs` (1.1.0). Message `durationMs`. Bash `wall_time_seconds`. | Client sends `{"type":"abort"}`. No cancel event on this turn. |
| Command Code | No sample. The driver feeds every `session/update` through the shared ACP translator. | No sample. | No sample. | No sample. | No sample. | No sample. `usage_update` is context size (`used`, `size`), which the driver does read. | Driver sends `session/cancel` (`command_code.rs:564–569`). |
| Antigravity | On the `tool_call` itself: `read`, `edit`, `execute`, `other`. | On the pending edit: `content[].type == "diff"` with `path`, `newText`, `_meta.kind: "add"`. The live read had none. | Not sent. | `status: "in_progress"` with no output body. Bytes arrive on the completed update. | `rawOutput.combinedOutput` / `formatted_output` at completion. No `type: "terminal"`. | Not sent. `usage_update` is `used` / `size` only. | Driver sends `session/cancel`. Live turn ended `end_turn`. Fixtures use tool status `failed`, not `cancelled`. |

## Shared ACP translator

Grok, Command Code, and Antigravity all call `translate_session_update` (`crates/harness/src/acp.rs:196`). The `Value` still holds every field. The translator reads only these:

- `tool_call`: `toolCallId`, `title` / `name`, `rawInput` / `input`, `locations`. `kind` is not read (`acp.rs:231–274`).
- `tool_call_update`: only when `status` is `completed` or `failed` (`acp.rs:282–287`). Output text comes from `content[].content.text` or `rawOutput` as a string or as `combinedOutput` / `formatted_output` / `output` / `stdout` (`acp.rs:358–381`). A `diff` block and a `terminal` block are not read.
- `kind` is read only while building a permission question (`acp.rs:487–497`).
- `sessionUpdate: "plan"` falls through the default arm, which only emits thought/text (`acp.rs:327–351`).
- No timestamp, duration, or cancel-status field is copied onto `ToolStarted` / `ToolFinished`.

Keel tells every ACP agent `terminal: false` (`grok.rs:120`, `command_code.rs:125`, `antigravity.rs:909`). An ACP `content.type: "terminal"` block refers to a terminal the client hosts. With that capability off, these agents inline the bytes instead.

## Grok

ACP over `grok agent stdio`. Protocol version answered: 1. The live tool start has no top-level `kind`. The vendor record and a Pascal-case shadow enum travel beside it:

```json
{"sessionUpdate":"tool_call","title":"read_file",
 "_meta":{"x.ai/tool":{"name":"read_file","kind":"read","read_only":true}}}
```

`params._meta.updateParams` on that same frame is `{"kind":"Other","status":"Pending"}`. That `Other` is not the ACP enum.

The ACP kind arrives on the next update, which has no `status`, so `translate_session_update` drops it:

```json
{"sessionUpdate":"tool_call_update","kind":"read",
 "title":"Read `<scratch>/notes.txt`","locations":[{"path":"<scratch>/notes.txt"}]}
```

`list_dir` is the same shape with a split kind: `_meta["x.ai/tool"].kind` is `"list"`, and the status-less update says `"kind":"other"`. Mapping `other` to Search would be wrong; the name `list_dir` (or the vendor kind `list`) is what says search.

The completed update drops `kind` again and carries the body. A read looks like `content: [{"type":"content","content":{"type":"text"}}]` plus `rawOutput.type: "ReadFile"`. `list_dir` completes with `rawOutput.type: "ListDir"` and no `content` array.

The checked-in bash turn (`turn_v1.jsonl` lines 13–16) is the streaming case. Line 13 sets `kind: "execute"` with a content blurb and still no terminal status. Lines 14–15 are `status: "in_progress"` with `content[].content.text` growing from `""` to `"hi\n"` and `rawOutput.output_for_prompt`. Line 16 is `status: "completed"` with the same text, `exit_code`, and `output_file` pointing at a `terminal/<toolCallId>.log`. There is no `type: "terminal"` block and no `diff`.

1.0.52 also streams arguments before the call exists, as `sessionUpdate: "tool_call_delta_chunk"` with `name` then `arguments_delta`. The translator has no arm for that, or for `turn_completed`.

Timing sits on the notification, not on the tool. Every tool frame's `params._meta` has `agentTimestampMs`, `streamStartMs`, and `turnStartMs` (live read spanned about 1792046957904–1792046957980). The turn ends with:

```json
{"sessionUpdate":"turn_completed","stop_reason":"end_turn","elapsed_ms":10581,
 "usage":{"apiDurationMs":10005}}
```

`prompt_usage` (`grok.rs:130–144`) keeps token counts and drops `apiDurationMs`. Hook runs carry their own `elapsed_ms`; that is not tool time.

`session/new` included `cancelRewind: true`. No `session/cancel` was sent, and no tool status was `cancelled`. The driver's interrupt is `session/cancel` (`grok.rs:519–524`). `parse_turn_status_from_stop_reason` maps `cancelled` to `TurnStatus::Interrupted` (`acp.rs:183`).

No `sessionUpdate: "plan"` and no `oldText` / `newText` in the live log or `turn_v1.jsonl`.

## Claude Code

Stream-json, the same flags as `ClaudeHarness::base` plus `--include-partial-messages` and `--no-session-persistence`. The process produced `system/init`, `hook_started`, `hook_response`, `status: requesting`, a replayed user message, one assistant text block (`API Error: 402 insufficient balance`), and:

```json
{"type":"result","subtype":"success","is_error":true,"stop_reason":"stop_sequence",
 "terminal_reason":"api_error","api_error_status":402,
 "duration_ms":1399,"duration_api_ms":0,"num_turns":1}
```

`claude.rs` never reads `duration_ms`. The fixture `crates/harness/fixtures/claude/not_logged_in.jsonl` is the same envelope (init, synthetic assistant text, `result` with both duration fields).

`system/init.tools` on this build is:

`Task`, `AskUserQuestion`, `Bash`, `CronCreate`, `CronDelete`, `CronList`, `Edit`, `EnterPlanMode`, `EnterWorktree`, `ExitPlanMode`, `ExitWorktree`, `ListAgents`, `NotebookEdit`, `Read`, `ReportFindings`, `ScheduleWakeup`, `SendMessage`, `Skill`, `TaskStop`, `WebFetch`, `WebSearch`, `Workflow`, `Write`.

`capabilities` includes `interrupt_receipt_v1` and `interrupt_cancel_queued_v1`. No interrupt was sent, so the receipt body was not captured. The driver's interrupt is `control_request` / `interrupt` (`claude.rs:423–428`).

What the adapter will accept once a tool actually runs, from `claude.rs:741–819` and the unit-test doubles around `claude.rs:1102` and `claude.rs:1109`:

- `assistant.message.content[]` of `type: "tool_use"` with `id`, `name`, `input`.
- A later `user.message.content[]` of `type: "tool_result"` with `tool_use_id`, `content` (string or `{type,text}[]`), `is_error`.
- `stream_event` deltas of `text_delta` and `thinking_delta` only (`claude.rs:681–709`). `input_json_delta` is ignored.

There is no `kind` field in that shape. Kind has to be inferred from `name` (`Read`, `Edit`, `Bash`, `WebFetch`, …). `EnterPlanMode` is the plan-shaped tool on this build. `TodoWrite` was not advertised. No `tool_use` was on the wire, so an Edit `input` diff (`old_string` / `new_string` or otherwise) was not observed.

## Codex

`codex app-server --listen stdio://`, then `thread/start` with `sandbox: "read-only"`, `approvalPolicy: "never"`, `ephemeral: true`, then `turn/start`. The server accepted the thread. The turn failed before a tool item:

```json
{"method":"turn/completed","params":{"turn":{
  "status":"failed","startedAt":1792046963,"completedAt":null,"durationMs":18322}}}
```

`item/started` and `item/completed` in this capture are `userMessage` only. The same shape is `crates/harness/fixtures/codex/turn_failed_401.jsonl`, which also puts `startedAtMs` on `item/started`. `thread_notification` (`codex.rs:814–954`) does not read `durationMs`, `startedAt`, or `startedAtMs`.

The 0.154.0 schema (`methods.json` records `cli_version: "codex-cli 0.154.0"`, matching this binary) is the evidence for the fields a successful turn can emit. They were not on this wire.

- Kind is `item.type`. Tools Keel already classifies (`codex.rs:721–732`): `commandExecution`, `fileChange`, `mcpToolCall`, `dynamicToolCall`, `collabAgentToolCall`, `webSearch`, `imageView`, `imageGeneration`. A `plan` item is a separate type and is not in that list, so `item/started` returns before it becomes a row (`codex.rs:845–849`).
- Diff is `fileChange.changes[]` as `FileUpdateChange`: required `path`, `diff` (string), `kind` (`add` / `delete` / `update`) at `ServerNotification.json:1685–1702`. The turn also has `turn/diff/updated` with a single unified `diff` string (`ServerNotification.json:6210–6228`). On `item/completed`, only `commandExecution.aggregatedOutput` and `mcpToolCall.result` are copied (`codex.rs:908–912`). `changes` is dropped.
- Plan is `turn/plan/updated`: `plan[]` of `{step, status}` with status `pending` / `inProgress` / `completed`, plus optional `explanation` (`ServerNotification.json:6312–6361` and `fixtures/codex/v2/TurnPlanUpdatedNotification.json`). There is also a `plan` thread item whose body is a `text` string (`ServerNotification.json:4637–4659`). Neither method is in the notification match.
- In-progress terminal output is `item/commandExecution/outputDelta` with `delta`, `itemId`, `threadId`, `turnId` (`ServerNotification.json:1025–1046` and the method at line 7453). `commandExecution` itself carries `aggregatedOutput`, `exitCode`, `durationMs`, `status` (`inProgress` / `completed` / `failed` / `declined`) at lines 4696–4794. No `cancelled` status.
- Turn timing is `Turn.startedAt`, `completedAt`, `durationMs` (`ServerNotification.json:6127–6186`). `TurnStatus` is `completed` / `interrupted` / `failed` / `inProgress` (line 6378).
- Cancel from Keel is `turn/interrupt` with `threadId` and `turnId` (`codex.rs:677–686`).

## Pi

`pi --mode rpc`, then `{"type":"prompt"}`. Pi 1.1.0 events for the two tools:

```json
{"type":"message_update","assistantMessageEvent":{"type":"toolcall_start","toolName":"read"}}
{"type":"message_update","assistantMessageEvent":{"type":"toolcall_delta","delta":"…"}}
{"type":"tool_execution_start","toolName":"read","args":{"path":"<scratch>/notes.txt"}}
{"type":"tool_execution_end","toolName":"read","durationMs":8,"isError":false,
 "result":{"content":[{"type":"text"}],"structuredContent":"<string>"}}
{"type":"tool_execution_update","toolName":"bash","partialResult":{"content":[]}}
{"type":"tool_execution_update","toolName":"bash",
 "partialResult":{"content":[{"type":"text","text":"<242 chars>"}],"details":{}}}
{"type":"tool_execution_end","toolName":"bash","durationMs":19,"isError":false,
 "result":{"content":[{"type":"text"}],
           "structuredContent":{"output":"…","exit_code":0,"truncated":false,"wall_time_seconds":0}}}
```

Assistant `message_end` also carries `durationMs` (2181 on the tool-using message, 3878 on the reply). The 1.0.4 fixture `crates/harness/fixtures/pi-1.0.4-turn.jsonl` has `tool_execution_update` and `wall_time_seconds` and does not have top-level `durationMs`, so that field is new in 1.1.0.

`parse_line` (`pi.rs:841–844`) emits `tool_execution_start` and `tool_execution_end` only. `tool_execution_update`, `toolcall_delta`, and `durationMs` are dropped. `tool_finished` (`pi.rs:1157–1201`) keeps `result.content` text and ignores `structuredContent` whenever that text is non-empty, which drops `exit_code` and `wall_time_seconds` for bash.

The tool catalogue advertised on this turn's messages includes `read`, `bash`, `edit`, `write`, `web_search`, `fetch_content`, `get_search_content`, `source_check`. No event was named plan, diff, or kind. Interrupt is `{"type":"abort"}` after `clear_queue` (`pi.rs:653–658`).

## Command Code

No binary on `PATH` and no turn fixture. `crates/harness/fixtures/command_code/initialize.json` is an ACP v1 `initialize` result for Command Code 1.79.1 (`loadSession`, prompt images, `sessionCapabilities.list/resume/close`). It contains no `tool_call`.

`command_code.rs` spawns `command-code acp` and uses the shared translator. The only session updates it handles itself are `usage_update` (`used`, `size`, `command_code.rs:498–501`) and `config_option_update`. A `kind`, `diff`, `plan`, or `in_progress` body would be subject to the drops in `acp.rs`. Whether 1.79 actually emits them is unmeasured.

## Antigravity

ACP v2 server `agy_acp_server.par` 1.3.0. The prompt response still carries `stopReason` (live: `end_turn`); there was no idle `state_update`.

Live execute, same `toolCallId` from pending through completion:

```json
{"sessionUpdate":"tool_call","toolCallId":"…","title":"ls -la",
 "kind":"execute","status":"pending","rawInput":{"CommandLine":"ls -la",…}}
{"sessionUpdate":"tool_call_update","kind":"execute","status":"in_progress","title":"ls -la"}
{"sessionUpdate":"tool_call_update","status":"completed","rawOutput":{
  "commandLine":"ls -la","workingDir":"…","exitCode":0,"exit_code":0,
  "combinedOutput":"<92 chars>","formatted_output":"<92 chars>"}}
```

The `in_progress` update has no `content` and no output. `tool_output_text` already picks up `combinedOutput` at completion.

Live reads:

```json
{"sessionUpdate":"tool_call","title":"Running view_file","kind":"read",
 "status":"in_progress","locations":[{"path":"<scratch>/notes.txt"}],
 "rawInput":{"AbsolutePath":"<scratch>/notes.txt"}}
{"sessionUpdate":"tool_call_update","toolCallId":"…","status":"completed"}
```

The completed read has no `rawOutput` and no `content`. The file body was not sent to the client. `OpenReads` in `antigravity.rs:564` already looks at `kind != "execute"` so it can close those reads when text arrives, and still does not forward `kind`.

The edit fixture is the diff sample (`turn_edit.jsonl` line 1), on the permission preview rather than on the later `edit_file` run:

```json
{"sessionUpdate":"tool_call","title":"Run create_file?","kind":"edit","status":"pending",
 "content":[{"type":"diff","path":"…/hello.txt","newText":"banana","_meta":{"kind":"add"}}],
 "locations":[{"path":"…/hello.txt"}],
 "rawInput":{"TargetFile":"…/hello.txt","CodeContent":"banana","Overwrite":true}}
```

No `oldText` in that add. The executing call is a different id, `kind: "edit"`, `status: "in_progress"`, `rawInput.file_path`, and its completion is `rawOutput: "Create file hello.txt"` with no diff (`turn_edit.jsonl` lines 3–5). `turn_subagent.jsonl` repeats `kind` values `other` (spawn), `read`, and `edit`, and its read completions are also empty.

No `sessionUpdate: "plan"`, no `type: "terminal"`, no `elapsed_ms`. `usage_update` is `{used, size}` and the shared translator ignores it (Command Code is the driver that reads it).

Tool statuses in the live log and the fixtures: `pending`, `in_progress`, `completed`, `failed`. The failed preview says `rawOutput: "Tool call was approved but never executed."` Cancel from Keel is `session/cancel`, and a server that ignores it is killed after 3 seconds (`antigravity.rs:1117–1123`, `CANCEL_WAIT`). The captured turn was not cancelled.

## What is safe to build on

- Antigravity `kind` is on the `tool_call` Keel already turns into `ToolStarted`. Grok's ACP `kind` is on a status-less `tool_call_update`, and the reliable name is `_meta["x.ai/tool"]` (`read`, `execute`, `list`). `list` needs its own mapping. Claude and Pi only have tool names. Codex kind is `item.type`, declared in schema, not seen on a live tool.
- The one captured diff is Antigravity's pending `content[].type == "diff"` (`path`, `newText`, `_meta.kind`). Codex `fileChange.changes[].diff` is a unified-diff string in the schema.
- Nobody sent ACP `sessionUpdate: "plan"` or `content.type: "terminal"`. Codex is the harness whose schema has a real plan (`turn/plan/updated`) and a real output stream (`item/commandExecution/outputDelta`). Claude's plan hook on 2.1.280 is the `EnterPlanMode` tool, which this turn did not call.
- Streaming tool text that Keel currently drops: Grok bash `in_progress` content (`turn_v1.jsonl`), Grok `tool_call_delta_chunk`, Pi `tool_execution_update`. Antigravity's `in_progress` frame has no bytes.
- Per-tool duration is on the wire for Pi 1.1.0 (`durationMs` on `tool_execution_end`). Grok, Claude, and Codex expose turn-level durations (`elapsed_ms` / `apiDurationMs`, `duration_ms`, `turn.durationMs`). Antigravity and Command Code do not, in the samples we have. Grok's `agentTimestampMs` can bracket a tool if both frames are kept.
- Cancellation is a turn stop (`stopReason: cancelled`, Codex `interrupted`, Pi `abort`), not a tool status. None of the live turns were cancelled, so a server's cancel payload was not recorded. Tool statuses actually seen are `pending`, `in_progress`, `completed`, `failed`, plus Codex schema `declined`.

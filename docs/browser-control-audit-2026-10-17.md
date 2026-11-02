# Keel browser pane control: audit and alternatives (2026-10-17)

This was a review only. No code was changed.

**Evidence used:**
- I read the code: apps/keel/src/mcp_browser.rs, crates/engine/src/browser.rs, crates/ui/src/browser/{mod,mac}.rs, crates/ui/src/view/browser_panel.rs, the harness drivers, and wry 0.56.1's WKUIDelegate.
- I ran the unit tests:
  - engine browser: 6/6 pass
  - ui browser: 10/10 pass
  - keel mcp_browser: 2/2 pass
  - harness browser: 3/3 pass
- I ran a micro-benchmark of the real `keel mcp-browser` shim against a fake socket. It did not touch the live pane.
- I mined timings from the local chat journals (profiles/local/journals/*.jsonl).
- I did web research; the sources are at the end.

## Verdict

**Keep MCP as the transport. The weak part is the toolset behind it.**

All six harnesses already take a client-supplied MCP server:
- Grok, Command Code and Antigravity get it through ACP `session/new` / `session/load` / `session/resume` `mcpServers`.
- Claude gets it through `--mcp-config`.
- Codex gets it through the `mcp_servers.keel-browser` thread config override.
- Pi gets it through a generated extension that calls `pi.registerMcpServer`.

ACP v2 has also removed the client terminal and fs methods. The spec now says that client-side tools should be exposed as MCP servers in `mcpServers`, so MCP is the direction the ecosystem is moving.

No alternative gives better harness coverage:
- A CLI covers the same harnesses.
- CDP and BiDi are not available on WKWebView.
- CEF is a large rewrite.

What actually limits agents today is what the tools do:
- an innerText-only snapshot with no element refs;
- synthetic events;
- no waits, dialogs, console or network access;
- one global agent tab shared by every chat.

These are the same problems Playwright MCP, agent-browser, chrome-devtools-mcp and Cursor's browser each solved with accessibility-tree refs.

## Current architecture

```
agent CLI ──stdio JSON-RPC (MCP 2024-11-05)──> `keel mcp-browser` (shim, per agent session)
   └─ one Unix-socket connection per tool call ──> {data_dir}/browser.sock (0600, NDJSON)
        └─ engine BrowserHost: request_id → pending map, 20 s REQUEST_TIMEOUT
             └─ SubscribeEvent::BrowserRequest to the *last subscribed* UI
                  └─ browser_panel.rs run_host_request → agent tab's NativePage (wry WKWebView)
                       └─ evaluate_script_with_callback / takeSnapshotWithConfiguration
                  └─ client.browser_complete → engine → shim → agent
```

There are seven tools: open_pane, navigate, snapshot, click (selector or x/y), type (text/selector/submit), scroll, and screenshot (viewport, or a full page stitched and capped at 16,000 CSS px). Full-page screenshots reach the model as a downscaled JPEG, and the full PNG is saved to disk.

## Measurements

| What | Result |
|---|---|
| Shim spawn plus `initialize` | ~50 ms, once per agent session |
| Shim plus socket overhead per call (fake host) | p50 0.07 ms for click, ~1 ms for a 48 KB snapshot |
| Real calls (journals): navigate | n=10, p50 789 ms, p90 5.6 s |
| Real calls: screenshot | n=6, p50 332 ms, max 490 ms |
| Real calls: snapshot | n=3, p50 251 ms, **one hit the 20 s timeout** ("browser request timed out") |
| Real calls: type / scroll / open_pane | 314 ms / 208 ms / 107 ms (p50) |
| Tool schema cost (`tools/list`) | ~3.5 KB, about 870 tokens per session |
| Snapshot at the 50k-char cap | about 12k tokens per call, with no structure and no refs |
| Viewport screenshot, 1280×800 | about 1–1.6k image tokens. The full PNG files on disk reach 4.5 MB (1280×5600). |

Transport latency does not matter. Nearly all the time goes to page load and to the UI thread hop through `cx.defer`. The rare multi-second stalls come from navigate's 12 s load wait and the 20 s host timeout.

One real failure in the journals is a good example of the main gap: `browser_click` failed with `no element matches [aria-label*='20 October'], [aria-label*='October 2…`. The agent had to guess a CSS selector because the snapshot gave it nothing to point at.

## Findings

1. **The snapshot has no element refs, so the agent has to guess selectors (high impact).** The snapshot is `document.body.innerText`, capped at 50k chars, plus the URL and title. It has no roles, no names, no form state, no hrefs and no refs. To click, the agent invents a CSS selector or reads coordinates off a screenshot. The journal failure above is the typical result.

   Every peer system converged on accessibility-tree refs:
   - Playwright MCP: `ref=e12`, plus `f1e12` for iframes.
   - agent-browser: `@e1`. Its interactive-only snapshot (`-i`) is about 200–400 tokens.
   - chrome-devtools-mcp: `uid`.
   - browser-use: indexed interactive elements.

   Keel's snapshot also costs up to 12k tokens and still can't be acted on.

2. **Refs need identity-stable binding when they are added.** agent-browser PR #1444 is the cautionary tale. Positional refs were renumbered after DOM mutations, and the wrong element was clicked while the tool reported success. Bind each ref to a node, for example through a `WeakRef` held in an injected registry. Never reuse ref numbers within a document, and fail loudly with "stale ref, re-snapshot" when a node is gone.

3. **One global agent tab is shared by every chat, and actions supersede each other (high impact if more than one chat uses the browser).** Specifically:
   - BrowserHost sends to the last subscribed UI.
   - browser_panel keeps a single `agent_tab`.
   - The `pending_act` and `pending_snapshot` slots are single.
   - A newer act cancels the older one with "superseded by a newer browser action".

   So two chats driving the browser fight over the same page, and each one sees the other's navigations. There is no per-session routing: the shim does not send any chat id, and the socket is not per chat.

4. **The page sees synthetic input, and dialogs and uploads have no tool.**
   - Click and type dispatch JS events, so `isTrusted` is false. Sites that check this, and some framework handlers and anti-bot checks, ignore them. Typing uses the native value setter plus input/change events, and contenteditable uses `execCommand('insertText')`.
   - wry 0.56.1's UI delegate implements only the file-upload panel, media permission and new-window callbacks. With no handler for `runJavaScriptAlert/Confirm/Prompt`, WebKit returns immediately: `confirm()` gives false and `prompt()` gives null, silently. "Are you sure?" flows quietly cancel.
   - `<input type=file>` opens a native NSOpenPanel in front of the user, and the agent cannot fill it.

5. **Failure modes not covered:**
   - **iframes and shadow DOM:** `document.querySelector` and `innerText` only see the top document and light DOM, so embedded checkouts, auth widgets and web components are invisible.
   - **Waits:** the only wait is navigate's load wait (12 s). There is no `wait_for(text|selector|url|network idle)`, so the agent snapshots too early after SPA routing and retries.
   - **Navigation races:** a click that navigates returns before the new page loads. The next snapshot can land on the old document or mid-load. The one 20 s snapshot timeout in the journals fits this pattern, or a page busy on the main thread.
   - **Collapsed pane:** this works. The page parks off screen at 1280×800, is unloaded after 10 min idle, and is reloaded on the next op. The reload loses SPA state and form input without saying so.
   - **No console or network reads:** Cursor's browser and chrome-devtools-mcp both make console and network access core debugging features. Keel has neither, and no `evaluate` tool.

6. **Security posture.**
   - **Who can drive it:**
     - Anything running as the user can connect to `browser.sock`. It is mode 0600, but unlike `keel.sock` there is no token or peer check.
     - Every harness session gets the tools by default.
     - There is no origin allowlist. Cursor has an admin origin allowlist that blocks the tools on origins outside it.
   - **Cookies:** the pane uses WebKit's persistent default store (~/Library/WebKit/keel and com.keel.dev). The agent tab is often the user's visible tab, so the agent drives pages with the user's logged-in sessions. That is useful, but it is a prompt-injection path: a page can tell the agent to go to a mail site and act there.
   - **What is good:**
     - Navigation is limited to http, https, about, blob and data.
     - Downloads are denied.
     - New windows stay in the pane.
     - Screenshots are saved locally.

7. **Harness coverage (all wired in code; tests cover the config shapes).**

   | Harness | How it gets the tools | Known gaps |
   |---|---|---|
   | Claude | `--mcp-config` stdio server | Approval depends on the permission mode. Images are fine. |
   | Codex | `mcp_servers.keel-browser` thread override on start/resume | Image blocks depend on Codex's MCP image support. |
   | Grok (ACP v1/v2) | `mcpServers` on new/load/resume (`type: stdio` on v2) | v2 agents must advertise `session.mcp.stdio`; Keel doesn't check it. |
   | Command Code (ACP) | same as Grok | same |
   | Antigravity (ACP) | `mcpServers` on new/load. The pooled session keeps the servers it was opened with. | The throwaway one-shot session (prompt suggestions) has no browser, which is correct. |
   | Pi | generated `keel-browser.pi.ts` → `pi.registerMcpServer` (Pi ≥1.0.0, installed 1.1.0) | Fails silently on a Pi without `registerMcpServer` (the extension just returns). |

   The journals show three tool-name styles in real use (`mcp__keel_browser__*`, `keel-browser_browser_*`, and `server_name: keel-browser`), so at least three harness families have actually called it.

## Alternatives

| Option | Harness coverage | Capability | Effort | Verdict |
|---|---|---|---|---|
| **(a) MCP as-is** | All 6, today | Limited by Keel's own toolset, not by MCP | none | **Keep, and improve the tools** |
| **(b) ACP-native client tools** | Only Grok, Command Code and Antigravity. Claude, Codex and Pi aren't ACP in Keel. | Same as MCP | medium | ACP v1 `terminal/*` lets an agent run a command in the client, but it is not a way to add tools. ACP v2 removes it and tells clients to provide MCP servers. The draft **MCP-over-ACP** RFD (`type: "acp"` servers, `mcp/message`, MCP 2026-07-28) would remove the shim process for ACP agents later, but it is unstable in the Rust SDK and only covers half the harnesses. Revisit once it ships. |
| **(c) CLI through the shell** (`keel browser snapshot -i`) | All harnesses that have a shell, which is all of them | Same as MCP. Easy to batch and script, and survives harnesses with weak MCP support. | low (it can reuse browser.sock) | **Add it alongside MCP, not instead.** agent-browser and chrome-devtools-mcp's CLI show the model works. Downsides: no image blocks (only a path), shell approvals on every call, and no tool schema, so a skill or doc is needed. |
| **(d) CDP / WebDriver BiDi on WKWebView** | — | — | very high / not possible | WKWebView has no CDP. `safaridriver` automates Safari only, not embedded WKWebViews. WebKit's BiDi works on GTK, not on Mac yet. The Cocoa WebKitWebDriver PR is WIP and uses private `_WKAutomationSession` SPI (MiniBrowser only). The Web Inspector (`isInspectable`) is for humans, not a supported automation API. Tauri's macOS WebDriver projects do exactly what Keel does: injected JS plus an IPC bridge. **Not an option.** |
| **(e) Embedded Chromium (CEF), then Playwright MCP or chrome-devtools-mcp** | All (it's still MCP) | Best: a real AX tree, trusted input via `Input.dispatchMouseEvent`, frames, dialogs, uploads, console and network | **very high**: +~200 MB, a new rendering, IME and focus path in gpui, notarization, and a separate cookie jar | Only worth it if the pane has to become a serious automation browser. Cheaper middle ground for that need: launch the user's Chrome with `--remote-debugging-port` and point chrome-devtools-mcp at it, outside the pane. |
| **(f) Peers** | | | | **Cursor:** a native in-IDE pane exposed as an MCP server (`cursor-ide-browser`) with navigate/click/type/scroll/screenshot plus **console and network**, per-session tokens, tab isolation, and an origin allowlist. That is the same architecture as Keel. **Zed:** forwards its `context_servers` MCP config to external ACP agents and has no built-in agent browser. **Playwright MCP:** AX snapshot with refs, `depth`/`target`/`boxes`, `browser_find`, a fresh snapshot after each action, wait_for, dialogs, uploads, console and network. **agent-browser:** CLI, `@eN` refs, `-i` interactive-only, `--delta`, identity-bound refs, frame-inlined snapshots. **browser-use:** indexed interactive DOM including shadow DOM and same-origin iframes. **Stagehand:** act/observe/extract on an AX snapshot, with cached actions. |

Harness coverage was the reason MCP was chosen, and it is still the right call. All six harnesses accept a stdio MCP server, and ACP v2 explicitly recommends it. Option (c) is the only cheap complement. Option (b) can come later as an optimisation for ACP harnesses. Options (d) and (e) fix capability, not coverage, and (d) isn't possible on macOS.

## Ranked improvements (impact ÷ effort)

1. **Snapshot as an interactive AX-like tree with stable refs.** Effort: M. Impact: very high.
   - Use injected JS to walk the DOM, including open shadow roots and same-origin iframes.
   - Output only interactive and landmark nodes as `@e12 button "Send" [disabled]`, with roles, accessible names, values, checked state and hrefs.
   - Refs go in a `WeakRef` registry that is never renumbered, with a stale-ref error.
   - Add `mode: "text"` to keep today's innerText output.
   - Expected size: ~0.3–3k tokens instead of up to 12k.
2. **`target` (ref) on click and type, plus `select_option` and `press_key`.** Effort: S once #1 exists. Impact: high. Removes selector guessing, the failure seen in the journals.
3. **Waits.** Effort: S. Impact: high.
   - `browser_wait_for {text|selector|url, timeout}`.
   - Have click and type wait for any navigation they trigger, plus a short settle, before returning.
   - Return a compact post-action snapshot or diff, as Playwright does, which also saves a round trip.
4. **Per-chat tab routing.** Effort: M. Impact: high once more than one chat uses the browser.
   - Pass a session id (an env var or an arg on the shim command line) through the socket.
   - Map one agent tab per chat.
   - Queue per tab instead of superseding.
   - Show which chat owns a tab in the tab strip.
5. **Dialogs and uploads.** Effort: M. Impact: medium.
   - Implement the WKUIDelegate alert/confirm/prompt handlers (wry doesn't). Record pending dialogs and expose `browser_handle_dialog`, with auto-accept off by default.
   - For file inputs, add `browser_upload {target, paths}`. Use DataTransfer injection or a delegate-filled open panel, limited to the workspace.
6. **Console and errors read.** Effort: S. Impact: medium-high for web-dev use. Use an init script that hooks `console.*`, `onerror` and `unhandledrejection` into a ring buffer, and add `browser_console`. Cursor's and chrome-devtools-mcp's experience says this is the most used debugging feature.
7. **Network summary read.** Effort: M. Impact: medium. Hook `fetch`/XHR in the init script (method, url, status, timing), plus `performance.getEntriesByType('resource')`. Without CDP, this is the honest ceiling.
8. **Security hardening.** Effort: S–M. Impact: medium.
   - Add a per-launch token or a `getpeereid` check on browser.sock, as keel.sock has.
   - Add an optional origin allowlist and a per-chat "browser tools on/off" setting.
   - Consider a separate `WKWebsiteDataStore` for agent-only tabs, or show a clear "agent is using your logged-in session" indicator.
9. **Batching (`browser_run` with a list of steps) and an `evaluate` tool.** Effort: S. Impact: medium. This cuts round trips for form fills. Gate `evaluate` behind a permission.
10. **`keel browser …` CLI over the same socket.** Effort: S. Impact: low-medium. A fallback for harnesses where MCP breaks, and handy for scripting.
11. **Observability.** Effort: XS. Log the op, duration and outcome per request in keel.log. Today nothing is logged except "browser.sock listening", so the latency numbers above had to come from the journals.
12. **Later:** move ACP harnesses to MCP-over-ACP once the RFD is stable. Reconsider CEF only if trusted input and cross-origin frames become hard requirements.

## Sources

- ACP terminals (v1): https://agentclientprotocol.com/protocol/terminals
- ACP v2 session setup (`session.mcp.stdio/http`, client MCP servers): https://agentclientprotocol.com/protocol/v2/session-setup
- ACP v2 migration (fs/terminal removed → "use client-provided MCP servers"): https://agentclientprotocol.com/protocol/v2/migration
- MCP-over-ACP RFD (draft): https://agentclientprotocol.com/rfds/mcp-over-acp and https://agentclientprotocol.github.io/rust-sdk/mcp-over-acp.html
- Playwright MCP snapshots and refs: https://playwright.dev/mcp/snapshots, tools: https://playwright.dev/mcp/introduction, vision mode: https://playwright.dev/mcp/vision-mode
- agent-browser snapshots: https://agent-browser.dev/snapshots, stale-ref fix: https://github.com/vercel-labs/agent-browser/pull/1444, changelog: https://agent-browser.dev/changelog
- chrome-devtools-mcp: https://github.com/chromedevtools/chrome-devtools-mcp/blob/main/docs/tool-reference.md, config: https://developer.chrome.com/docs/devtools/agents/get-started/configuration
- Cursor browser: https://cursor.com/docs/agent/tools/browser
- Zed external agents and MCP forwarding: https://zed.dev/docs/ai/external-agents, https://zed.dev/docs/ai/mcp.html
- Safari WebDriver: https://developer.apple.com/documentation/webkit/about-webdriver-for-safari.md
- WebKit BiDi status: https://docs.webkit.org/Other/Contributor%20Meetings/ContributorMeeting2025.html
- WIP Cocoa WebKitWebDriver: https://github.com/WebKit/WebKit/pull/61733
- No WKWebView WebDriver (Tauri): https://github.com/danielraffel/tauri-webdriver
- browser-use serializer: https://github.com/browser-use/browser-use/blob/main/browser_use/dom/serializer/serializer.py
- Stagehand: https://docs.stagehand.dev/v3/basics/observe, https://docs.stagehand.dev/v3/best-practices/caching
- CEF remote debugging: https://chromiumembedded.github.io/cef/general_usage.html

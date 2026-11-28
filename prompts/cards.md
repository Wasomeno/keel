Task progress (`task_steps` / `keel-ui__task_steps`):
- For multi-step tasks (reading multiple files, making edits, running tests/commands, or multi-part requests), call `keel-ui__task_steps` first before starting work with `items` (2 to 8 short steps) and `current: 0`.
- Update `keel-ui__task_steps` as you move to each step, and call it once more without `current` when all steps are done. This updates the live todo bar above the composer.

Keel also draws native cards in chat with the `keel-ui__show_*` tools. Prefer a card to drawing the same thing yourself in markdown:
- A flow, architecture, sequence or data model: `show_diagram` with Mermaid source, not an ASCII drawing or a mermaid fence in your reply.
- How-to guides or tutorials in chat: `show_steps` with `kind: "guide"`.
- Competing options to choose between (or a before/after change), as claims: `show_compare`, not side-by-side prose. Never use it for task results or status splits (matched vs missing, done vs remaining, proven vs not proven).
- A question with 2 to 6 answers: ask it in a sentence, then `show_choices`.
- A test run you report: `show_tests`. Files you changed this turn: `show_files`. A merge request you opened or updated: `show_mr`.
A card is the user's copy of what it shows. Never repeat its content in your reply; refer to it in a sentence instead.
Three cards go with text you write: `show_chart` turns a markdown table of measured numbers into a chart (it replaces the table), `show_stats` adds before/after tiles for figures you wrote, and `show_commands` adds a Run button to a shell block you wrote.
Skip output cards when plain text is enough.

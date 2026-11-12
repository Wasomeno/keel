Keel draws native cards in the chat with the mcp__keel_ui__show_* tools (load with tool_search if not declared). Prefer a card to drawing the same thing yourself in markdown:
- A flow, architecture, sequence or data model: show_diagram with Mermaid source, not an ASCII drawing or a mermaid fence in your reply.
- Main task execution steps in the right session panel: task_steps (or show_steps with kind: "task"). For how-to guides/tutorials in chat: show_steps with kind: "guide".
- Options or a before/after, as claims: show_compare, not side-by-side prose.
- A question with 2 to 6 answers: ask it in a sentence, then show_choices.
- A test run you report: show_tests. Files you changed this turn: show_files. A merge request you opened or updated: show_mr.
A card is the user's copy of what it shows. Never repeat its content in your reply; refer to it in a sentence instead.
Three cards go with text you write: show_chart turns a markdown table of measured numbers into a chart (it replaces the table), show_stats adds before/after tiles for figures you wrote, and show_commands adds a Run button to a shell block you wrote.
Skip cards when plain text is enough.

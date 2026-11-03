//! One assistant message as ordered steps: thinking, tool calls and text,
//! in the order they streamed.
//!
//! The session doc keeps a message's parts in stream order (`parts` on the
//! message view). Consecutive parts of one kind fold into one step: text
//! parts (the doc splits long text into continuation parts), thinking parts,
//! and tool calls ("Ran 2 tools"). Live deltas extend the last step of their
//! kind or start a new one; a later doc push is matched step by step (the
//! n-th text step to the n-th text step), so live text that is ahead of the
//! doc is kept and text after a tool never merges into the text before it.

use crate::markdown::{MessageBlockState, ToolEntry, TranscriptBlock};

/// One step of an assistant message.
#[derive(Debug, Clone)]
pub enum Step {
    /// Answer text, parsed into markdown rows in the background.
    Text(MessageBlockState),
    /// Thinking, folded into one row.
    Reasoning(String),
    /// Consecutive tool calls, folded into one row.
    Tools(Vec<ToolEntry>),
    /// A spawned subagent: an indicator row that opens its session.
    Subagent {
        subagent_id: String,
        name: String,
        state: String,
        summary: String,
    },
    /// A chart the engine's generative UI added (a `ui` part).
    Chart {
        id: String,
        chart: crate::chart::Chart,
    },
    /// Choice buttons the engine's generative UI added (a `ui` part).
    Choices {
        id: String,
        choices: crate::choices::Choices,
    },
    /// A test result card the engine's generative UI added (a `ui` part).
    Tests {
        id: String,
        run: crate::test_run::TestRun,
    },
    /// A merge request card the engine's generative UI added (a `ui` part).
    Mr {
        id: String,
        card: crate::mr_card::MrCard,
    },
    /// Stat tiles the engine's generative UI added (a `ui` part).
    Stats {
        id: String,
        items: Vec<crate::stats::Stat>,
    },
    /// The turn's changed files the engine's generative UI added (a `ui` part).
    Files {
        id: String,
        card: crate::file_changes::FileChanges,
    },
    /// Shell blocks to give a Run button, by their text (a `ui` part).
    Commands(Vec<(String, crate::runnable::Run)>),
    /// Numbered steps the engine's generative UI added (a `ui` part).
    Steps {
        id: String,
        steps: crate::steps::Steps,
    },
    /// One sentence pulled out of the reply (a `ui` part).
    Callout {
        id: String,
        callout: crate::callout::Callout,
    },
    /// Side-by-side claims (a `ui` part).
    Compare {
        id: String,
        columns: Vec<crate::compare::Column>,
    },
    /// A box drawing (a `ui` part).
    Diagram { id: String, flow: crate::flow::Flow },
    /// The agent's plan (a `plan` part); one per message, replaced whole.
    Plan(Vec<crate::trace::PlanItem>),
}

/// A step as the session doc has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocStep {
    Text(String),
    Reasoning(String),
    Tools(Vec<ToolEntry>),
    Subagent {
        subagent_id: String,
        name: String,
        state: String,
        summary: String,
    },
    Chart {
        id: String,
        chart: crate::chart::Chart,
    },
    Choices {
        id: String,
        choices: crate::choices::Choices,
    },
    Tests {
        id: String,
        run: crate::test_run::TestRun,
    },
    Mr {
        id: String,
        card: crate::mr_card::MrCard,
    },
    Stats {
        id: String,
        items: Vec<crate::stats::Stat>,
    },
    Files {
        id: String,
        card: crate::file_changes::FileChanges,
    },
    Commands(Vec<(String, crate::runnable::Run)>),
    Steps {
        id: String,
        steps: crate::steps::Steps,
    },
    Callout {
        id: String,
        callout: crate::callout::Callout,
    },
    Compare {
        id: String,
        columns: Vec<crate::compare::Column>,
    },
    Diagram {
        id: String,
        flow: crate::flow::Flow,
    },
    Plan(Vec<crate::trace::PlanItem>),
}

/// Parse id of a message's `ordinal`-th text step. Its rows are
/// `{message_id}#t{ordinal}#{block}`.
pub fn text_id(message_id: &str, ordinal: usize) -> String {
    format!("{message_id}#t{ordinal}")
}

/// A message view's text: its text parts in order (the engine sends no
/// top-level copy); a `text` field, if an older engine sent one.
pub fn view_text(view: &serde_json::Value) -> Option<String> {
    if let Some(text) = view.get("text").and_then(|v| v.as_str()) {
        return Some(text.to_string());
    }
    let parts = view.get("parts")?.as_array()?;
    Some(
        parts
            .iter()
            .filter(|p| p.get("kind").and_then(|k| k.as_str()) == Some("text"))
            .filter_map(|p| p.get("body").and_then(|b| b.as_str()))
            .collect(),
    )
}

/// Applies one message push frame to the view kept for that message.
///
/// The engine sends a message whole once per subscription, then only what
/// changed (`keel_engine` `push.rs`): `parts_from` keeps that many parts,
/// `body_from` appends the first incoming part's body to part `parts_from`,
/// and `parts_len` is the count afterwards. A frame without `parts_from` is a
/// whole view from an older engine. `None` when the frame does not fit the
/// stored view (it is dropped; the next whole push repairs it).
pub fn apply_message_frame(
    stored: Option<serde_json::Value>,
    frame: &serde_json::Value,
) -> Option<serde_json::Value> {
    use serde_json::Value;
    let Some(from) = frame.get("parts_from").and_then(Value::as_u64) else {
        return Some(frame.clone());
    };
    let from = from as usize;
    let body_from = frame
        .get("body_from")
        .and_then(Value::as_u64)
        .map(|b| b as usize);
    let mut view = match stored {
        Some(view) if view.get("parts").is_some_and(Value::is_array) => view,
        _ if from == 0 && body_from.is_none() => serde_json::json!({ "parts": [] }),
        _ => return None,
    };
    let incoming = frame.get("parts")?.as_array()?;
    let parts = view.get_mut("parts")?.as_array_mut()?;
    let mut rest = incoming.iter();
    if let Some(offset) = body_from {
        let more = rest.next()?.get("body")?.as_str()?;
        let target = parts.get_mut(from)?;
        let Some(Value::String(body)) = target.get_mut("body") else {
            return None;
        };
        if body.len() != offset {
            return None;
        }
        body.push_str(more);
        parts.truncate(from + 1);
    } else {
        if parts.len() < from {
            return None;
        }
        parts.truncate(from);
    }
    parts.extend(rest.cloned());
    if Some(parts.len() as u64) != frame.get("parts_len").and_then(Value::as_u64) {
        return None;
    }
    for key in ["id", "role", "created_at", "attachments"] {
        if let Some(value) = frame.get(key) {
            view[key] = value.clone();
        }
    }
    Some(view)
}

/// Steps from a message view. Uses the ordered `parts` array; a view from an
/// engine without it falls back to thinking, tools, then text.
pub fn doc_steps(view: &serde_json::Value) -> Vec<DocStep> {
    let mut out: Vec<DocStep> = Vec::new();
    let Some(parts) = view.get("parts").and_then(|v| v.as_array()) else {
        let s = |k: &str| {
            view.get(k)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string()
        };
        let reasoning = s("reasoning");
        if !reasoning.is_empty() {
            out.push(DocStep::Reasoning(reasoning));
        }
        let tools = ToolEntry::parse_list(view);
        if !tools.is_empty() {
            out.push(DocStep::Tools(tools));
        }
        let text = s("text");
        if !text.is_empty() {
            out.push(DocStep::Text(text));
        }
        return out;
    };
    for part in parts {
        let s = |k: &str| {
            part.get(k)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string()
        };
        match s("kind").as_str() {
            "text" => {
                let body = s("body");
                match out.last_mut() {
                    Some(DocStep::Text(text)) => text.push_str(&body),
                    _ if body.is_empty() => {}
                    _ => out.push(DocStep::Text(body)),
                }
            }
            "reasoning" => {
                let body = s("body");
                match out.last_mut() {
                    Some(DocStep::Reasoning(text)) => text.push_str(&body),
                    _ if body.is_empty() => {}
                    _ => out.push(DocStep::Reasoning(body)),
                }
            }
            "tool" => {
                let tool = ToolEntry::from_json(part);
                // A Keel UI call is the card below it, not a second row.
                if keel_proto::is_ui_tool(&tool.name) {
                    continue;
                }
                // A question's receipt is a row of its own: never folded
                // into the tools around it.
                match out.last_mut() {
                    Some(DocStep::Tools(tools))
                        if !tool.is_receipt() && !tools.iter().any(ToolEntry::is_receipt) =>
                    {
                        tools.push(tool)
                    }
                    _ => out.push(DocStep::Tools(vec![tool])),
                }
            }
            "plan" => {
                let entries = part
                    .get("entries")
                    .and_then(|v| v.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|e| {
                        let text = |k: &str| {
                            e.get(k)
                                .and_then(|v| v.as_str())
                                .filter(|v| !v.is_empty())
                                .map(str::to_string)
                        };
                        Some(crate::trace::PlanItem {
                            content: text("content")?,
                            status: text("status").unwrap_or_else(|| "pending".into()),
                            priority: text("priority"),
                        })
                    })
                    .collect::<Vec<_>>();
                if !entries.is_empty() {
                    out.push(DocStep::Plan(entries));
                }
            }
            "subagent" => {
                let step = DocStep::Subagent {
                    subagent_id: s("subagent_id"),
                    name: s("name"),
                    state: s("state"),
                    summary: s("summary"),
                };
                out.push(step)
            }
            "ui" => {
                let spec = part.get("spec").unwrap_or(&serde_json::Value::Null);
                if let Some(chart) = crate::chart::Chart::from_spec(spec) {
                    out.push(DocStep::Chart { id: s("id"), chart });
                } else if let Some(choices) = crate::choices::Choices::from_spec(spec) {
                    out.push(DocStep::Choices {
                        id: s("id"),
                        choices,
                    });
                } else if let Some(run) = crate::test_run::TestRun::from_spec(spec) {
                    out.push(DocStep::Tests { id: s("id"), run });
                } else if let Some(card) = crate::mr_card::MrCard::from_spec(spec) {
                    out.push(DocStep::Mr { id: s("id"), card });
                } else if let Some(items) = crate::stats::from_spec(spec) {
                    out.push(DocStep::Stats { id: s("id"), items });
                } else if let Some(card) = crate::file_changes::FileChanges::from_spec(spec) {
                    out.push(DocStep::Files { id: s("id"), card });
                } else if let Some(items) = crate::runnable::from_spec(spec) {
                    out.push(DocStep::Commands(items));
                } else if let Some(steps) = crate::steps::Steps::from_spec(spec) {
                    out.push(DocStep::Steps { id: s("id"), steps });
                } else if let Some(callout) = crate::callout::Callout::from_spec(spec) {
                    out.push(DocStep::Callout {
                        id: s("id"),
                        callout,
                    });
                } else if let Some(columns) = crate::compare::from_spec(spec) {
                    out.push(DocStep::Compare {
                        id: s("id"),
                        columns,
                    });
                } else if let Some(flow) = crate::flow::Flow::from_spec(spec) {
                    out.push(DocStep::Diagram { id: s("id"), flow });
                }
            }
            _ => {}
        }
    }
    out
}

fn kind(step: &Step) -> u8 {
    match step {
        Step::Text(_) => 0,
        Step::Reasoning(_) => 1,
        Step::Tools(_) => 2,
        Step::Subagent { .. } => 3,
        Step::Chart { .. } => 4,
        Step::Choices { .. } => 5,
        Step::Tests { .. } => 6,
        Step::Mr { .. } => 7,
        Step::Stats { .. } => 8,
        Step::Files { .. } => 9,
        Step::Commands(_) => 10,
        Step::Plan(_) => 11,
        Step::Steps { .. } => 12,
        Step::Callout { .. } => 13,
        Step::Compare { .. } => 14,
        Step::Diagram { .. } => 15,
    }
}

fn text_count(steps: &[Step]) -> usize {
    steps.iter().filter(|s| matches!(s, Step::Text(_))).count()
}

/// Replace `old` with the doc's steps, keeping parse progress and any live
/// text or thinking the doc has not caught up to. Steps pair up by kind and
/// ordinal. Live steps past the doc's (it lags the stream) stay at the end.
pub fn merge_doc(message_id: &str, old: Vec<Step>, doc: Vec<DocStep>) -> Vec<Step> {
    // Old steps by kind, in order.
    let mut by_kind: [std::collections::VecDeque<Step>; 16] = Default::default();
    for step in old {
        by_kind[kind(&step) as usize].push_back(step);
    }
    let mut out: Vec<Step> = Vec::with_capacity(doc.len());
    for step in doc {
        out.push(match step {
            DocStep::Text(text) => {
                let ordinal = text_count(&out);
                let mut state = match by_kind[0].pop_front() {
                    Some(Step::Text(state)) if state.message_id == text_id(message_id, ordinal) => {
                        state
                    }
                    _ => MessageBlockState::new(text_id(message_id, ordinal)),
                };
                state.set_text(&text);
                Step::Text(state)
            }
            DocStep::Reasoning(body) => match by_kind[1].pop_front() {
                Some(Step::Reasoning(live))
                    if live.len() > body.len() && live.starts_with(&body) =>
                {
                    Step::Reasoning(live)
                }
                _ => Step::Reasoning(body),
            },
            DocStep::Tools(tools) => {
                by_kind[2].pop_front();
                Step::Tools(tools)
            }
            DocStep::Subagent {
                subagent_id,
                name,
                state,
                summary,
            } => {
                by_kind[3].pop_front();
                Step::Subagent {
                    subagent_id,
                    name,
                    state,
                    summary,
                }
            }
            DocStep::Chart { id, chart } => {
                by_kind[4].pop_front();
                Step::Chart { id, chart }
            }
            DocStep::Choices { id, choices } => {
                by_kind[5].pop_front();
                Step::Choices { id, choices }
            }
            DocStep::Tests { id, run } => {
                by_kind[6].pop_front();
                Step::Tests { id, run }
            }
            DocStep::Mr { id, card } => {
                by_kind[7].pop_front();
                Step::Mr { id, card }
            }
            DocStep::Stats { id, items } => {
                by_kind[8].pop_front();
                Step::Stats { id, items }
            }
            DocStep::Files { id, card } => {
                by_kind[9].pop_front();
                Step::Files { id, card }
            }
            DocStep::Plan(entries) => {
                by_kind[11].pop_front();
                Step::Plan(entries)
            }
            DocStep::Commands(items) => {
                by_kind[10].pop_front();
                Step::Commands(items)
            }
            DocStep::Steps { id, steps } => {
                by_kind[12].pop_front();
                Step::Steps { id, steps }
            }
            DocStep::Callout { id, callout } => {
                by_kind[13].pop_front();
                Step::Callout { id, callout }
            }
            DocStep::Compare { id, columns } => {
                by_kind[14].pop_front();
                Step::Compare { id, columns }
            }
            DocStep::Diagram { id, flow } => {
                by_kind[15].pop_front();
                Step::Diagram { id, flow }
            }
        });
    }
    // Live-only text and thinking (tools, subagents and charts only ever come from the doc).
    let [texts, reasonings, _, _, _, _, _, _, _, _, _, _, _, _, _, _] = by_kind;
    for step in reasonings.into_iter().chain(texts) {
        // A live step the doc already holds (a driver that glued it onto an
        // earlier part) is not shown twice.
        let held = |live: &str| {
            let live = live.trim();
            !live.is_empty()
                && out.iter().any(|step| match step {
                    Step::Text(state) => state.full_text.contains(live),
                    Step::Reasoning(body) => body.contains(live),
                    _ => false,
                })
        };
        match step {
            Step::Text(state) if held(&state.full_text) => {}
            Step::Reasoning(body) if held(&body) => {}
            Step::Text(state) => {
                let ordinal = text_count(&out);
                let state = if state.message_id == text_id(message_id, ordinal) {
                    state
                } else {
                    let mut fresh = MessageBlockState::new(text_id(message_id, ordinal));
                    fresh.full_text = state.full_text;
                    fresh
                };
                out.push(Step::Text(state));
            }
            other => out.push(other),
        }
    }
    out
}

/// A live text delta: extends the last step when it is text (and no tool
/// started since), else starts a new text step. Returns nothing; the caller
/// marks the message for parsing.
pub fn push_text(steps: &mut Vec<Step>, message_id: &str, delta: &str, new_step: bool) {
    let end = stream_end(steps);
    if !new_step && let Some(Step::Text(state)) = steps[..end].last_mut() {
        state.full_text.push_str(delta);
        return;
    }
    let mut state = MessageBlockState::new(text_id(message_id, text_count(&steps[..end])));
    state.full_text.push_str(delta);
    steps.insert(end, Step::Text(state));
}

impl Step {
    /// Generative UI the engine added, rather than streamed text or tools.
    fn is_generated(&self) -> bool {
        matches!(
            self,
            Step::Chart { .. }
                | Step::Choices { .. }
                | Step::Tests { .. }
                | Step::Mr { .. }
                | Step::Stats { .. }
                | Step::Files { .. }
                | Step::Commands(_)
                | Step::Steps { .. }
                | Step::Callout { .. }
                | Step::Compare { .. }
                | Step::Diagram { .. }
        )
    }
}

/// Where live deltas go: before any trailing generative UI, which can land
/// while the reply still streams.
fn stream_end(steps: &[Step]) -> usize {
    steps
        .iter()
        .rposition(|step| !step.is_generated())
        .map_or(0, |ix| ix + 1)
}

/// A live thinking delta: extends the last step when it is thinking, else
/// starts a new one.
pub fn push_reasoning(steps: &mut Vec<Step>, delta: &str, new_step: bool) {
    let end = stream_end(steps);
    if !new_step && let Some(Step::Reasoning(body)) = steps[..end].last_mut() {
        body.push_str(delta);
        return;
    }
    steps.insert(end, Step::Reasoning(delta.to_string()));
}

/// Append one message's rows to a turn's rows, in step order. A tool step
/// right after another tool row (also across messages of one turn) joins
/// that group. `expanded` says whether a folded row id is open.
/// True when the message's callout says nothing its text doesn't: the
/// reply, all of it, is that one sentence. Compared loosely (whitespace,
/// case, a closing period), so a callout that adds a point still shows.
fn repeats_reply(steps: &[Step]) -> bool {
    let plain = |text: &str| {
        let words = text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        words.trim_end_matches(['.', '!']).to_string()
    };
    let reply = plain(
        &steps
            .iter()
            .filter_map(|step| {
                if let Step::Text(text) = step {
                    Some(text.full_text.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    );
    !reply.is_empty()
        && steps.iter().any(
            |step| matches!(step, Step::Callout { callout, .. } if plain(&callout.text) == reply),
        )
}

pub fn push_rows(
    message_id: &str,
    steps: &[Step],
    expanded: &dyn Fn(&str) -> bool,
    out: &mut Vec<TranscriptBlock>,
) {
    let (mut reasoning_ix, mut tools_ix) = (0usize, 0usize);
    let mut placed: Vec<&String> = Vec::new();
    let runs: Vec<&(String, crate::runnable::Run)> = steps
        .iter()
        .filter_map(|step| match step {
            Step::Commands(items) => Some(items.iter()),
            _ => None,
        })
        .flatten()
        .collect();
    let charts: Vec<(&String, &crate::chart::Chart)> = steps
        .iter()
        .filter_map(|step| match step {
            Step::Chart { id, chart } => Some((id, chart)),
            _ => None,
        })
        .collect();
    for (_ix, step) in steps.iter().enumerate() {
        match step {
            Step::Text(state) => {
                let blocks = state.all_blocks();
                for mut block in blocks {
                    // A shell block the engine marked runnable gets its Run.
                    if let TranscriptBlock::Code { content, run, .. } = &mut block
                        && let Some((_, found)) =
                            runs.iter().find(|(text, _)| text == content.trim())
                    {
                        *run = Some(found.clone());
                    }
                    if let TranscriptBlock::Table { header, rows, .. } = &block {
                        let header: Vec<&str> =
                            header.iter().map(|cell| cell.text.as_str()).collect();
                        let rows: Vec<Vec<&str>> = rows
                            .iter()
                            .map(|row| row.iter().map(|cell| cell.text.as_str()).collect())
                            .collect();
                        if let Some(&(id, chart)) = charts
                            .iter()
                            .find(|(id, chart)| !placed.contains(id) && chart.is_of(&header, &rows))
                        {
                            placed.push(id);
                            let id = format!("{message_id}#ui-{id}");
                            out.push(TranscriptBlock::Chart {
                                id,
                                chart: chart.clone(),
                                replaces: Some(Box::new(block)),
                            });
                            continue;
                        }
                    }
                    out.push(block);
                }
            }
            Step::Subagent {
                subagent_id,
                name,
                state,
                summary,
            } => {
                let id = format!("{message_id}#sub-{subagent_id}");
                out.push(TranscriptBlock::Subagent {
                    id,
                    subagent_id: subagent_id.clone(),
                    name: name.clone(),
                    state: state.clone(),
                    summary: summary.clone(),
                });
            }
            Step::Choices { id, choices } => {
                out.push(TranscriptBlock::Choices {
                    id: format!("{message_id}#ui-{id}"),
                    choices: choices.clone(),
                });
            }
            Step::Tests { id, run } => {
                out.push(TranscriptBlock::Tests {
                    id: format!("{message_id}#ui-{id}"),
                    run: run.clone(),
                });
            }
            Step::Commands(_) => {}
            Step::Plan(entries) => {
                let id = format!("{message_id}#plan");
                let live = entries
                    .iter()
                    .any(|e| matches!(e.status.as_str(), "in_progress" | "inProgress" | "running"));
                let expanded = match (expanded(&format!("{id}=1")), expanded(&format!("{id}=0"))) {
                    (true, _) => Some(true),
                    (_, true) => Some(false),
                    _ => None,
                };
                out.push(TranscriptBlock::Plan {
                    id,
                    entries: entries.clone(),
                    live,
                    expanded,
                });
            }
            Step::Stats { id, items } => {
                out.push(TranscriptBlock::Stats {
                    id: format!("{message_id}#ui-{id}"),
                    items: items.clone(),
                });
            }
            Step::Files { id, card } => {
                let row_id = format!("{message_id}#ui-{id}");
                out.push(TranscriptBlock::FileChanges {
                    id: row_id.clone(),
                    card: card.clone(),
                    expanded: expanded(&row_id),
                });
            }
            Step::Mr { id, card } => {
                out.push(TranscriptBlock::MrCard {
                    id: format!("{message_id}#ui-{id}"),
                    card: card.clone(),
                    message_id: message_id.to_string(),
                    part_id: id.clone(),
                });
            }
            Step::Steps { id, steps } => {
                if steps.kind == crate::steps::StepKind::Guide {
                    out.push(TranscriptBlock::Steps {
                        id: format!("{message_id}#ui-{id}"),
                        steps: steps.clone(),
                    });
                }
            }
            // A callout that is the whole reply would show the same sentence twice.
            Step::Callout { .. } if repeats_reply(steps) => {}
            Step::Callout { id, callout } => {
                out.push(TranscriptBlock::Callout {
                    id: format!("{message_id}#ui-{id}"),
                    callout: callout.clone(),
                });
            }
            Step::Compare { id, columns } => {
                out.push(TranscriptBlock::Compare {
                    id: format!("{message_id}#ui-{id}"),
                    columns: columns.clone(),
                });
            }
            Step::Diagram { id, flow } => {
                out.push(flow.block(format!("{message_id}#ui-{id}")));
            }
            // Placed where its table was; one whose table isn't found goes last.
            Step::Chart { id, chart } => {
                if !placed.contains(&id) {
                    out.push(TranscriptBlock::Chart {
                        id: format!("{message_id}#ui-{id}"),
                        chart: chart.clone(),
                        replaces: None,
                    });
                }
            }
            Step::Reasoning(body) => {
                let id = format!("{message_id}#reasoning{reasoning_ix}");
                reasoning_ix += 1;
                if body.trim().is_empty() {
                    continue;
                }
                // Thinking right after thinking (two parts, or across the
                // turn's messages) is one Thought row.
                if let Some(TranscriptBlock::Reasoning { content, .. }) = out.last_mut() {
                    let more = body.trim();
                    if !content.contains(more) {
                        content.push_str("\n\n");
                        content.push_str(more);
                    }
                    continue;
                }
                out.push(TranscriptBlock::Reasoning {
                    expanded: expanded(&id),
                    id,
                    content: body.trim().to_string(),
                });
            }
            Step::Tools(tools) => {
                let id = format!("{message_id}#tools{tools_ix}");
                tools_ix += 1;
                if tools.is_empty() {
                    continue;
                }
                if tools.iter().any(ToolEntry::is_receipt) {
                    let id = format!("{message_id}#receipt-{}", tools[0].tool_id);
                    out.push(TranscriptBlock::ToolGroup {
                        expanded: expanded(&id),
                        id,
                        tools: tools.clone(),
                        overrides: Vec::new(),
                    });
                    continue;
                }
                let images: Vec<String> = tools
                    .iter()
                    .flat_map(|t| t.images.iter().cloned())
                    .collect();
                // A run of tools folds into the group before it; its
                // screenshots join that group's (visible) image row.
                let len = out.len();
                let shots_after_group = len >= 2
                    && matches!(out[len - 1], TranscriptBlock::Shots { .. })
                    && matches!(&out[len - 2], TranscriptBlock::ToolGroup { tools, .. } if !tools.iter().any(ToolEntry::is_receipt));
                if shots_after_group {
                    if let TranscriptBlock::ToolGroup { tools: group, .. } = &mut out[len - 2] {
                        group.extend(tools.iter().cloned());
                    }
                    if let TranscriptBlock::Shots { paths, .. } = &mut out[len - 1] {
                        paths.extend(images);
                    }
                    continue;
                }
                if let Some(TranscriptBlock::ToolGroup {
                    tools: group,
                    id: group_id,
                    overrides,
                    ..
                }) = out.last_mut()
                    && !group.iter().any(ToolEntry::is_receipt)
                {
                    overrides.extend(line_overrides(group_id, tools, expanded));
                    group.extend(tools.iter().cloned());
                    if !images.is_empty() {
                        let id = format!("{group_id}#shots");
                        out.push(TranscriptBlock::Shots { id, paths: images });
                    }
                    continue;
                }
                let shots = format!("{id}#shots");
                let overrides = line_overrides(&id, tools, expanded);
                out.push(TranscriptBlock::ToolGroup {
                    expanded: expanded(&id),
                    id,
                    tools: tools.clone(),
                    overrides,
                });
                if !images.is_empty() {
                    out.push(TranscriptBlock::Shots {
                        id: shots,
                        paths: images,
                    });
                }
            }
        }
    }
}

/// The id of one trace line inside a tool-group row. Toggles send
/// `{line}=1` / `{line}=0` (the state the user chose).
pub fn line_id(group_id: &str, tool_id: &str) -> String {
    format!("{group_id}~{tool_id}")
}

/// The open/closed choices the user made for these tools' lines and for an
/// Explored fold that starts at one of them.
fn line_overrides(
    group_id: &str,
    tools: &[ToolEntry],
    expanded: &dyn Fn(&str) -> bool,
) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    for tool in tools {
        for line in [
            line_id(group_id, &tool.tool_id),
            line_id(group_id, &format!("explored-{}", tool.tool_id)),
        ] {
            if expanded(&format!("{line}=1")) {
                out.push((line, true));
            } else if expanded(&format!("{line}=0")) {
                out.push((line, false));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn message_frames_apply_in_order() {
        use serde_json::json;
        let head = |from: usize, len: usize, body_from: Option<usize>, parts: serde_json::Value| {
            let mut f = json!({ "id": "m", "role": "assistant", "created_at": 1, "attachments": [], "parts_from": from, "parts_len": len, "parts": parts });
            if let Some(b) = body_from {
                f["body_from"] = json!(b);
            }
            f
        };
        let v = apply_message_frame(
            None,
            &head(
                0,
                1,
                None,
                json!([{ "kind": "text", "id": "p", "body": "he" }]),
            ),
        )
        .unwrap();
        let v = apply_message_frame(
            Some(v),
            &head(
                0,
                1,
                Some(2),
                json!([{ "kind": "text", "id": "p", "body": "llo" }]),
            ),
        )
        .unwrap();
        assert_eq!(view_text(&v).as_deref(), Some("hello"));
        let v = apply_message_frame(
            Some(v),
            &head(
                1,
                2,
                None,
                json!([{ "kind": "tool", "tool_id": "t", "name": "bash", "state": "running" }]),
            ),
        )
        .unwrap();
        assert_eq!(v["parts"].as_array().unwrap().len(), 2);
        // A frame that does not line up is refused rather than corrupting.
        assert!(
            apply_message_frame(
                Some(v.clone()),
                &head(0, 1, Some(9), json!([{ "body": "x" }]))
            )
            .is_none()
        );
        assert!(apply_message_frame(Some(v.clone()), &head(5, 6, None, json!([{}]))).is_none());
        assert!(apply_message_frame(None, &head(1, 2, None, json!([{}]))).is_none());
        // Shrinking truncates; an old whole view replaces.
        let v = apply_message_frame(Some(v), &head(1, 1, None, json!([]))).unwrap();
        assert_eq!(v["parts"].as_array().unwrap().len(), 1);
        let whole = json!({ "id": "m", "parts": [] });
        assert_eq!(apply_message_frame(Some(v), &whole), Some(whole));
    }

    #[test]
    fn view_text_joins_text_parts() {
        let view = serde_json::json!({"parts": [
            {"kind": "text", "id": "a", "body": "Hello "},
            {"kind": "tool", "tool_id": "1", "name": "bash", "state": "done", "summary": ""},
            {"kind": "reasoning", "id": "r", "body": "hmm"},
            {"kind": "text", "id": "b", "body": "world"},
        ]});
        assert_eq!(view_text(&view).as_deref(), Some("Hello world"));
        assert_eq!(
            view_text(&serde_json::json!({"text": "legacy"})).as_deref(),
            Some("legacy")
        );
        assert_eq!(view_text(&serde_json::json!({})), None);
    }

    use super::*;

    fn tool(id: &str) -> serde_json::Value {
        serde_json::json!({"kind": "tool", "tool_id": id, "name": "bash", "state": "done", "summary": "ok"})
    }

    #[test]
    fn a_ui_chart_part_becomes_a_chart_row_after_the_text() {
        let view = serde_json::json!({"parts": [
            {"kind": "text", "id": "t", "body": "Done."},
            {"kind": "ui", "id": "chart-0", "spec": {"component": "chart", "kind": "line", "x": "v",
                "labels": ["1", "2"], "series": [{"name": "ms", "values": [3.0, 4.0]}]}},
            {"kind": "ui", "id": "x", "spec": {"component": "unknown"}},
        ]});
        let steps = merge_doc("m", Vec::new(), doc_steps(&view));
        let mut rows = Vec::new();
        push_rows("m", &steps, &|_| false, &mut rows);
        assert!(
            matches!(rows.last(), Some(TranscriptBlock::Chart { id, chart, .. })
            if id == "m#ui-chart-0" && chart.kind == crate::chart::Kind::Line)
        );
        assert_eq!(steps.len(), 2);
    }

    #[test]
    fn a_callout_that_is_the_whole_reply_is_not_shown_twice() {
        let rows_for = |text: &str, callout: &str| {
            let view = serde_json::json!({"parts": [
                {"kind": "text", "id": "t0", "body": text},
                {"kind": "ui", "id": "note", "spec": {"component": "callout", "kind": "result", "text": callout}},
            ]});
            let mut rows = Vec::new();
            push_rows(
                "m",
                &merge_doc("m", Vec::new(), doc_steps(&view)),
                &|_| false,
                &mut rows,
            );
            rows.iter()
                .filter(|row| matches!(row, TranscriptBlock::Callout { .. }))
                .count()
        };
        assert_eq!(
            rows_for(
                "Google is open in the browser.",
                "Google is open in the browser."
            ),
            0
        );
        assert_eq!(
            rows_for(
                "google is open in the  browser",
                "Google is open in the browser."
            ),
            0
        );
        assert_eq!(
            rows_for(
                "Opened it. Google is open in the browser.",
                "Google is open in the browser."
            ),
            1
        );
    }

    #[test]
    fn an_explain_card_becomes_its_row_and_a_bad_drawing_stays_text() {
        let drawn = "┌───┐     ┌───┐\n│ A │────▶│ B │\n└───┘     └───┘";
        let view = serde_json::json!({"parts": [
            {"kind": "ui", "id": "steps", "spec": {"component": "steps", "items": ["Read", "Close"], "current": 1, "kind": "guide"}},
            {"kind": "ui", "id": "task_steps", "spec": {"component": "steps", "items": ["Task 1", "Task 2"], "current": 0, "kind": "task"}},
            {"kind": "ui", "id": "note", "spec": {"component": "callout", "kind": "result", "text": "The watcher was the leak."}},
            {"kind": "ui", "id": "cmp", "spec": {"component": "compare", "columns": [
                {"title": "Redis", "points": ["Survives"], "recommended": true},
                {"title": "Memory", "points": ["Nothing to run"]}
            ]}},
            {"kind": "ui", "id": "flow", "spec": {"component": "diagram", "source": drawn}},
            {"kind": "ui", "id": "junk", "spec": {"component": "diagram", "source": "just words"}},
        ]});
        let mut rows = Vec::new();
        push_rows(
            "m",
            &merge_doc("m", Vec::new(), doc_steps(&view)),
            &|_| false,
            &mut rows,
        );
        assert!(matches!(rows[0], TranscriptBlock::Steps { ref id, .. } if id == "m#ui-steps"));
        assert!(
            !rows
                .iter()
                .any(|r| matches!(r, TranscriptBlock::Steps { id, .. } if id == "m#ui-task_steps"))
        );
        assert!(matches!(rows[1], TranscriptBlock::Callout { .. }));
        assert!(
            matches!(rows[2], TranscriptBlock::Compare { ref columns, .. } if columns.len() == 2)
        );
        // A diagram card draws as its fence (an old box drawing: a `diagram` one).
        assert!(
            matches!(rows[3], TranscriptBlock::Code { language: Some(ref l), .. } if l == "diagram")
        );
        // One no fence can draw still shows its source rather than vanishing.
        assert!(
            matches!(rows[4], TranscriptBlock::Code { ref id, ref content, .. } if id == "m#ui-junk" && content == "just words")
        );
        assert_eq!(rows.len(), 5);
    }

    #[test]
    fn live_text_after_an_early_chart_extends_the_text_before_it() {
        let view = serde_json::json!({"parts": [
            {"kind": "text", "id": "t", "body": "| f | kb |\n|---|---|\n| a | 1 |\n| b | 2 |\n\nMore"},
            {"kind": "ui", "id": "chart-0", "spec": {"component": "chart", "kind": "bar", "x": "f",
                "labels": ["a", "b"], "series": [{"name": "kb", "values": [1.0, 2.0]}]}},
        ]});
        let mut steps = merge_doc("m", Vec::new(), doc_steps(&view));
        push_text(&mut steps, "m", " text", false);
        assert_eq!(steps.len(), 2);
        assert!(matches!(&steps[0], Step::Text(state) if state.full_text.ends_with("More text")));
        assert!(matches!(steps[1], Step::Chart { .. }));
    }

    #[test]
    fn choices_render_after_the_reply() {
        let view = serde_json::json!({"parts": [
            {"kind": "text", "id": "t", "body": "Which?\n\n1. Redis\n2. Memory\n"},
            {"kind": "ui", "id": "choices", "spec": {"component": "choices", "options": ["Redis", "Memory"], "recommended": 0}},
        ]});
        let steps = merge_doc("m", Vec::new(), doc_steps(&view));
        let mut rows = Vec::new();
        push_rows("m", &steps, &|_| false, &mut rows);
        assert!(
            matches!(rows.last(), Some(TranscriptBlock::Choices { id, choices })
            if id == "m#ui-choices" && choices.recommended == Some(0))
        );
    }

    #[test]
    fn a_runnable_shell_block_gets_its_run() {
        let view = serde_json::json!({"parts": [
            {"kind": "text", "id": "t", "body": "Run:\n\n```bash\npnpm test:run\n```\n"},
            {"kind": "ui", "id": "commands", "spec": {"component": "commands", "items": [
                {"content": "pnpm test:run\n", "run": "pnpm test:run", "risky": false}]}},
        ]});
        let mut steps = merge_doc("m", Vec::new(), doc_steps(&view));
        if let Some(Step::Text(state)) = steps.first_mut() {
            state.seal();
        }
        let mut rows = Vec::new();
        push_rows("m", &steps, &|_| false, &mut rows);
        assert!(rows.iter().any(|r| matches!(r, TranscriptBlock::Code { run: Some(run), .. } if run.command == "pnpm test:run")));
    }

    #[test]
    fn streaming_text_lands_per_token() {
        let view = serde_json::json!({"parts": [{"kind": "text", "id": "t", "body": "First paragraph.\n\nSecond, still stream"}]});
        let mut steps = merge_doc("m", Vec::new(), doc_steps(&view));
        if let Some(Step::Text(state)) = steps.first_mut() {
            state.reparse_tail();
        }
        let mut rows = Vec::new();
        push_rows("m", &steps, &|_| false, &mut rows);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn a_chart_takes_its_tables_place() {
        let view = serde_json::json!({"parts": [
            {"kind": "text", "id": "t", "body": "| a | b |\n|---|---|\n| x | y |\n\nSizes:\n\n| f | kb |\n|---|---|\n| a | 1 |\n| b | 2 |\n\nDone."},
            {"kind": "ui", "id": "chart-1", "spec": {"component": "chart", "kind": "bar", "x": "f",
                "labels": ["a", "b"], "series": [{"name": "kb", "values": [1.0, 2.0]}]}},
        ]});
        let mut steps = merge_doc("m", Vec::new(), doc_steps(&view));
        if let Some(Step::Text(state)) = steps.first_mut() {
            state.seal();
        }
        let mut rows = Vec::new();
        push_rows("m", &steps, &|_| false, &mut rows);
        let shape: Vec<&str> = rows
            .iter()
            .map(|r| match r {
                TranscriptBlock::Table { .. } => "table",
                TranscriptBlock::Chart {
                    replaces: Some(_), ..
                } => "chart",
                TranscriptBlock::Paragraph { .. } => "text",
                _ => "other",
            })
            .collect();
        assert_eq!(shape, ["table", "text", "chart", "text"]);
    }

    #[test]
    fn a_chart_whose_table_is_not_found_goes_last_and_hides_nothing() {
        let view = serde_json::json!({"parts": [
            {"kind": "text", "id": "t", "body": "| f | kb |\n|---|---|\n| a | 1 |\n| b | 2 |\n\nDone."},
            {"kind": "ui", "id": "chart-0", "spec": {"component": "chart", "kind": "bar", "x": "f",
                "labels": ["a", "c"], "series": [{"name": "kb", "values": [1.0, 2.0]}]}},
        ]});
        let mut steps = merge_doc("m", Vec::new(), doc_steps(&view));
        if let Some(Step::Text(state)) = steps.first_mut() {
            state.seal();
        }
        let mut rows = Vec::new();
        push_rows("m", &steps, &|_| false, &mut rows);
        assert!(matches!(rows[0], TranscriptBlock::Table { .. }));
        assert!(matches!(
            rows.last(),
            Some(TranscriptBlock::Chart { replaces: None, .. })
        ));
    }

    fn kinds(rows: &[TranscriptBlock]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                TranscriptBlock::Reasoning { .. } => "think".to_string(),
                TranscriptBlock::ToolGroup { tools, .. } => format!("tools{}", tools.len()),
                TranscriptBlock::Paragraph { content, .. } => content.clone(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    fn parsed(steps: &mut [Step]) {
        for step in steps {
            if let Step::Text(state) = step {
                state.reparse_tail();
            }
        }
    }

    /// A tool's screenshot becomes a visible image row right under its
    /// (folded) tool group; later tools in the run fold into both. Paths
    /// that don't exist here are dropped.
    #[test]
    fn tool_images_become_a_shots_row() {
        let dir = std::env::temp_dir().join(format!("keel-turn-shot-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.png"), dir.join("b.png"));
        std::fs::write(&a, b"png").unwrap();
        std::fs::write(&b, b"png").unwrap();
        let (a, b) = (
            a.to_string_lossy().into_owned(),
            b.to_string_lossy().into_owned(),
        );
        let shot = |id: &str, path: &str| serde_json::json!({"kind": "tool", "tool_id": id, "name": "browser_screenshot", "state": "done", "summary": "", "images": [path, "/no/such/file.png"]});
        let view = serde_json::json!({"parts": [
            shot("1", &a),
            {"kind": "text", "id": "p", "body": ""},
            tool("2"),
        ]});
        let mut steps = merge_doc("m", Vec::new(), doc_steps(&view));
        parsed(&mut steps);
        let mut rows = Vec::new();
        push_rows("m", &steps, &|_| false, &mut rows);
        assert_eq!(kinds(&rows)[0], "tools2");
        assert_eq!(
            rows[1],
            TranscriptBlock::Shots {
                id: "m#tools0#shots".into(),
                paths: vec![a.clone()]
            }
        );
        // A second message's screenshot tool folds into the same group and row.
        let more = serde_json::json!({"parts": [shot("3", &b)]});
        let mut steps = merge_doc("m2", Vec::new(), doc_steps(&more));
        parsed(&mut steps);
        push_rows("m2", &steps, &|_| false, &mut rows);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(kinds(&rows)[0], "tools3");
        assert_eq!(
            rows[1],
            TranscriptBlock::Shots {
                id: "m#tools0#shots".into(),
                paths: vec![a, b]
            }
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn parts_keep_stream_order_and_fold_runs() {
        let view = serde_json::json!({"parts": [
            {"kind": "reasoning", "id": "r1", "body": "plan"},
            tool("1"),
            {"kind": "text", "id": "t1", "body": "Gathering branch "},
            {"kind": "text", "id": "t2", "body": "state."},
            {"kind": "reasoning", "id": "r2", "body": "more"},
            tool("2"),
            tool("3"),
            {"kind": "text", "id": "t3", "body": "Checking the diff."}
        ]});
        let doc = doc_steps(&view);
        assert_eq!(doc.len(), 6);
        assert_eq!(doc[2], DocStep::Text("Gathering branch state.".into()));
        let mut steps = merge_doc("m", Vec::new(), doc);
        parsed(&mut steps);
        let mut rows = Vec::new();
        push_rows("m", &steps, &|_| false, &mut rows);
        assert_eq!(
            kinds(&rows),
            [
                "think",
                "tools1",
                "Gathering branch state.",
                "think",
                "tools2",
                "Checking the diff."
            ]
        );
    }

    #[test]
    fn old_views_fall_back_to_thinking_tools_text() {
        let view = serde_json::json!({"text": "hi", "reasoning": "r", "tools": [{"tool_id": "1", "name": "bash", "state": "done", "summary": ""}]});
        let doc = doc_steps(&view);
        assert!(matches!(
            doc.as_slice(),
            [DocStep::Reasoning(_), DocStep::Tools(_), DocStep::Text(_)]
        ));
    }

    #[test]
    fn text_after_a_tool_streams_into_its_own_step() {
        let mut steps = Vec::new();
        push_reasoning(&mut steps, "thinking", false);
        push_text(&mut steps, "m", "First.", false);
        // The doc arrives with the tool call.
        let doc = vec![
            DocStep::Reasoning("thinking".into()),
            DocStep::Text("First.".into()),
            DocStep::Tools(ToolEntry::parse_list(
                &serde_json::json!({"tools": [tool("1")]}),
            )),
        ];
        steps = merge_doc("m", steps, doc);
        push_text(&mut steps, "m", "Second", false);
        push_text(&mut steps, "m", " part.", false);
        parsed(&mut steps);
        let mut rows = Vec::new();
        push_rows("m", &steps, &|_| false, &mut rows);
        assert_eq!(kinds(&rows), ["think", "First.", "tools1", "Second part."]);
    }

    #[test]
    fn a_tool_boundary_splits_text_before_the_doc_catches_up() {
        let mut steps = Vec::new();
        push_text(&mut steps, "m", "Before.", false);
        // ToolStarted seen: the next text is a new step.
        push_text(&mut steps, "m", "After", true);
        // A lagging doc only knows the first text so far.
        steps = merge_doc("m", steps, vec![DocStep::Text("Before.".into())]);
        push_text(&mut steps, "m", " more.", false);
        // Now the doc has everything, with the tool in between.
        let doc = vec![
            DocStep::Text("Before.".into()),
            DocStep::Tools(ToolEntry::parse_list(
                &serde_json::json!({"tools": [tool("1")]}),
            )),
            DocStep::Text("After".into()),
        ];
        steps = merge_doc("m", steps, doc);
        parsed(&mut steps);
        let mut rows = Vec::new();
        push_rows("m", &steps, &|_| false, &mut rows);
        // The live text that is ahead of the doc is kept, in its own place.
        assert_eq!(kinds(&rows), ["Before.", "tools1", "After more."]);
        assert_eq!(rows[2].id(), "m#t1#0");
    }

    #[test]
    fn tools_group_across_messages_of_a_turn() {
        let tools = ToolEntry::parse_list(&serde_json::json!({"tools": [tool("1")]}));
        let a = merge_doc("a", Vec::new(), vec![DocStep::Tools(tools.clone())]);
        let b = merge_doc(
            "b",
            Vec::new(),
            vec![DocStep::Tools(tools), DocStep::Text("done".into())],
        );
        let mut rows = Vec::new();
        push_rows("a", &a, &|_| false, &mut rows);
        push_rows("b", &parsed_steps(b), &|_| false, &mut rows);
        assert_eq!(kinds(&rows), ["tools2", "done"]);
        assert_eq!(rows[0].id(), "a#tools0");
    }

    fn parsed_steps(mut steps: Vec<Step>) -> Vec<Step> {
        parsed(&mut steps);
        steps
    }

    #[test]
    fn thinking_rows_in_a_row_are_one_thought() {
        let a = merge_doc("a", Vec::new(), vec![DocStep::Reasoning("first".into())]);
        let b = merge_doc(
            "b",
            Vec::new(),
            vec![
                DocStep::Reasoning("second".into()),
                DocStep::Text("done".into()),
            ],
        );
        let mut rows = Vec::new();
        push_rows("a", &a, &|_| false, &mut rows);
        push_rows("b", &parsed_steps(b), &|_| false, &mut rows);
        assert_eq!(kinds(&rows), ["think", "done"]);
        let TranscriptBlock::Reasoning { content, .. } = &rows[0] else {
            panic!()
        };
        assert_eq!(content, "first\n\nsecond");
    }

    #[test]
    fn live_text_the_doc_already_holds_is_not_shown_twice() {
        // A driver glued the second run onto the first part; the live step
        // that split there must not reappear after the tools.
        let mut steps = Vec::new();
        push_text(&mut steps, "m", "Looking it up.", false);
        push_text(&mut steps, "m", "Here is what I found.", true);
        let doc = vec![
            DocStep::Text("Looking it up.Here is what I found.".into()),
            DocStep::Tools(ToolEntry::parse_list(
                &serde_json::json!({"tools": [tool("1")]}),
            )),
        ];
        let steps = parsed_steps(merge_doc("m", steps, doc));
        let mut rows = Vec::new();
        push_rows("m", &steps, &|_| false, &mut rows);
        assert_eq!(
            kinds(&rows),
            ["Looking it up.Here is what I found.", "tools1"]
        );
    }
}

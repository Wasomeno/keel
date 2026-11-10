//! Generative UI. The agent calls `keel-ui` tools (`show_chart`,
//! `show_choices`, …) when a native card is worth showing. The tool prints a
//! spec; the engine stores it as a `ui` part (see `SessionDoc::put_ui`).
//! The host draws the card. The model never writes the widget, and a chart
//! only shows numbers the agent passed in the call.

use serde_json::{Value, json};

use keel_proto::UI_TOOLS;

const MAX_ROWS: usize = 50;
/// The MR card's spec. `offer_merge` shows the Merge button; the engine
/// still checks the MR live before merging.
pub(crate) fn mr_card(mr: &keel_proto::mr::MrRef, snap: &Value, offer_merge: bool) -> Value {
    json!({
        "component": "mr",
        "host": mr.host,
        "project": mr.project,
        "iid": mr.iid,
        "snap": snap,
        "offer_merge": offer_merge,
    })
}

const MAX_COMMANDS: usize = 4;
const MAX_STATS: usize = 3;
const MAX_FILES: usize = 20;
const MAX_STEPS: usize = 8;
const MAX_POINTS: usize = 4;
const MAX_CALLOUT: usize = 160;
const MAX_FAILURES: usize = 6;
const MAX_PLAN_STEPS: usize = 24;

/// Words that make a command risky whatever the agent says.
fn risky_words(command: &str) -> bool {
    const RISKY: [&str; 14] = [
        "rm -rf",
        "rm -r ",
        "sudo ",
        "git push",
        "--force",
        "git reset --hard",
        "git clean",
        "drop ",
        "deploy",
        "publish",
        "kubectl delete",
        "terraform apply",
        "mkfs",
        "> /dev/",
    ];
    let lower = command.to_ascii_lowercase();
    RISKY.iter().any(|word| lower.contains(word))
}

/// The tool list `keel mcp-ui` advertises. Descriptions tell the agent when
/// a card is worth calling; the schema is what it must pass.
pub fn ui_tools() -> Value {
    json!([
        {
            "name": "show_chart",
            "description": "Show a native bar or line chart in place of a markdown table you wrote in this reply. Call it only when the table compares a measured quantity (sizes, times, counts, costs, scores) across items, or shows one quantity changing over a sequence. Do not call it for ids, codes, ranks, versions, or status. The numbers must be the table's numbers.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": {"type": "string", "enum": ["bar", "line"], "description": "bar compares categories; line is a trend over an ordered sequence"},
                    "title": {"type": "string", "description": "Short title describing what the chart compares or trends, in sentence case"},
                    "unit": {"type": "string", "description": "Optional unit or symbol for y-axis values, e.g. $, %, KB, ms"},
                    "headline": {
                        "type": "object",
                        "properties": {
                            "value": {"type": "string", "description": "Primary aggregate or latest number, e.g. $142.5k"},
                            "label": {"type": "string", "description": "Short context label, e.g. Latest total"},
                            "delta": {"type": "string", "description": "Optional change, e.g. +14% or -2.4k"},
                            "better": {"type": "string", "enum": ["higher", "lower", "neither"], "description": "Direction considered positive for delta tinting"}
                        },
                        "required": ["value"]
                    },
                    "x": {"type": "string", "description": "The table column that labels the categories or the x-axis, exactly as written in the header"},
                    "labels": {"type": "array", "items": {"type": "string"}, "description": "That column's cells, in order"},
                    "series": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string", "description": "The numeric column's header"},
                                "values": {"type": "array", "items": {"type": ["number", "null"]}, "description": "That column's cells, in the same order; null for a gap"}
                            },
                            "required": ["name", "values"]
                        }
                    }
                },
                "required": ["kind", "x", "labels", "series"]
            }
        },
        {
            "name": "show_choices",
            "description": "Show choice buttons when this reply asks the user to pick one of 2 to 6 alternatives. Do not call it for steps, facts, or a report. A click sends that option as the user's next message.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "options": {"type": "array", "items": {"type": "string"}, "minItems": 2, "maxItems": 6, "description": "Button labels, in the order you listed them"},
                    "recommended": {"type": "integer", "description": "Index of the option you recommend, if you recommend one"}
                },
                "required": ["options"]
            }
        },
        {
            "name": "show_stats",
            "description": "Show before-and-after tiles for a measured outcome you wrote in this reply, such as build time from 41s to 28s. One tile per figure, at most three. Do not call it for a change you did not write as two numbers.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "items": {
                        "type": "array",
                        "maxItems": 3,
                        "items": {
                            "type": "object",
                            "properties": {
                                "label": {"type": "string"},
                                "before": {"type": "string", "description": "As written, such as 41s"},
                                "after": {"type": "string"},
                                "better": {"type": "string", "enum": ["lower", "higher", "neither"], "description": "Which direction is an improvement"}
                            },
                            "required": ["label", "before", "after", "better"]
                        }
                    }
                },
                "required": ["items"]
            }
        },
        {
            "name": "show_commands",
            "description": "Add a Run button to a shell block you wrote for the user to type. Do not call it for program output, a file's contents, or code in another language. Mark risky when the command deletes or overwrites data, changes remote or shared state, or needs admin rights.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "items": {
                        "type": "array",
                        "maxItems": 4,
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": {"type": "string", "description": "The fenced block's text, so the button finds that block"},
                                "run": {"type": "string", "description": "The command to type, without $ prompts or console output"},
                                "risky": {"type": "boolean"}
                            },
                            "required": ["content", "run"]
                        }
                    }
                },
                "required": ["items"]
            }
        },
        {
            "name": "show_files",
            "description": "Show the files this turn changed, when the reply is about those edits. Pass the paths you changed, relative to the working directory. The host fills status and line counts.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "paths": {"type": "array", "items": {"type": "string"}, "description": "Paths you created, edited, or deleted this turn"}
                },
                "required": ["paths"]
            }
        },
        {
            "name": "show_steps",
            "description": "Show a numbered step list. Use kind: 'task' (default) to show main task execution progress in the right session panel. Use kind: 'guide' to show how-to instructions in the chat reply. At most eight steps. current is the index of the step in progress; omit it when every step is done.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "items": {"type": "array", "items": {"type": "string"}, "minItems": 2, "maxItems": 8, "description": "Step labels, in order, one short line each"},
                    "current": {"type": "integer", "description": "Index of the step in progress. Omit when all are done"},
                    "kind": {"type": "string", "enum": ["task", "guide"], "description": "'task' (default) for main task execution steps in the right session panel; 'guide' for how-to instructions in chat"}
                },
                "required": ["items"]
            }
        },
        {
            "name": "task_steps",
            "description": "Show or update the main task execution steps in the right session panel. At most eight steps. current is the index of the step in progress; omit it when every step is done.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "items": {"type": "array", "items": {"type": "string"}, "minItems": 2, "maxItems": 8, "description": "Task step labels, in order, one short line each"},
                    "current": {"type": "integer", "description": "Index of the step in progress. Omit when all are done"}
                },
                "required": ["items"]
            }
        },
        {
            "name": "show_callout",
            "description": "Show one short card for the sentence this reply should be remembered by. Call it once, for a note, a warning, or the result. Do not call it to restate the reply, and do not put more than one sentence in it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": {"type": "string", "enum": ["note", "warning", "result"]},
                    "text": {"type": "string", "description": "One sentence, as written in the reply"}
                },
                "required": ["kind", "text"]
            }
        },
        {
            "name": "show_compare",
            "description": "Show two or three columns of short claims. The card is the user's copy of the comparison: do not also write it in your reply. Call it for options or before/after as claims, not numbers. Numbers belong on show_chart or show_stats. Mark recommended on at most one column.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "columns": {
                        "type": "array",
                        "minItems": 2,
                        "maxItems": 3,
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": {"type": "string"},
                                "points": {"type": "array", "items": {"type": "string"}, "maxItems": 4, "description": "Short claims, one line each"},
                                "recommended": {"type": "boolean"}
                            },
                            "required": ["title", "points"]
                        }
                    }
                },
                "required": ["columns"]
            }
        },
        {
            "name": "show_diagram",
            "description": "Show a diagram card drawn from Mermaid source: a flow, architecture, sequence, state machine, or data model. The card is the user's copy of the diagram: do not also write it, or a drawing of it, in your reply. Use Mermaid's own syntax (flowchart TD, sequenceDiagram, stateDiagram-v2, erDiagram, classDiagram); quote labels with punctuation, A[\"POST /auth/oidc\"], use <br/> for a second line, and label arrows with A -->|click| B. If the source does not parse, the error comes back: fix it and call again. Do not call it for a list or a table.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source": {"type": "string", "description": "Mermaid source, starting with the diagram type, such as flowchart TD"}
                },
                "required": ["source"]
            }
        },
        {
            "name": "show_tests",
            "description": "Show a test-result card for a run you reported in this reply. Pass the counts from that run, and the failing test names. Do not call it for a run you did not report, or invent counts.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "The command, such as cargo test. May be empty"},
                    "passed": {"type": "integer"},
                    "failed": {"type": "integer"},
                    "skipped": {"type": "integer"},
                    "failures": {"type": "array", "items": {"type": "string"}, "description": "Failing test names, at most six"}
                },
                "required": ["passed", "failed"]
            }
        },
        {
            "name": "show_mr",
            "description": "Show a merge request card when this reply is about that merge request: opening it, updating it, or reporting where it stands. Do not call it for a mention in passing. offer_merge only when you say it is done and ready to merge now. The host checks GitLab before merging.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": {"type": "string", "description": "The merge request URL you wrote, or a !42 mention"},
                    "offer_merge": {"type": "boolean"}
                },
                "required": ["url"]
            }
        },
        {
            "name": "show_plan",
            "description": "Show one plan card and stop. Call it only in plan mode, after you have researched with read-only tools. Exactly one call. The card is the user's copy of the plan: do not also write the steps in your reply.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": {"type": "string", "description": "Short plan title, sentence case"},
                    "summary": {"type": "string", "description": "One or two sentences on what the plan does"},
                    "steps": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": {"type": "string"},
                                "kind": {"type": "string", "enum": ["read", "edit", "run", "other"]},
                                "targets": {"type": "array", "items": {"type": "string"}, "description": "Paths or commands this step touches"}
                            },
                            "required": ["title", "kind"]
                        }
                    },
                    "footer": {"type": "string", "description": "Touched files or external effects, one line"}
                },
                "required": ["title", "summary", "steps", "footer"]
            }
        }
    ])
}

/// The card spec a tool call prints. `None` for another tool. `Some(Err)`
/// is a reason the agent can correct. The engine stores an `Ok` string as
/// the `ui` part. Numbers are the agent's; the host drops a spec its card
/// cannot draw.
pub fn ui_call(name: &str, args: &Value) -> Option<Result<String, String>> {
    let tool = UI_TOOLS
        .iter()
        .find(|tool| name == **tool || name.ends_with(*tool))?;
    let spec = match *tool {
        "show_chart" => chart_spec(args),
        "show_choices" => choices_spec(args),
        "show_stats" => stats_spec(args),
        "show_commands" => commands_spec(args),
        "show_files" => files_spec(args),
        "show_mr" => mr_call_spec(args),
        "show_steps" => steps_spec(args),
        "task_steps" => steps_spec(args),
        "show_callout" => callout_spec(args),
        "show_compare" => compare_spec(args),
        "show_diagram" => diagram_spec(args),
        "show_tests" => tests_spec(args),
        "show_plan" => plan_spec(args),
        _ => return None,
    };
    // The spec stays the last JSON line: the engine reads the card from it.
    Some(spec.map(|spec| match *tool {
        "show_steps" | "task_steps" | "show_compare" | "show_diagram" | "show_plan" => {
            format!("Shown to the user as a card. Do not repeat it in your reply.\n{spec}")
        }
        _ => spec.to_string(),
    }))
}

fn plan_kind(raw: &str) -> &'static str {
    match raw.trim() {
        "read" => "read",
        "edit" => "edit",
        "run" => "run",
        _ => "other",
    }
}

fn plan_spec(args: &Value) -> Result<Value, String> {
    let title = args["title"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("a plan needs a title")?;
    let summary = args["summary"].as_str().unwrap_or("").trim();
    let footer = args["footer"].as_str().unwrap_or("").trim();
    let steps: Vec<Value> = args["steps"]
        .as_array()
        .ok_or("a plan needs at least one step")?
        .iter()
        .filter_map(|step| {
            let title = step["title"].as_str().map(str::trim).filter(|s| !s.is_empty())?;
            let kind = plan_kind(step["kind"].as_str().unwrap_or("other"));
            let targets: Vec<String> = step["targets"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|v| v.as_str().map(str::trim).filter(|s| !s.is_empty()))
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            Some(json!({"title": title, "kind": kind, "targets": targets}))
        })
        .take(MAX_PLAN_STEPS)
        .collect();
    if steps.is_empty() {
        return Err("a plan needs at least one step".into());
    }
    Ok(json!({
        "component": "plan",
        "title": title,
        "summary": summary,
        "steps": steps,
        "footer": footer,
    }))
}

fn chart_spec(args: &Value) -> Result<Value, String> {
    let labels = strings(&args["labels"])
        .filter(|l| (2..=MAX_ROWS).contains(&l.len()))
        .ok_or("a chart needs 2 to 50 labels")?;
    let series: Vec<Value> = args["series"]
        .as_array()
        .ok_or("series is required")?
        .iter()
        .filter_map(|s| {
            let values = s["values"].as_array()?;
            if values.len() != labels.len() {
                return None;
            }
            let values: Vec<Value> = values
                .iter()
                .map(|v| {
                    v.as_f64()
                        .filter(|n| n.is_finite())
                        .map(|n| json!(n))
                        .unwrap_or(Value::Null)
                })
                .collect();
            if values.iter().filter(|v| !v.is_null()).count() < 2 {
                return None;
            }
            Some(json!({"name": s["name"].as_str().unwrap_or_default(), "values": values}))
        })
        .collect();
    if series.is_empty() {
        return Err("a series needs two numbers, in the same order as the labels".into());
    }
    let kind = if args["kind"] == "line" {
        "line"
    } else {
        "bar"
    };
    let mut spec = json!({
        "component": "chart",
        "kind": kind,
        "x": args["x"].as_str().unwrap_or_default(),
        "labels": labels,
        "series": series,
    });
    if let Some(title) = args["title"].as_str().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        spec["title"] = json!(title);
    }
    if let Some(unit) = args["unit"].as_str().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        spec["unit"] = json!(unit);
    }
    if let Some(obj) = args["headline"].as_object() {
        if let Some(val) = obj.get("value").and_then(|v| v.as_str()).map(|s| s.trim()).filter(|s| !s.is_empty()) {
            let mut h = json!({ "value": val });
            if let Some(label) = obj.get("label").and_then(|v| v.as_str()).map(|s| s.trim()).filter(|s| !s.is_empty()) {
                h["label"] = json!(label);
            }
            if let Some(delta) = obj.get("delta").and_then(|v| v.as_str()).map(|s| s.trim()).filter(|s| !s.is_empty()) {
                h["delta"] = json!(delta);
            }
            if let Some(better) = obj.get("better").and_then(|v| v.as_str()).map(|s| s.trim()).filter(|s| !s.is_empty()) {
                h["better"] = json!(better);
            }
            spec["headline"] = h;
        }
    }
    Ok(spec)
}

fn choices_spec(args: &Value) -> Result<Value, String> {
    let options = strings(&args["options"])
        .filter(|o| (2..=6).contains(&o.len()))
        .ok_or("choices need 2 to 6 options")?;
    let recommended = args["recommended"]
        .as_u64()
        .map(|ix| ix as usize)
        .filter(|&ix| ix < options.len());
    Ok(json!({"component": "choices", "options": options, "recommended": recommended}))
}

fn stats_spec(args: &Value) -> Result<Value, String> {
    let items: Vec<Value> = args["items"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|item| {
            let (before, after) = (item["before"].as_str()?, item["after"].as_str()?);
            let (from, to) = (number(before)?, number(after)?);
            if from == to {
                return None;
            }
            let better = match item["better"].as_str() {
                Some("lower" | "higher") => item["better"].as_str().unwrap(),
                _ => "neither",
            };
            Some(json!({
                "label": item["label"].as_str().unwrap_or_default(),
                "before": before,
                "after": after,
                "change": (from != 0.).then(|| (to - from) / from.abs() * 100.),
                "better": better,
            }))
        })
        .take(MAX_STATS)
        .collect();
    if items.is_empty() {
        return Err("a stat needs two different figures, as written".into());
    }
    Ok(json!({"component": "stats", "items": items}))
}

fn commands_spec(args: &Value) -> Result<Value, String> {
    let items: Vec<Value> = args["items"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|item| {
            let (content, run) = (
                item["content"].as_str()?.trim(),
                item["run"].as_str()?.trim(),
            );
            if content.is_empty() || run.is_empty() || run.lines().count() > 12 {
                return None;
            }
            let risky = item["risky"] == true || risky_words(run);
            Some(json!({"content": content, "run": run, "risky": risky}))
        })
        .take(MAX_COMMANDS)
        .collect();
    if items.is_empty() {
        return Err("a command needs the block text and the command to type".into());
    }
    Ok(json!({"component": "commands", "items": items}))
}

fn files_spec(args: &Value) -> Result<Value, String> {
    let paths = strings(&args["paths"])
        .filter(|p| !p.is_empty())
        .ok_or("files needs the paths this turn changed")?;
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    // Host fills git status and line counts against HEAD
    let mut stats: std::collections::HashMap<String, (char, Option<u32>, Option<u32>)> =
        std::collections::HashMap::new();

    if let Ok(numstat) = crate::repo::run_git_cmd_sync(&cwd, &["diff", "HEAD", "--numstat", "-z"])
        .or_else(|_| crate::repo::run_git_cmd_sync(&cwd, &["diff", "--numstat", "-z"]))
    {
        let mut records = numstat.split('\0');
        while let Some(record) = records.next() {
            let mut fields = record.splitn(3, '\t');
            let (Some(added), Some(removed), Some(path)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let path = if path.is_empty() {
                records.next();
                records.next().unwrap_or_default()
            } else {
                path
            };
            stats.insert(
                path.to_string(),
                ('M', added.parse().ok(), removed.parse().ok()),
            );
        }
    }

    if let Ok(status_out) = crate::repo::run_git_cmd_sync(&cwd, &["status", "--porcelain", "-z"]) {
        let mut entries = status_out.split('\0');
        while let Some(entry) = entries.next() {
            if entry.len() < 4 {
                continue;
            }
            let xy = &entry[..2];
            let path = entry[3..].to_string();
            let status = if xy.contains('?') {
                '?'
            } else if xy.contains('D') {
                'D'
            } else if xy.contains('A') {
                'A'
            } else {
                'M'
            };
            if let Some(s) = stats.get_mut(&path) {
                s.0 = status;
            } else {
                let lines = if status == '?' || status == 'A' {
                    std::fs::read_to_string(cwd.join(&path))
                        .ok()
                        .map(|c| c.lines().count() as u32)
                } else {
                    None
                };
                stats.insert(path, (status, lines, Some(0)));
            }
        }
    }

    let files: Vec<Value> = paths
        .into_iter()
        .take(MAX_FILES)
        .map(|path| {
            let norm_path = path.trim_start_matches("./");
            let (status, added, removed) = stats
                .get(norm_path)
                .or_else(|| stats.get(&path))
                .copied()
                .unwrap_or(('M', None, None));
            let mut obj = json!({
                "path": path,
                "status": status.to_string(),
            });
            if let Some(a) = added {
                obj["added"] = json!(a);
            }
            if let Some(r) = removed {
                obj["removed"] = json!(r);
            }
            obj
        })
        .collect();

    let root = cwd.to_string_lossy().into_owned();
    Ok(json!({"component": "files", "root": root, "files": files, "more": 0}))
}

fn steps_spec(args: &Value) -> Result<Value, String> {
    let items = strings(&args["items"])
        .filter(|s| (2..=MAX_STEPS).contains(&s.len()))
        .ok_or("steps need 2 to 8 items")?;
    let current = args["current"]
        .as_u64()
        .map(|ix| ix as usize)
        .filter(|&ix| ix < items.len());
    let kind = match args["kind"].as_str() {
        Some("guide") => "guide",
        _ => "task",
    };
    Ok(json!({"component": "steps", "items": items, "current": current, "kind": kind}))
}

fn callout_spec(args: &Value) -> Result<Value, String> {
    let text = args["text"].as_str().unwrap_or("").trim();
    if text.is_empty() || text.chars().count() > MAX_CALLOUT {
        return Err("a callout is one sentence, at most 160 characters".into());
    }
    let kind = match args["kind"].as_str() {
        Some("warning") => "warning",
        Some("result") => "result",
        _ => "note",
    };
    Ok(json!({"component": "callout", "kind": kind, "text": text}))
}

fn compare_spec(args: &Value) -> Result<Value, String> {
    let columns: Vec<Value> = args["columns"].as_array().unwrap_or(&Vec::new()).iter().filter_map(|col| {
        let title = col["title"].as_str()?.trim();
        let points = strings(&col["points"]).filter(|p| !p.is_empty())?;
        if title.is_empty() || points.is_empty() {
            return None;
        }
        Some(json!({"title": title, "points": points.into_iter().take(MAX_POINTS).collect::<Vec<_>>(), "recommended": col["recommended"] == true}))
    }).take(3).collect();
    if columns.len() < 2 {
        return Err("a comparison needs two columns".into());
    }
    Ok(json!({"component": "compare", "columns": columns}))
}

/// Mermaid source the host can draw. It is rendered here the way the card
/// renders it (merman, resvg-safe SVG), so a card that is stored draws, and a
/// source that doesn't goes back to the agent with merman's reason.
fn diagram_spec(args: &Value) -> Result<Value, String> {
    use merman::svg::SvgPipeline;
    use merman::{OperationControl, RenderOutput, RenderRequest, Renderer, SvgRequest};
    let source = args["source"].as_str().unwrap_or("").trim();
    if source.is_empty() {
        return Err("source must be Mermaid, such as flowchart TD".into());
    }
    let request = SvgRequest {
        pipeline: Some(SvgPipeline::resvg_safe()),
        ..SvgRequest::default()
    };
    match Renderer::new().render(RenderRequest::svg(source, OperationControl::new(), request)) {
        Ok(RenderOutput::Svg(Some(_))) => {
            Ok(json!({"component": "diagram", "format": "mermaid", "source": source}))
        }
        Ok(_) => Err(
            "Mermaid drew nothing from this source; check the diagram type on the first line"
                .into(),
        ),
        Err(err) => Err(format!(
            "Mermaid could not draw this: {err}. Fix the source and call again."
        )),
    }
}

fn tests_spec(args: &Value) -> Result<Value, String> {
    let n = |key: &str| args[key].as_u64().unwrap_or(0);
    let (passed, failed, skipped) = (n("passed"), n("failed"), n("skipped"));
    if passed + failed + skipped == 0 {
        return Err("a test card needs the run's counts".into());
    }
    let failures = strings(&args["failures"])
        .unwrap_or_default()
        .into_iter()
        .take(MAX_FAILURES)
        .collect::<Vec<_>>();
    Ok(json!({
        "component": "tests",
        "command": args["command"].as_str().unwrap_or("").trim(),
        "passed": passed,
        "failed": failed,
        "skipped": skipped,
        "failures": failures,
    }))
}

fn mr_call_spec(args: &Value) -> Result<Value, String> {
    let url = args["url"].as_str().unwrap_or("").trim();
    let mr = keel_proto::mr::parse_url(url).ok_or("url must be a merge request link, like https://gitlab.com/group/project/-/merge_requests/42")?;
    Ok(mr_card(&mr, &json!({}), args["offer_merge"] == true))
}

/// A figure as written (`41s`, `1,024`, `$3.50`, `12%`). Not a version or prose.
fn number(cell: &str) -> Option<f64> {
    let s = cell.trim().trim_start_matches(['$', '€', '£', '~', '+']);
    let end = s.find(|ch: char| !(ch.is_ascii_digit() || ch == '.' || ch == ',' || ch == '-'))?;
    if end == 0 {
        return None;
    }
    let unit = s[end..].trim();
    if unit.len() > 4 || unit.chars().any(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let scale = match unit {
        "k" | "K" => 1e3,
        "M" => 1e6,
        _ => 1.,
    };
    s[..end]
        .replace(',', "")
        .parse::<f64>()
        .ok()
        .map(|v| v * scale)
        .filter(|v| v.is_finite())
}

fn strings(value: &Value) -> Option<Vec<String>> {
    Some(
        value
            .as_array()?
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_chart_call_becomes_the_same_spec_a_table_would() {
        let spec = ui_call(
            "show_chart",
            &json!({
                "kind": "bar",
                "x": "crate",
                "labels": ["ui", "engine"],
                "series": [
                    {"name": "before (s)", "values": [41.2, 33.5]},
                    {"name": "after (s)", "values": [28.7, 30.1]}
                ]
            }),
        )
        .unwrap()
        .unwrap();
        let spec: Value = serde_json::from_str(&spec).unwrap();
        assert_eq!(spec["component"], "chart");
        assert_eq!(spec["labels"], json!(["ui", "engine"]));
        assert_eq!(
            spec["series"][1],
            json!({"name": "after (s)", "values": [28.7, 30.1]})
        );
        // A gap is null, never zero. A one-number series is not a chart.
        let gapped = ui_call(
            "mcp__keel-ui__show_chart",
            &json!({
                "kind": "line", "x": "route", "labels": ["/", "/p"],
                "series": [{"name": "gzip", "values": [410, null]}]
            }),
        );
        assert!(gapped.unwrap().is_err());
        assert!(ui_call("bash", &json!({})).is_none());

        let with_meta = ui_call(
            "show_chart",
            &json!({
                "kind": "bar",
                "title": "Revenue and costs by month",
                "unit": "$k",
                "headline": {
                    "value": "$142.5k",
                    "label": "Total revenue",
                    "delta": "+14%",
                    "better": "higher"
                },
                "x": "Month",
                "labels": ["Jan", "Feb"],
                "series": [
                    {"name": "Revenue", "values": [42.0, 55.0]}
                ]
            }),
        )
        .unwrap()
        .unwrap();
        let with_meta: Value = serde_json::from_str(&with_meta).unwrap();
        assert_eq!(with_meta["title"], "Revenue and costs by month");
        assert_eq!(with_meta["unit"], "$k");
        assert_eq!(with_meta["headline"]["value"], "$142.5k");
        assert_eq!(with_meta["headline"]["delta"], "+14%");
    }

    #[test]
    fn a_choice_call_keeps_only_a_real_pick() {
        let spec: Value = serde_json::from_str(
            &ui_call(
                "show_choices",
                &json!({
                    "options": ["Redis", "Memory"], "recommended": 1
                }),
            )
            .unwrap()
            .unwrap(),
        )
        .unwrap();
        assert_eq!(spec["recommended"], 1);
        assert!(
            ui_call("show_choices", &json!({"options": ["only"]}))
                .unwrap()
                .is_err()
        );
        let out = ui_call(
            "show_choices",
            &json!({"options": ["a", "b"], "recommended": 5}),
        )
        .unwrap()
        .unwrap();
        assert!(serde_json::from_str::<Value>(&out).unwrap()["recommended"].is_null());
    }

    #[test]
    fn a_stat_call_carries_the_change_the_reply_wrote() {
        let spec: Value = serde_json::from_str(&ui_call("show_stats", &json!({
            "items": [{"label": "Build time", "before": "40s", "after": "30s", "better": "lower"}]
        })).unwrap().unwrap()).unwrap();
        assert_eq!(spec["items"][0]["change"], json!(-25.0));
        assert!(
            ui_call(
                "show_stats",
                &json!({"items": [{"label": "x", "before": "1", "after": "1", "better": "lower"}]})
            )
            .unwrap()
            .is_err()
        );
    }

    #[test]
    fn a_command_call_marks_a_push_risky_even_if_the_agent_does_not() {
        let spec: Value = serde_json::from_str(
            &ui_call(
                "show_commands",
                &json!({
                    "items": [
                        {"content": "$ cargo t\ntest result: ok", "run": "cargo t"},
                        {"content": "git push --force", "run": "git push --force", "risky": false}
                    ]
                }),
            )
            .unwrap()
            .unwrap(),
        )
        .unwrap();
        assert_eq!(spec["items"][0]["risky"], false);
        assert_eq!(spec["items"][1]["risky"], true);
    }

    #[test]
    fn ui_tool_names_match_with_a_harness_prefix() {
        assert!(keel_proto::is_ui_tool("show_chart"));
        assert!(keel_proto::is_ui_tool("mcp__keel-ui__show_choices"));
        assert!(keel_proto::is_ui_tool("mcp__keel_ui__show_mr"));
        assert!(!keel_proto::is_ui_tool("bash"));
        assert!(!keel_proto::is_ui_tool("show_chart_extra"));
    }

    /// The spec a call printed: its last line, as the engine reads it.
    fn card(out: Option<Result<String, String>>) -> Value {
        serde_json::from_str(out.unwrap().unwrap().lines().last().unwrap()).unwrap()
    }

    #[test]
    fn explain_cards_keep_a_real_shape_and_drop_a_restatement() {
        let steps = card(ui_call(
            "show_steps",
            &json!({
                "items": ["Read the test", "Close the watcher", "Rerun it"], "current": 1
            }),
        ));
        assert_eq!(steps["current"], 1);
        assert_eq!(steps["kind"], "task");
        let guide_steps = card(ui_call(
            "show_steps",
            &json!({"items": ["1", "2"], "kind": "guide"}),
        ));
        assert_eq!(guide_steps["kind"], "guide");
        let task_card = card(ui_call("task_steps", &json!({"items": ["1", "2"]})));
        assert_eq!(task_card["kind"], "task");
        assert!(
            ui_call("show_steps", &json!({"items": ["only"]}))
                .unwrap()
                .is_err()
        );
        assert!(
            ui_call("show_steps", &json!({"items": ["a", "b"], "current": 9}))
                .unwrap()
                .unwrap()
                .contains("\"current\":null")
        );

        let note: Value = serde_json::from_str(
            &ui_call(
                "show_callout",
                &json!({
                    "kind": "warning", "text": "  Closing the cache drops in-flight reads.  "
                }),
            )
            .unwrap()
            .unwrap(),
        )
        .unwrap();
        assert_eq!(note["text"], "Closing the cache drops in-flight reads.");
        assert!(
            ui_call(
                "show_callout",
                &json!({"kind": "note", "text": "x".repeat(161)})
            )
            .unwrap()
            .is_err()
        );

        let compared = card(ui_call(
            "show_compare",
            &json!({
                "columns": [
                    {"title": "Redis", "points": ["Survives a restart"], "recommended": true},
                    {"title": "Memory", "points": ["Nothing to run"]}
                ]
            }),
        ));
        assert_eq!(compared["columns"][0]["recommended"], true);
        assert!(
            ui_call(
                "show_compare",
                &json!({"columns": [{"title": "Only", "points": ["a"]}]})
            )
            .unwrap()
            .is_err()
        );

        let drawn = "flowchart TD\n  A[\"/auth/login\"] -->|click| B{success?}\n  B -->|yes| C[Dashboard<br/>with permissions]";
        let diagram = card(ui_call("show_diagram", &json!({"source": drawn})));
        assert_eq!(diagram["format"], "mermaid");
        assert!(
            ui_call("show_diagram", &json!({"source": drawn}))
                .unwrap()
                .unwrap()
                .starts_with("Shown to the user")
        );
        // A source Mermaid can't parse goes back with the reason, so the agent fixes it.
        let broken = ui_call(
            "show_diagram",
            &json!({"source": "flowchart TD\n  A[unclosed --> B"}),
        )
        .unwrap()
        .unwrap_err();
        assert!(
            broken.starts_with("Mermaid could not draw this"),
            "{broken}"
        );
        assert!(
            ui_call("show_diagram", &json!({"source": "┌───┐\n│ A │\n└───┘"}))
                .unwrap()
                .is_err(),
            "box art is not Mermaid"
        );
        assert_eq!(diagram["component"], "diagram");
        assert!(
            ui_call("show_diagram", &json!({"source": "just words"}))
                .unwrap()
                .is_err()
        );

        let tests: Value = serde_json::from_str(&ui_call("show_tests", &json!({
            "command": "cargo test", "passed": 170, "failed": 2, "failures": ["session::drop_closes_watcher"]
        })).unwrap().unwrap()).unwrap();
        assert_eq!(tests["failed"], 2);
        assert!(
            ui_call("show_tests", &json!({"passed": 0, "failed": 0}))
                .unwrap()
                .is_err()
        );
        assert!(keel_proto::is_ui_tool("mcp__keel-ui__show_callout"));
    }

    #[test]
    fn a_plan_spec_round_trips_and_a_spec_without_title_or_steps_is_rejected() {
        let spec = card(ui_call(
            "show_plan",
            &json!({
                "title": "Ship plan mode",
                "summary": "Add a plan card and keep tools read-only until it is approved.",
                "footer": "Touches the composer and the harness gate.",
                "steps": [
                    {"title": "Read the composer", "kind": "read", "targets": ["crates/ui/src/view/composer_view.rs"]},
                    {"title": "Add the card", "kind": "edit", "targets": ["crates/ui/src/transcript.rs"]},
                    {"title": "Run the tests", "kind": "run", "targets": ["cargo test -p keel-ui"]},
                    {"title": "Note the limit", "kind": "nope", "targets": []}
                ]
            }),
        ));
        assert_eq!(spec["component"], "plan");
        assert_eq!(spec["title"], "Ship plan mode");
        assert_eq!(spec["steps"][0]["kind"], "read");
        assert_eq!(spec["steps"][3]["kind"], "other");
        assert!(
            ui_call("show_plan", &json!({"summary": "x", "steps": [{"title": "A", "kind": "read"}]}))
                .unwrap()
                .unwrap_err()
                .contains("title")
        );
        assert!(
            ui_call("show_plan", &json!({"title": "T", "steps": []}))
                .unwrap()
                .unwrap_err()
                .contains("step")
        );
        assert!(
            ui_call("show_plan", &json!({"title": "T", "steps": [{"title": "  ", "kind": "read"}]}))
                .unwrap()
                .is_err()
        );
        assert!(ui_call("show_chart", &json!({"kind": "bar"})).is_some());
        let chart = ui_call(
            "show_chart",
            &json!({"kind": "bar", "x": "c", "labels": ["a", "b"], "series": [{"name": "n", "values": [1, 2]}]}),
        )
        .unwrap()
        .unwrap();
        assert!(!chart.contains("\"component\":\"plan\""));
    }
}

//! Incremental message pushes.
//!
//! A message push used to carry the whole message JSON every time (each fold
//! flush and tool event), so a streaming reply cost O(message) per push and a
//! message past the 1 MiB frame cap stopped updating. Each subscriber now
//! remembers what it last received for a message, and a push sends one or more
//! frames with only what changed:
//!
//! ```json
//! { "id", "role", "created_at", "attachments",
//!   "parts_from": k,      // parts before k are unchanged
//!   "parts_len": n,       // the message has n parts after this frame
//!   "body_from": b,       // optional: parts[0] appends to part k's body at byte b
//!   "parts": [ ... ] }
//! ```
//!
//! A first push is the same frame with `parts_from: 0`. Anything too big for
//! one frame is split across several, each applying on top of the previous.
//! The client side is `keel_ui::turn::apply_message_frame`.

use serde_json::{Map, Value, json};

/// Frames stay well under `keel_rpc::MAX_FRAME_BYTES` (1 MiB) after the
/// stream envelope and JSON escaping are added.
pub(crate) const FRAME_BUDGET: usize = 768 * 1024;
/// Raw bytes of body per chunk when one part alone is over budget. Escaping
/// can grow text (quotes, newlines, control characters), so this is a quarter
/// of the budget.
const BODY_CHUNK: usize = FRAME_BUDGET / 4;
/// A tool summary over this is cut in the push (the doc keeps it whole).
const SUMMARY_CAP: usize = BODY_CHUNK;
/// Messages remembered per subscriber; older ones get a whole push again.
pub(crate) const REMEMBERED: usize = 64;

/// What one subscriber last received for a message.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Pushed {
    role: Value,
    attachments: Value,
    parts: Vec<Value>,
}

impl Pushed {
    pub(crate) fn of(full: &Value) -> Self {
        Self {
            role: full.get("role").cloned().unwrap_or(Value::Null),
            attachments: full.get("attachments").cloned().unwrap_or(Value::Null),
            // Capped like the frames, so a capped part compares equal next time.
            parts: full
                .get("parts")
                .and_then(Value::as_array)
                .map(|parts| parts.iter().map(cap_summary).collect())
                .unwrap_or_default(),
        }
    }
}

/// The frames that take a subscriber from `prev` to `full` (a `message_json`
/// value). Empty when nothing changed.
pub(crate) fn message_frames(prev: Option<&Pushed>, full: &Value) -> Vec<Value> {
    let parts: Vec<Value> = full
        .get("parts")
        .and_then(Value::as_array)
        .map(|parts| parts.iter().map(cap_summary).collect())
        .unwrap_or_default();
    let empty = Pushed::default();
    let old = prev.unwrap_or(&empty);
    let same_meta = prev.is_some()
        && full.get("role").unwrap_or(&Value::Null) == &old.role
        && full.get("attachments").unwrap_or(&Value::Null) == &old.attachments;
    let mut from = old
        .parts
        .iter()
        .zip(&parts)
        .take_while(|(a, b)| a == b)
        .count();
    if prev.is_some() && same_meta && from == parts.len() && from == old.parts.len() {
        return Vec::new();
    }
    let mut body_from = None;
    if let (Some(was), Some(now)) = (old.parts.get(from), parts.get(from))
        && let Some(offset) = appended(was, now)
    {
        body_from = Some(offset);
    }
    if prev.is_none() {
        from = 0;
        body_from = None;
    }
    let mut frames = Vec::new();
    let head = |from: usize, len: usize, body_from: Option<usize>, slice: Vec<Value>| {
        let mut frame = Map::new();
        for key in ["id", "role", "created_at", "attachments"] {
            frame.insert(key.into(), full.get(key).cloned().unwrap_or(Value::Null));
        }
        frame.insert("parts_from".into(), json!(from));
        frame.insert("parts_len".into(), json!(len));
        if let Some(offset) = body_from {
            frame.insert("body_from".into(), json!(offset));
        }
        frame.insert("parts".into(), Value::Array(slice));
        Value::Object(frame)
    };
    // The tail as one frame when it fits.
    let mut tail: Vec<Value> = parts[from..].to_vec();
    if let (Some(offset), Some(first)) = (body_from, tail.first_mut()) {
        let body = first["body"].as_str().unwrap_or_default();
        first["body"] = Value::String(body[offset..].to_string());
    }
    let whole = head(from, parts.len(), body_from, tail);
    if size(&whole) <= FRAME_BUDGET {
        frames.push(whole);
        return frames;
    }
    // Too big: one part per frame, and long bodies in chunks.
    for index in from..parts.len() {
        let part = &parts[index];
        let start = if index == from { body_from } else { None };
        let body = part.get("body").and_then(Value::as_str);
        match body {
            Some(body) if size(part) > FRAME_BUDGET || start.is_some() => {
                let mut offset = start.unwrap_or(0);
                let mut first = start.is_none();
                // A fresh part starts with its first chunk replacing; the rest append.
                loop {
                    let end = char_floor(body, (offset + BODY_CHUNK).min(body.len()));
                    let mut piece = part.clone();
                    piece["body"] = Value::String(body[offset..end].to_string());
                    let frame = if first {
                        head(index, index + 1, None, vec![piece])
                    } else {
                        head(index, index + 1, Some(offset), vec![piece])
                    };
                    frames.push(frame);
                    first = false;
                    offset = end;
                    if offset >= body.len() {
                        break;
                    }
                }
            }
            _ => frames.push(head(index, index + 1, None, vec![part.clone()])),
        }
    }
    // Parts dropped at the end (the new message is shorter) or none sent at all.
    if frames.is_empty() || parts.len() < old.parts.len() {
        frames.push(head(parts.len(), parts.len(), None, Vec::new()));
    }
    frames
}

/// The byte offset where `now` continues `was`: the same text or thinking part
/// whose body only grew.
fn appended(was: &Value, now: &Value) -> Option<usize> {
    let kind = was.get("kind")?.as_str()?;
    if !matches!(kind, "text" | "reasoning") || now.get("kind") != was.get("kind") || now.get("id") != was.get("id") {
        return None;
    }
    let (old, new) = (was.get("body")?.as_str()?, now.get("body")?.as_str()?);
    (new.len() > old.len() && new.starts_with(old)).then_some(old.len())
}

fn cap_summary(part: &Value) -> Value {
    match part.get("summary").and_then(Value::as_str) {
        Some(summary) if summary.len() > SUMMARY_CAP => {
            let mut part = part.clone();
            let cut = char_floor(summary, SUMMARY_CAP);
            part["summary"] = Value::String(format!("{}\n… (truncated)", &summary[..cut]));
            part
        }
        _ => part.clone(),
    }
}

fn char_floor(text: &str, mut index: usize) -> usize {
    while index < text.len() && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn size(value: &Value) -> usize {
    serde_json::to_string(value).map_or(usize::MAX, |s| s.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(parts: Vec<Value>) -> Value {
        json!({ "id": "m1", "role": "assistant", "created_at": 1, "attachments": [], "parts": parts })
    }

    fn text(body: &str) -> Value {
        json!({ "kind": "text", "id": "p1", "body": body })
    }

    /// The client merge, as `keel_ui::turn::apply_message_frame` does it.
    fn apply(stored: Option<Value>, frame: &Value) -> Option<Value> {
        let from = frame["parts_from"].as_u64()? as usize;
        let mut view = match stored {
            Some(view) => view,
            None if from == 0 && frame.get("body_from").is_none() => json!({ "parts": [] }),
            None => return None,
        };
        let mut parts = view["parts"].as_array().cloned()?;
        let mut incoming = frame["parts"].as_array().cloned()?;
        if let Some(offset) = frame.get("body_from").and_then(Value::as_u64) {
            let target = parts.get_mut(from)?;
            let body = target["body"].as_str()?.to_string();
            if body.len() != offset as usize || incoming.is_empty() {
                return None;
            }
            let more = incoming.remove(0);
            target["body"] = Value::String(body + more["body"].as_str()?);
            parts.truncate(from + 1);
        } else {
            if parts.len() < from {
                return None;
            }
            parts.truncate(from);
        }
        parts.extend(incoming);
        if parts.len() != frame["parts_len"].as_u64()? as usize {
            return None;
        }
        for key in ["id", "role", "created_at", "attachments"] {
            view[key] = frame[key].clone();
        }
        view["parts"] = Value::Array(parts);
        Some(view)
    }

    fn replay(prev: Option<&Value>, full: &Value) -> Value {
        let frames = message_frames(prev.map(Pushed::of).as_ref(), full);
        let mut view = prev.cloned();
        for frame in &frames {
            assert!(size(frame) <= FRAME_BUDGET, "frame of {} bytes", size(frame));
            view = Some(apply(view, frame).expect("frame applies"));
        }
        view.expect("a view")
    }

    #[test]
    fn first_push_is_whole_and_unchanged_is_silent() {
        let full = message(vec![text("hello")]);
        let frames = message_frames(None, &full);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0]["parts_from"], 0);
        assert_eq!(replay(None, &full)["parts"], full["parts"]);
        assert!(message_frames(Some(&Pushed::of(&full)), &full).is_empty());
    }

    #[test]
    fn growing_text_sends_only_the_suffix() {
        let before = message(vec![text("hello")]);
        let after = message(vec![text("hello world")]);
        let frames = message_frames(Some(&Pushed::of(&before)), &after);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0]["body_from"], 5);
        assert_eq!(frames[0]["parts"], json!([{ "kind": "text", "id": "p1", "body": " world" }]));
        assert_eq!(replay(Some(&before), &after)["parts"], after["parts"]);
    }

    #[test]
    fn new_and_changed_parts_send_from_the_first_difference() {
        let tool = |state: &str| json!({ "kind": "tool", "tool_id": "t1", "name": "bash", "state": state, "summary": "ls", "images": [] });
        let before = message(vec![text("a"), tool("running")]);
        let after = message(vec![text("a"), tool("done"), json!({ "kind": "text", "id": "p2", "body": "b" })]);
        let frames = message_frames(Some(&Pushed::of(&before)), &after);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0]["parts_from"], 1);
        assert_eq!(frames[0]["parts"].as_array().unwrap().len(), 2);
        assert_eq!(replay(Some(&before), &after)["parts"], after["parts"]);
        // A shorter message (a part went away) truncates.
        let shorter = message(vec![text("a")]);
        assert_eq!(replay(Some(&after), &shorter)["parts"], shorter["parts"]);
    }

    #[test]
    fn oversize_messages_split_into_frames_that_fit() {
        let big = "x".repeat(3 * 1024 * 1024 + 7);
        let full = message(vec![text("intro"), json!({ "kind": "reasoning", "id": "r", "body": big }), text("end")]);
        let frames = message_frames(None, &full);
        assert!(frames.len() > 3, "{} frames", frames.len());
        assert_eq!(replay(None, &full)["parts"], full["parts"]);
        // Then a big append on top also splits and lands exactly.
        let mut grown = full.clone();
        grown["parts"][2]["body"] = Value::String(format!("end{}", "é".repeat(700 * 1024)));
        assert_eq!(replay(Some(&full), &grown)["parts"], grown["parts"]);
    }

    #[test]
    fn huge_tool_summaries_are_capped_in_the_push() {
        let full = message(vec![json!({ "kind": "tool", "tool_id": "t", "name": "bash", "state": "done", "summary": "y".repeat(2 << 20), "images": [] })]);
        let view = replay(None, &full);
        let summary = view["parts"][0]["summary"].as_str().unwrap();
        assert!(summary.len() < FRAME_BUDGET && summary.ends_with("(truncated)"));
    }

    #[test]
    fn attachment_changes_alone_still_push() {
        let before = message(vec![text("a")]);
        let mut after = before.clone();
        after["attachments"] = json!([{ "id": "f", "name": "a.png", "mime": "image/png", "path": "/tmp/a.png" }]);
        let frames = message_frames(Some(&Pushed::of(&before)), &after);
        assert_eq!(frames.len(), 1);
        assert_eq!(replay(Some(&before), &after)["attachments"], after["attachments"]);
    }
}

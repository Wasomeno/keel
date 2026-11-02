//! Sizes the session document enforces. Later PRs own the engine timers.

/// `meta.schema` written by this crate. A greater value refuses to open.
pub const SCHEMA_VERSION: i64 = 1;

/// Maximum UTF-8 bytes in one text part. The next byte starts a continuation part.
pub const CONTINUATION_SPLIT: usize = 256 * 1024;

/// Tool parts keep this many chars of the first non-empty summary line.
pub const TOOL_SUMMARY_CHARS: usize = 160;

/// A question's receipt (tool name `keel:receipt`) keeps its JSON summary
/// whole up to this many chars: it is one line, and cutting it breaks it.
pub const RECEIPT_SUMMARY_CHARS: usize = 16 * 1024;

/// Unprocessed commands older than this, on the host drain, expire.
/// An age equal to this value stays unprocessed.
pub const COMMAND_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// First non-empty line, truncated to [`TOOL_SUMMARY_CHARS`].
pub fn tool_summary(raw: &str) -> String {
    let line = raw
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    line.chars().take(TOOL_SUMMARY_CHARS).collect()
}

/// The summary a tool part stores: receipts whole, others capped.
pub fn tool_part_summary(name: &str, raw: &str) -> String {
    if name == "keel:receipt" {
        return raw.chars().take(RECEIPT_SUMMARY_CHARS).collect();
    }
    tool_summary(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_skips_blank_lines_and_caps_chars() {
        let raw = format!("\n  \n{}\nsecond", "y".repeat(180));
        let summary = tool_summary(&raw);
        assert_eq!(summary, "y".repeat(TOOL_SUMMARY_CHARS));
        assert_eq!(tool_summary("\n\n"), "");
        assert_eq!(tool_summary("  hello"), "  hello");
    }
}

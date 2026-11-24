//! Merge request card snapshot model, status mapping, and timestamp helpers.

use serde::{Deserialize, Serialize};

pub use keel_proto::mr::{MrRef, find_mr_urls, parse_remote, parse_url};

/// What one fetch of the MR said.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MrSnapshot {
    pub title: String,
    /// `opened`, `merged`, `closed`, `locked`. Empty when not fetched yet.
    pub state: String,
    pub draft: bool,
    pub detailed_merge_status: String,
    pub has_conflicts: bool,
    /// Head pipeline status (`running`, `failed`, `success`…), empty if none.
    pub pipeline: String,
    pub source_branch: String,
    pub target_branch: String,
    pub created_at: String,
    pub merged_at: String,
    pub closed_at: String,
    pub updated_at: String,
    pub files_changed: Option<u64>,
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
    pub web_url: String,
}

impl MrSnapshot {
    /// The engine's `gitlab_mr` answer.
    pub fn parse(v: &serde_json::Value) -> Self {
        let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
        let detailed = s("detailed_merge_status");
        let num = |k: &str| {
            v[k].as_u64().or_else(|| {
                v[k].as_str()
                    .and_then(|raw| raw.trim().trim_end_matches('+').parse::<u64>().ok())
            })
        };
        Self {
            title: s("title"),
            state: s("state"),
            draft: v["draft"].as_bool().unwrap_or(false),
            detailed_merge_status: if detailed.is_empty() {
                s("merge_status")
            } else {
                detailed
            },
            has_conflicts: v["has_conflicts"].as_bool().unwrap_or(false),
            pipeline: s("pipeline"),
            source_branch: s("source_branch"),
            target_branch: s("target_branch"),
            created_at: s("created_at"),
            merged_at: s("merged_at"),
            closed_at: s("closed_at"),
            updated_at: s("updated_at"),
            files_changed: num("files_changed").or_else(|| num("changes_count")),
            additions: num("additions"),
            deletions: num("deletions"),
            web_url: s("web_url"),
        }
    }

    pub fn is_loaded(&self) -> bool {
        !self.state.trim().is_empty()
    }

    /// The one state the dot shows (draft is drawn separately: hollow).
    pub fn status(&self) -> MrStatus {
        match self.state.trim() {
            "" => return MrStatus::Unknown,
            "merged" => return MrStatus::Merged,
            "closed" | "locked" => return MrStatus::Closed,
            _ => {}
        }
        let dms = self.detailed_merge_status.as_str();
        if self.pipeline == "failed" {
            MrStatus::Failed
        } else if self.has_conflicts
            || matches!(dms, "conflict" | "need_rebase" | "cannot_be_merged")
        {
            MrStatus::Conflicts
        } else if matches!(
            self.pipeline.as_str(),
            "running" | "pending" | "created" | "preparing" | "waiting_for_resource" | "scheduled"
        ) {
            MrStatus::Running
        } else if !self.draft && matches!(dms, "mergeable" | "can_be_merged") {
            MrStatus::Mergeable
        } else {
            MrStatus::Open
        }
    }

    /// Words for the card and hover tooltip, reflecting GitLab's real state.
    pub fn status_text(&self) -> String {
        let status = self.status();
        let dms = self.detailed_merge_status.as_str();
        let word = match status {
            MrStatus::Unknown => "Unknown",
            MrStatus::Open => match dms {
                "not_approved" => "Needs approval",
                "discussions_not_resolved" => "Unresolved threads",
                "ci_must_pass" => "Waiting for CI",
                "ci_still_running" => "Pipeline running",
                "checking" | "unchecked" => "Checking",
                "requested_changes" => "Changes requested",
                "blocked_status" | "approvals_syncing" => "Blocked",
                "not_open" => "Not open",
                _ => "Open",
            },
            MrStatus::Running => "Pipeline running",
            MrStatus::Failed => "Pipeline failed",
            MrStatus::Conflicts if dms == "need_rebase" => "Needs rebase",
            MrStatus::Conflicts => "Has conflicts",
            MrStatus::Mergeable => "Ready to merge",
            MrStatus::Merged => "Merged",
            MrStatus::Closed if self.state == "locked" => "Locked",
            MrStatus::Closed => "Closed",
        };
        if self.draft && !status.is_done() && status != MrStatus::Unknown {
            if status == MrStatus::Open {
                "Draft".into()
            } else {
                format!("Draft · {word}")
            }
        } else {
            word.into()
        }
    }
}

/// GitLab's merged purple.
const MERGED: u32 = 0x8B5C_F6FF;

/// A status's color: the header dot and the MR card.
pub fn status_color(status: Option<MrStatus>, t: keel_theme::Tokens) -> u32 {
    match status {
        Some(MrStatus::Unknown) => t.text_faint,
        None | Some(MrStatus::Open | MrStatus::Running) => t.accent,
        Some(MrStatus::Failed) => t.danger,
        Some(MrStatus::Conflicts) => t.warning,
        Some(MrStatus::Mergeable) => t.success,
        Some(MrStatus::Merged) => MERGED,
        Some(MrStatus::Closed) => t.text_faint,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MrStatus {
    Unknown,
    Open,
    Running,
    Failed,
    Conflicts,
    Mergeable,
    Merged,
    Closed,
}

impl MrStatus {
    pub fn is_done(self) -> bool {
        matches!(self, Self::Merged | Self::Closed)
    }

    /// Changes worth one soft pulse.
    pub fn is_meaningful(self) -> bool {
        matches!(self, Self::Failed | Self::Mergeable | Self::Merged)
    }
}

/// Parses a GitLab ISO-8601 timestamp (`2026-11-24T17:53:56.835+07:00` or
/// `...Z`) into Unix epoch seconds, adjusting for its timezone offset.
pub fn parse_iso_epoch(iso: &str) -> Option<i64> {
    let iso = iso.trim();
    if iso.len() < 19 {
        return None;
    }
    let num = |s: &str| s.parse::<i64>().ok();
    let (y, m, d) = (num(&iso[0..4])?, num(&iso[5..7])?, num(&iso[8..10])?);
    let (hh, mm, ss) = (num(&iso[11..13])?, num(&iso[14..16])?, num(&iso[17..19])?);
    let rest = &iso[19..];
    let after_frac = if let Some(after_dot) = rest.strip_prefix('.') {
        after_dot.trim_start_matches(|c: char| c.is_ascii_digit())
    } else {
        rest
    };
    let tz_offset_secs = if after_frac.is_empty() || after_frac.eq_ignore_ascii_case("z") {
        0
    } else if let Some(sign) = after_frac.bytes().next().filter(|&b| b == b'+' || b == b'-') {
        let digits: String = after_frac[1..]
            .chars()
            .filter(char::is_ascii_digit)
            .collect();
        if digits.len() < 2 {
            return None;
        }
        let oh: i64 = digits[0..2].parse().ok()?;
        let om: i64 = if digits.len() >= 4 {
            digits[2..4].parse().ok()?
        } else {
            0
        };
        let mag = oh * 3600 + om * 60;
        if sign == b'+' { mag } else { -mag }
    } else {
        0
    };
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let naive_secs = (era * 146_097 + doe - 719_468) * 86_400 + hh * 3600 + mm * 60 + ss;
    Some(naive_secs - tz_offset_secs)
}

/// `"5m ago"` for a GitLab ISO time relative to `now_secs`.
pub fn ago_iso_at(iso: &str, now_secs: i64) -> Option<String> {
    let secs = parse_iso_epoch(iso)?;
    let age = (now_secs - secs).max(0);
    Some(match age {
        s if s < 60 => "just now".into(),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s if s < 86_400 * 30 => format!("{}d ago", s / 86_400),
        s => format!("{}mo ago", s / (86_400 * 30)),
    })
}

/// `"5m ago"` for a GitLab ISO time against the current wall clock.
pub fn ago_iso(iso: &str) -> Option<String> {
    ago_iso_at(iso, crate::timer::now_ms() / 1000)
}

/// Exact local date/time for tooltips (e.g. `"Nov 24, 2026 · 5:53 PM"`).
pub fn format_local_iso(iso: &str) -> Option<String> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let secs = parse_iso_epoch(iso)? as libc::time_t;
    #[cfg(unix)]
    let tm = {
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let res = unsafe { libc::localtime_r(&secs, &mut tm) };
        if res.is_null() {
            return None;
        }
        tm
    };
    #[cfg(not(unix))]
    let _ = secs;
    #[cfg(not(unix))]
    return Some(iso.to_string());
    #[cfg(unix)]
    {
        let month = MONTHS.get(tm.tm_mon as usize).copied().unwrap_or("Jan");
        let day = tm.tm_mday;
        let year = 1900 + tm.tm_year;
        let h24 = tm.tm_hour;
        let ampm = if h24 >= 12 { "PM" } else { "AM" };
        let h12 = match h24 % 12 {
            0 => 12,
            h => h,
        };
        let min = tm.tm_min;
        Some(format!("{month} {day}, {year} · {h12}:{min:02} {ampm}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mr(iid: u64) -> MrRef {
        MrRef {
            host: "https://gitlab.com".into(),
            project: "kev/keel".into(),
            iid,
        }
    }

    #[test]
    fn bang_mentions_are_found_at_word_starts() {
        use keel_proto::mr::find_bang_iids;
        assert_eq!(
            find_bang_iids("Merge request !337 is merged (see !12). Not a!5, !x, !9a or !"),
            [337, 12]
        );
    }

    #[test]
    fn urls_are_found_in_prose_and_tool_output() {
        let text = "Opened https://gitlab.com/kev/keel/-/merge_requests/42 (and see\nhttps://GitLab.example.com/a/b/c/-/merge_requests/7#note_1). Again: https://gitlab.com/kev/keel/-/merge_requests/42.";
        let found = find_mr_urls(text);
        assert_eq!(
            found,
            vec![
                mr(42),
                MrRef {
                    host: "https://gitlab.example.com".into(),
                    project: "a/b/c".into(),
                    iid: 7
                }
            ]
        );
        assert!(find_mr_urls("merge_requests/ with no link /-/merge_requests/x").is_empty());
        assert_eq!(
            mr(42).web_url(),
            "https://gitlab.com/kev/keel/-/merge_requests/42"
        );
    }

    #[test]
    fn remotes_name_host_and_project() {
        let want = Some(("https://gitlab.com".to_string(), "kev/keel".to_string()));
        assert_eq!(parse_remote("git@gitlab.com:kev/keel.git"), want);
        assert_eq!(parse_remote("https://gitlab.com/kev/keel.git"), want);
        assert_eq!(parse_remote("ssh://git@gitlab.com:2222/kev/keel.git"), want);
        assert_eq!(parse_remote("https://oauth2:tok@gitlab.com/kev/keel"), want);
        assert_eq!(parse_remote("nonsense"), None);
    }

    #[test]
    fn status_picks_the_loudest_fact() {
        let snap = |state: &str, pipeline: &str, dms: &str| MrSnapshot {
            state: state.into(),
            pipeline: pipeline.into(),
            detailed_merge_status: dms.into(),
            ..Default::default()
        };
        assert_eq!(snap("merged", "failed", "").status(), MrStatus::Merged);
        assert_eq!(snap("closed", "", "").status(), MrStatus::Closed);
        assert_eq!(
            snap("opened", "failed", "conflict").status(),
            MrStatus::Failed
        );
        assert_eq!(
            snap("opened", "success", "need_rebase").status(),
            MrStatus::Conflicts
        );
        assert_eq!(
            snap("opened", "running", "ci_still_running").status(),
            MrStatus::Running
        );
        assert_eq!(
            snap("opened", "success", "mergeable").status(),
            MrStatus::Mergeable
        );
        assert_eq!(snap("opened", "", "not_approved").status(), MrStatus::Open);
        let draft = MrSnapshot {
            draft: true,
            ..snap("opened", "", "draft_status")
        };
        assert_eq!(
            (draft.status(), draft.status_text().as_str()),
            (MrStatus::Open, "Draft")
        );
        let parsed = MrSnapshot::parse(
            &serde_json::json!({"title": "T", "state": "opened", "merge_status": "can_be_merged", "pipeline": null}),
        );
        assert_eq!(
            (parsed.status(), parsed.title.as_str()),
            (MrStatus::Mergeable, "T")
        );
    }

    #[test]
    fn snapshot_status_handles_empty_and_detailed_states() {
        let empty = MrSnapshot::default();
        assert_eq!(empty.status(), MrStatus::Unknown);
        assert_eq!(empty.status_text(), "Unknown");
        assert!(!empty.is_loaded());

        let approvals = MrSnapshot {
            state: "opened".into(),
            detailed_merge_status: "not_approved".into(),
            ..Default::default()
        };
        assert_eq!(approvals.status(), MrStatus::Open);
        assert_eq!(approvals.status_text(), "Needs approval");

        let parsed = MrSnapshot::parse(&serde_json::json!({
            "title": "Fix table",
            "state": "merged",
            "created_at": "2026-11-24T15:00:00+07:00",
            "merged_at": "2026-11-24T17:53:56.835+07:00",
            "files_changed": 4,
            "additions": 120,
            "deletions": 32,
        }));
        assert!(parsed.is_loaded());
        assert_eq!(parsed.status(), MrStatus::Merged);
        assert_eq!(parsed.files_changed, Some(4));
        assert_eq!(parsed.additions, Some(120));
        assert_eq!(parsed.deletions, Some(32));
    }

    #[test]
    fn iso_timestamp_respects_timezone_offsets() {
        let utc = parse_iso_epoch("2026-11-24T10:53:56Z").unwrap();
        let plus7 = parse_iso_epoch("2026-11-24T17:53:56.835+07:00").unwrap();
        let minus5 = parse_iso_epoch("2026-11-24T05:53:56-05:00").unwrap();
        assert_eq!(utc, plus7);
        assert_eq!(utc, minus5);
        assert_eq!(ago_iso_at("2026-11-24T17:50:00+07:00", utc + 60), Some("4m ago".into()));
    }
}

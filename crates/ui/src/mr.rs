//! The per-session merge request monitor's model: which MR a chat tracks,
//! its last fetched state, the one-time "track this?" offer, and MRs never
//! to offer again. `session-mr.json` in the data dir, keyed by chat id.
//! Rendering and polling live in `view/mr_chip.rs`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use keel_proto::mr::{MrRef, find_mr_urls, parse_remote, parse_url};

/// What one fetch of the MR said.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MrSnapshot {
    pub title: String,
    /// `opened`, `merged`, `closed`, `locked`.
    pub state: String,
    pub draft: bool,
    pub detailed_merge_status: String,
    pub has_conflicts: bool,
    /// Head pipeline status (`running`, `failed`, `success`…), empty if none.
    pub pipeline: String,
    pub source_branch: String,
    pub target_branch: String,
    pub updated_at: String,
    pub web_url: String,
}

impl MrSnapshot {
    /// The engine's `gitlab_mr` answer.
    pub fn parse(v: &serde_json::Value) -> Self {
        let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
        let detailed = s("detailed_merge_status");
        Self {
            title: s("title"),
            state: s("state"),
            draft: v["draft"].as_bool().unwrap_or(false),
            detailed_merge_status: if detailed.is_empty() { s("merge_status") } else { detailed },
            has_conflicts: v["has_conflicts"].as_bool().unwrap_or(false),
            pipeline: s("pipeline"),
            source_branch: s("source_branch"),
            target_branch: s("target_branch"),
            updated_at: s("updated_at"),
            web_url: s("web_url"),
        }
    }

    /// The one state the dot shows (draft is drawn separately: hollow).
    pub fn status(&self) -> MrStatus {
        match self.state.as_str() {
            "merged" => return MrStatus::Merged,
            "closed" | "locked" => return MrStatus::Closed,
            _ => {}
        }
        let dms = self.detailed_merge_status.as_str();
        if self.pipeline == "failed" {
            MrStatus::Failed
        } else if self.has_conflicts || matches!(dms, "conflict" | "need_rebase" | "cannot_be_merged") {
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

    /// Words for the hover card.
    pub fn status_text(&self) -> String {
        let status = self.status();
        let word = match status {
            MrStatus::Open => "Open",
            MrStatus::Running => "Pipeline running",
            MrStatus::Failed => "Pipeline failed",
            MrStatus::Conflicts if self.detailed_merge_status == "need_rebase" => "Needs rebase",
            MrStatus::Conflicts => "Has conflicts",
            MrStatus::Mergeable => "Ready to merge",
            MrStatus::Merged => "Merged",
            MrStatus::Closed => "Closed",
        };
        if self.draft && !status.is_done() {
            if status == MrStatus::Open { "Draft".into() } else { format!("Draft · {word}") }
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

/// The MR in a chat's header.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tracked {
    pub mr: MrRef,
    #[serde(default)]
    pub snap: Option<MrSnapshot>,
    /// Last good fetch and last attempt (ms), and failures since the last
    /// good one (the last one's message in `error`).
    #[serde(default)]
    pub ok_ms: i64,
    #[serde(default)]
    pub tried_ms: i64,
    #[serde(default)]
    pub errors: u32,
    #[serde(default)]
    pub error: Option<String>,
}

impl Tracked {
    pub fn new(mr: MrRef) -> Self {
        Self { mr, snap: None, ok_ms: 0, tried_ms: 0, errors: 0, error: None }
    }

    pub fn status(&self) -> Option<MrStatus> {
        self.snap.as_ref().map(MrSnapshot::status)
    }

    /// Due for a fetch: never tried, or `every` ms since the last try,
    /// doubled per failure (up to 16×).
    pub fn due(&self, now: i64, every: i64) -> bool {
        self.tried_ms == 0 || now - self.tried_ms >= every << self.errors.min(4)
    }
}

/// "Track this?" under the turn that mentioned an MR.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Offer {
    pub mr: MrRef,
    /// The turn's message id (the row it rides under).
    pub msg_id: String,
    /// Fetched for its title; `None` until then.
    #[serde(default)]
    pub snap: Option<MrSnapshot>,
}

/// A quiet history line: "!42 merged · 3:12 PM".
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub text: String,
    /// The row it sits under; filled in when the chat is next on screen.
    #[serde(default)]
    pub msg_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionMr {
    pub tracked: Option<Tracked>,
    pub offer: Option<Offer>,
    /// MR keys ignored, stopped or finished here: never offered again.
    pub seen: Vec<String>,
    pub notes: Vec<Note>,
}

/// What a mention of an MR in a turn led to.
#[derive(Debug, PartialEq, Eq)]
pub enum Mention {
    Nothing,
    /// Offered under the turn (a plain mention, or a second MR to replace with).
    Offered,
    /// The agent created it: tracked right away (with Undo).
    Tracked,
}

impl SessionMr {
    fn is_seen(&self, mr: &MrRef) -> bool {
        self.seen.iter().any(|k| *k == mr.key())
    }

    fn remember(&mut self, mr: &MrRef) {
        if !self.is_seen(mr) {
            self.seen.push(mr.key());
        }
    }

    /// `mr` showed up in turn `msg_id`; `created` when a tool in that turn
    /// made it. One MR per session: anything else is only offered.
    pub fn mention(&mut self, mr: MrRef, msg_id: &str, created: bool) -> Mention {
        if self.is_seen(&mr) || self.tracked.as_ref().is_some_and(|t| t.mr == mr) {
            return Mention::Nothing;
        }
        if self.offer.as_ref().is_some_and(|o| o.mr == mr) {
            return Mention::Nothing;
        }
        if created && self.tracked.is_none() {
            self.offer = None;
            self.tracked = Some(Tracked::new(mr));
            return Mention::Tracked;
        }
        self.offer = Some(Offer { mr, msg_id: msg_id.to_string(), snap: None });
        Mention::Offered
    }

    /// Track (from the offer, the prompt, or Replace): the old one, if any,
    /// is remembered so it isn't offered again.
    pub fn track(&mut self, mr: MrRef) {
        if self.tracked.as_ref().is_some_and(|t| t.mr == mr) {
            return;
        }
        if let Some(old) = self.tracked.take() {
            self.remember(&old.mr);
        }
        if self.offer.as_ref().is_some_and(|o| o.mr == mr) {
            self.offer = None;
        }
        self.tracked = Some(Tracked::new(mr));
    }

    pub fn ignore_offer(&mut self) {
        if let Some(offer) = self.offer.take() {
            self.remember(&offer.mr);
        }
    }

    pub fn stop(&mut self) {
        if let Some(t) = self.tracked.take() {
            self.remember(&t.mr);
        }
    }

    /// The tracked MR merged or closed and its moment in the header is over:
    /// drop it and leave a note.
    pub fn finish(&mut self, note: String) {
        if let Some(t) = self.tracked.take() {
            self.remember(&t.mr);
            self.notes.push(Note { text: note, msg_id: None });
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tracked.is_none() && self.offer.is_none() && self.seen.is_empty() && self.notes.is_empty()
    }
}

/// Every session's MR state.
#[derive(Default)]
pub struct MrStore {
    pub sessions: HashMap<String, SessionMr>,
}

impl MrStore {
    fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("session-mr.json")
    }

    pub fn load(data_dir: &Path) -> Self {
        let sessions = std::fs::read_to_string(Self::path(data_dir))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self { sessions }
    }

    pub fn save(&mut self, data_dir: &Path) {
        self.sessions.retain(|_, s| !s.is_empty());
        let path = Self::path(data_dir);
        let Ok(text) = serde_json::to_string_pretty(&self.sessions) else {
            return;
        };
        let _ = std::fs::create_dir_all(data_dir);
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, format!("{text}\n")).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    pub fn get(&self, chat: &str) -> Option<&SessionMr> {
        self.sessions.get(chat)
    }

    pub fn entry(&mut self, chat: &str) -> &mut SessionMr {
        self.sessions.entry(chat.to_string()).or_default()
    }

    pub fn tracked(&self, chat: &str) -> Option<&Tracked> {
        self.sessions.get(chat)?.tracked.as_ref()
    }
}

/// What the "Track merge request…" box accepts: a URL, or `!42` / `42` in
/// `default` (the project this chat already points at).
pub fn parse_input(input: &str, default: Option<(&str, &str)>) -> Result<MrRef, String> {
    let input = input.trim();
    if input.contains("://") {
        return parse_url(input).ok_or_else(|| "That isn't a merge request link.".to_string());
    }
    let iid: u64 = input
        .trim_start_matches('!')
        .parse()
        .map_err(|_| "Paste a merge request link or type !42.".to_string())?;
    let (host, project) = default.ok_or_else(|| "No GitLab project known for this chat; paste the full link.".to_string())?;
    Ok(MrRef { host: host.to_string(), project: project.to_string(), iid })
}

/// A turn's tool call made an MR (rather than just mentioning one).
pub fn tool_creates_mr(name: &str, summary: &str) -> bool {
    let text = format!("{name} {summary}").to_ascii_lowercase();
    ["mr create", "create_merge_request", "merge_request.create", "create-merge-request", "createmergerequest", "mr_create"]
        .iter()
        .any(|needle| text.contains(needle))
}

/// Title cut to `max` characters with an ellipsis.
pub fn truncate(title: &str, max: usize) -> String {
    if title.chars().count() <= max {
        return title.to_string();
    }
    let cut: String = title.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mr(iid: u64) -> MrRef {
        MrRef { host: "https://gitlab.com".into(), project: "kev/keel".into(), iid }
    }

    #[test]
    fn bang_mentions_are_found_at_word_starts() {
        use keel_proto::mr::find_bang_iids;
        assert_eq!(find_bang_iids("Merge request !337 is merged (see !12). Not a!5, !x, !9a or !"), [337, 12]);
    }

    #[test]
    fn urls_are_found_in_prose_and_tool_output() {
        let text = "Opened https://gitlab.com/kev/keel/-/merge_requests/42 (and see\nhttps://GitLab.example.com/a/b/c/-/merge_requests/7#note_1). Again: https://gitlab.com/kev/keel/-/merge_requests/42.";
        let found = find_mr_urls(text);
        assert_eq!(found, vec![mr(42), MrRef { host: "https://gitlab.example.com".into(), project: "a/b/c".into(), iid: 7 }]);
        assert!(find_mr_urls("merge_requests/ with no link /-/merge_requests/x").is_empty());
        assert_eq!(mr(42).web_url(), "https://gitlab.com/kev/keel/-/merge_requests/42");
    }

    #[test]
    fn input_takes_a_link_or_a_number_in_the_chats_project() {
        let default = Some(("https://gitlab.com", "kev/keel"));
        assert_eq!(parse_input("!42", default), Ok(mr(42)));
        assert_eq!(parse_input(" 42 ", default), Ok(mr(42)));
        assert_eq!(parse_input("https://gitlab.com/kev/keel/-/merge_requests/42/diffs", None), Ok(mr(42)));
        assert!(parse_input("!42", None).is_err());
        assert!(parse_input("hello", default).is_err());
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
        assert_eq!(snap("opened", "failed", "conflict").status(), MrStatus::Failed);
        assert_eq!(snap("opened", "success", "need_rebase").status(), MrStatus::Conflicts);
        assert_eq!(snap("opened", "running", "ci_still_running").status(), MrStatus::Running);
        assert_eq!(snap("opened", "success", "mergeable").status(), MrStatus::Mergeable);
        assert_eq!(snap("opened", "", "not_approved").status(), MrStatus::Open);
        let draft = MrSnapshot { draft: true, ..snap("opened", "", "draft_status") };
        assert_eq!((draft.status(), draft.status_text().as_str()), (MrStatus::Open, "Draft"));
        let parsed = MrSnapshot::parse(&serde_json::json!({"title": "T", "state": "opened", "merge_status": "can_be_merged", "pipeline": null}));
        assert_eq!((parsed.status(), parsed.title.as_str()), (MrStatus::Mergeable, "T"));
    }

    #[test]
    fn one_mr_per_session_and_seen_ones_never_come_back() {
        let mut s = SessionMr::default();
        assert_eq!(s.mention(mr(1), "m1", false), Mention::Offered);
        s.ignore_offer();
        assert_eq!(s.mention(mr(1), "m1", false), Mention::Nothing);
        assert_eq!(s.mention(mr(2), "m2", true), Mention::Tracked);
        assert_eq!(s.tracked.as_ref().map(|t| t.mr.iid), Some(2));
        // A second one is only offered (as a replacement).
        assert_eq!(s.mention(mr(3), "m3", true), Mention::Offered);
        s.track(mr(3));
        assert_eq!((s.tracked.as_ref().map(|t| t.mr.iid), s.offer.is_none()), (Some(3), true));
        assert_eq!(s.mention(mr(2), "m2", true), Mention::Nothing);
        s.finish("!3 merged".into());
        assert!(s.tracked.is_none());
        assert_eq!(s.notes.len(), 1);
        assert_eq!(s.mention(mr(3), "m3", true), Mention::Nothing);
    }

    #[test]
    fn polling_backs_off_after_errors() {
        let mut t = Tracked::new(mr(1));
        assert!(t.due(1_000, 60_000));
        t.tried_ms = 1_000;
        assert!(!t.due(30_000, 60_000));
        assert!(t.due(61_000, 60_000));
        t.errors = 2;
        assert!(!t.due(200_000, 60_000));
        assert!(t.due(241_000, 60_000));
    }

    #[test]
    fn tools_that_create_mrs_are_told_apart() {
        assert!(tool_creates_mr("Bash", "glab mr create --fill"));
        assert!(tool_creates_mr("mcp__gitlab__create_merge_request", ""));
        assert!(!tool_creates_mr("Bash", "glab mr view 42"));
        assert_eq!(truncate("A fairly long merge request title here", 12), "A fairly lo…");
        assert_eq!(truncate("short", 12), "short");
    }
}

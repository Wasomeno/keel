//! A merge request card the engine's generative UI chose for a reply (a `ui`
//! part with `component: "mr"`): the MR's state when the turn ended, and
//! whether to offer the Merge button. The engine checks the MR live again
//! before it merges.

use serde_json::Value;

use crate::mr::{MrRef, MrSnapshot};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MrCard {
    pub mr: MrRef,
    pub snap: MrSnapshot,
    pub offer_merge: bool,
}

impl MrCard {
    /// An MR spec; `None` for another component or a malformed one.
    pub fn from_spec(spec: &Value) -> Option<Self> {
        if spec["component"] != "mr" {
            return None;
        }
        let mr = MrRef {
            host: spec["host"].as_str()?.to_string(),
            project: spec["project"].as_str()?.to_string(),
            iid: spec["iid"].as_u64()?,
        };
        let snap = MrSnapshot::parse(&spec["snap"]);
        Some(Self { mr, snap, offer_merge: spec["offer_merge"] == true })
    }

    pub fn url(&self) -> String {
        if self.snap.web_url.is_empty() { self.mr.web_url() } else { self.snap.web_url.clone() }
    }

    /// For the transcript's height memo: changes when the state does.
    pub fn content_len(&self) -> usize {
        self.snap.title.len() + self.snap.status_text().len() + usize::from(self.offer_merge)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn spec_needs_a_whole_ref() {
        let spec = json!({ "component": "mr", "host": "https://gitlab.com", "project": "k/keel", "iid": 42,
            "snap": { "title": "Cache", "state": "opened", "detailed_merge_status": "mergeable", "pipeline": "success" },
            "offer_merge": true });
        let card = MrCard::from_spec(&spec).unwrap();
        assert_eq!(card.url(), "https://gitlab.com/k/keel/-/merge_requests/42");
        assert_eq!(card.snap.status_text(), "Ready to merge");
        assert!(card.offer_merge);
        assert!(MrCard::from_spec(&json!({ "component": "mr", "host": "h" })).is_none());
    }
}

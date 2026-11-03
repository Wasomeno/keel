//! The model picker's list logic, free of UI: providers, families,
//! versions, dated snapshots, "older versions", fuzzy search, and the
//! recent list.
//!
//! The top level is the provider: the ID's prefix before `/`
//! (`anthropic/…` is Anthropic), or, with no prefix, the family's maker
//! (Claude is Anthropic, GPT and Codex are OpenAI), else "Other".
//!
//! A model's family comes from its ID or name (Opus, Sonnet, GPT, …, with
//! "Other" last). Within a family models sort newest first by the version
//! parsed from the ID. A model is "older" when it is a dated snapshot
//! (`…-20250514`) or another model of the same variant (the ID with its
//! version taken out, so `gpt-5-mini` and `gpt-5.1-mini` are one variant but
//! `gpt-5-mini` and `gpt-5.1` are not) has a newer version.

use crate::composer::ModelOption;

/// How many recent picks a harness keeps.
pub const RECENT_MAX: usize = 3;

/// Family names, checked in this order against the lowercased ID and name.
const FAMILIES: &[(&str, &[&str])] = &[
    ("Fable", &["fable"]),
    ("Opus", &["opus"]),
    ("Sonnet", &["sonnet"]),
    ("Haiku", &["haiku"]),
    ("Codex", &["codex"]),
    ("GPT", &["gpt", "chatgpt"]),
    ("Gemini", &["gemini"]),
    ("Gemma", &["gemma"]),
    ("Grok", &["grok"]),
    ("Claude", &["claude"]),
    ("Llama", &["llama"]),
    (
        "Mistral",
        &["mistral", "codestral", "devstral", "magistral"],
    ),
    ("DeepSeek", &["deepseek"]),
    ("Qwen", &["qwen", "qwq"]),
    ("Kimi", &["kimi", "moonshot"]),
    ("GLM", &["glm"]),
];

pub const OTHER: &str = "Other";

/// The part of an ID after its provider prefix (`anthropic/claude-…`).
fn base(id: &str) -> &str {
    id.rsplit('/').next().unwrap_or(id)
}

/// The family a model belongs to.
pub fn family(id: &str, name: &str) -> &'static str {
    let hay = format!("{} {}", base(id).to_lowercase(), name.to_lowercase());
    for (family, keys) in FAMILIES {
        if keys.iter().any(|k| hay.contains(k)) {
            return family;
        }
    }
    // OpenAI's reasoning models: o1, o3-mini, o4-mini.
    let b = base(id).to_lowercase();
    let mut chars = b.chars();
    if chars.next() == Some('o') && chars.next().is_some_and(|c| c.is_ascii_digit()) {
        return "GPT";
    }
    OTHER
}

/// Display names for ID prefixes.
const PROVIDERS: &[(&str, &str)] = &[
    ("anthropic", "Anthropic"),
    ("openai", "OpenAI"),
    ("azure", "Azure"),
    ("google", "Google"),
    ("gemini", "Google"),
    ("vertex", "Google"),
    ("vertexai", "Google"),
    ("xai", "xAI"),
    ("x-ai", "xAI"),
    ("meta", "Meta"),
    ("meta-llama", "Meta"),
    ("mistral", "Mistral"),
    ("mistralai", "Mistral"),
    ("deepseek", "DeepSeek"),
    ("qwen", "Alibaba"),
    ("alibaba", "Alibaba"),
    ("moonshot", "Moonshot"),
    ("moonshotai", "Moonshot"),
    ("zai", "Z.ai"),
    ("z-ai", "Z.ai"),
    ("zhipu", "Z.ai"),
    ("amazon", "Amazon"),
    ("bedrock", "Amazon Bedrock"),
    ("groq", "Groq"),
    ("ollama", "Ollama"),
    ("openrouter", "OpenRouter"),
    ("github-copilot", "GitHub Copilot"),
    ("copilot", "GitHub Copilot"),
];

/// Who makes each family, for IDs with no provider prefix.
const MAKERS: &[(&str, &str)] = &[
    ("Opus", "Anthropic"),
    ("Sonnet", "Anthropic"),
    ("Haiku", "Anthropic"),
    ("Claude", "Anthropic"),
    ("Codex", "OpenAI"),
    ("GPT", "OpenAI"),
    ("Gemini", "Google"),
    ("Gemma", "Google"),
    ("Grok", "xAI"),
    ("Llama", "Meta"),
    ("Mistral", "Mistral"),
    ("DeepSeek", "DeepSeek"),
    ("Qwen", "Alibaba"),
    ("Kimi", "Moonshot"),
    ("GLM", "Z.ai"),
];

/// The provider a model is listed under: the segment before its base ID
/// (`openrouter/anthropic/claude-…` is Anthropic), named nicely; an unknown
/// prefix is capitalised; no prefix falls back to the family's maker, then
/// [`OTHER`].
pub fn provider(id: &str, name: &str) -> String {
    let mut parts = id.rsplit('/');
    parts.next();
    if let Some(prefix) = parts.next().map(str::trim).filter(|p| !p.is_empty()) {
        let key = prefix.to_lowercase();
        if let Some((_, nice)) = PROVIDERS.iter().find(|(k, _)| *k == key) {
            return (*nice).to_string();
        }
        let mut chars = prefix.chars();
        return chars
            .next()
            .map(|c| c.to_uppercase().chain(chars).collect())
            .unwrap_or_else(|| OTHER.to_string());
    }
    let fam = family(id, name);
    MAKERS
        .iter()
        .find(|(f, _)| *f == fam)
        .map_or(OTHER, |(_, p)| p)
        .to_string()
}

/// A dated snapshot: the ID ends in a date (`-20250514`, `@20250514`,
/// `-2025-05-14`).
pub fn is_snapshot(id: &str) -> bool {
    let b = base(id).as_bytes();
    let digits = |s: &[u8]| !s.is_empty() && s.iter().all(u8::is_ascii_digit);
    let dated =
        |d: &[u8]| d.len() == 8 && digits(d) && (d.starts_with(b"19") || d.starts_with(b"20"));
    if b.len() > 9 && matches!(b[b.len() - 9], b'-' | b'@' | b'_') && dated(&b[b.len() - 8..]) {
        return true;
    }
    // -YYYY-MM-DD
    if b.len() > 11 {
        let tail = &b[b.len() - 11..];
        let (y, m, d) = (&tail[1..5], &tail[6..8], &tail[9..11]);
        return tail[0] == b'-'
            && tail[5] == b'-'
            && tail[8] == b'-'
            && digits(y)
            && digits(m)
            && digits(d)
            && (y.starts_with(b"19") || y.starts_with(b"20"));
    }
    false
}

/// The version in an ID: its short digit runs up to the first long one (a
/// date). `claude-opus-4-7` → [4, 7]; `claude-3-5-sonnet-20241022` → [3, 5];
/// `gpt-5.1-codex` → [5, 1]; none → empty.
pub fn version(id: &str) -> Vec<u32> {
    let mut out = Vec::new();
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut Vec<u32>| -> bool {
        if run.is_empty() {
            return true;
        }
        let keep = run.len() <= 3;
        if keep {
            out.push(run.parse().unwrap_or(0));
        }
        run.clear();
        keep
    };
    for ch in base(id).chars() {
        if ch.is_ascii_digit() {
            run.push(ch);
        } else if !flush(&mut run, &mut out) {
            return out;
        }
    }
    flush(&mut run, &mut out);
    out
}

/// The ID with its version and date taken out: models of one variant
/// differ only by version (`gpt-5-mini`, `gpt-5.1-mini` → `gpt-mini`).
fn variant(id: &str) -> String {
    let b = base(id).to_lowercase();
    let mut parts: Vec<String> = Vec::new();
    for part in b.split(['-', '_', '.', '@', ':']) {
        let kept: String = part.chars().filter(|c| !c.is_ascii_digit()).collect();
        if !kept.is_empty() {
            parts.push(kept);
        }
    }
    parts.join("-")
}

/// One family's models (indices into the list), newest first: those shown
/// and those behind "Older versions".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub family: &'static str,
    pub current: Vec<usize>,
    pub older: Vec<usize>,
}

/// The list grouped by family, in the order families first appear (the
/// harness lists its preferred models first), "Other" last.
pub fn group(models: &[ModelOption]) -> Vec<Group> {
    let mut order: Vec<&'static str> = Vec::new();
    let fams: Vec<&'static str> = models.iter().map(|m| family(&m.id, &m.name)).collect();
    for f in &fams {
        if !order.contains(f) {
            order.push(f);
        }
    }
    if let Some(pos) = order.iter().position(|f| *f == OTHER) {
        let other = order.remove(pos);
        order.push(other);
    }
    let versions: Vec<Vec<u32>> = models.iter().map(|m| version(&m.id)).collect();
    let variants: Vec<String> = models.iter().map(|m| variant(&m.id)).collect();
    let snaps: Vec<bool> = models.iter().map(|m| is_snapshot(&m.id)).collect();
    order
        .into_iter()
        .map(|family| {
            let mut members: Vec<usize> =
                (0..models.len()).filter(|&i| fams[i] == family).collect();
            // Newest first; equal versions keep the harness's order.
            members.sort_by(|&a, &b| versions[b].cmp(&versions[a]));
            let superseded = |i: usize| {
                !versions[i].is_empty()
                    && members.iter().any(|&j| {
                        j != i
                            && !snaps[j]
                            && variants[j] == variants[i]
                            && versions[j] > versions[i]
                    })
            };
            let (mut older, mut current): (Vec<usize>, Vec<usize>) =
                members.iter().partition(|&&i| snaps[i] || superseded(i));
            // A family of snapshots only: show them rather than an empty group.
            if current.is_empty() {
                current = std::mem::take(&mut older);
            }
            Group {
                family,
                current,
                older,
            }
        })
        .collect()
}

/// One provider's models: its families (newest first within each) and,
/// behind one "Older versions" fold, the dated snapshots and superseded
/// versions of all of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub provider: String,
    pub groups: Vec<Group>,
    pub older: Vec<usize>,
}

impl Section {
    pub fn count(&self) -> usize {
        self.groups.iter().map(|g| g.current.len()).sum::<usize>() + self.older.len()
    }

    pub fn contains(&self, i: usize) -> bool {
        self.older.contains(&i) || self.groups.iter().any(|g| g.current.contains(&i))
    }

    /// The models shown open (not older), in order: by family, newest first
    /// within each. `true` marks the first model of every family after the
    /// first, where the menu leaves a little air instead of a family label.
    pub fn listed(&self) -> Vec<(usize, bool)> {
        let mut out = Vec::new();
        for (gi, g) in self.groups.iter().enumerate() {
            for (k, &i) in g.current.iter().enumerate() {
                out.push((i, gi > 0 && k == 0));
            }
        }
        out
    }
}

/// The older-versions line at the end of a provider.
pub fn older_label(n: usize, open: bool) -> String {
    if open {
        "Hide older".to_string()
    } else {
        format!("Show {n} older")
    }
}

/// Providers in the order they first appear, "Other" last.
fn provider_order(of: &[String]) -> Vec<String> {
    let mut order: Vec<String> = Vec::new();
    for p in of {
        if !order.contains(p) {
            order.push(p.clone());
        }
    }
    if let Some(pos) = order.iter().position(|p| p == OTHER) {
        let other = order.remove(pos);
        order.push(other);
    }
    order
}

/// The list as provider sections.
pub fn sections(models: &[ModelOption]) -> Vec<Section> {
    let of: Vec<String> = models.iter().map(|m| provider(&m.id, &m.name)).collect();
    provider_order(&of)
        .into_iter()
        .map(|p| {
            let members: Vec<usize> = (0..models.len()).filter(|&i| of[i] == p).collect();
            let subset: Vec<ModelOption> = members.iter().map(|&i| models[i].clone()).collect();
            let mut groups = group(&subset);
            let mut older = Vec::new();
            for g in &mut groups {
                g.current = g.current.iter().map(|&j| members[j]).collect();
                older.extend(g.older.drain(..).map(|j| members[j]));
            }
            groups.retain(|g| !g.current.is_empty());
            Section {
                provider: p,
                groups,
                older,
            }
        })
        .collect()
}

/// Which providers start open: the saved set when there is one (kept to
/// providers still listed), else the current model's provider. A lone
/// provider starts open either way unless it was folded on purpose.
pub fn open_providers(
    saved: Option<&[String]>,
    sections: &[Section],
    current: Option<usize>,
) -> Vec<String> {
    if let Some(saved) = saved {
        return sections
            .iter()
            .filter(|s| saved.contains(&s.provider))
            .map(|s| s.provider.clone())
            .collect();
    }
    if let [only] = sections {
        return vec![only.provider.clone()];
    }
    current
        .and_then(|i| sections.iter().find(|s| s.contains(i)))
        .map(|s| vec![s.provider.clone()])
        .unwrap_or_default()
}

/// Matches for `query` by provider, flat (folds don't apply): providers in
/// the order of their best match, matches best first.
pub fn search_by_provider(models: &[ModelOption], query: &str) -> Vec<(String, Vec<usize>)> {
    let mut out: Vec<(String, Vec<usize>)> = Vec::new();
    for i in search(models, query) {
        let p = provider(&models[i].id, &models[i].name);
        match out.iter_mut().find(|(q, _)| *q == p) {
            Some((_, hits)) => hits.push(i),
            None => out.push((p, vec![i])),
        }
    }
    out
}

/// Lowercase letters and digits only: "Opus 4.7" and "opus-4-7" read alike.
fn squash(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// How well `query` matches a model, or `None`. Every word of the query must
/// appear in the name or ID, in order but not necessarily together
/// (separators and case don't count), so "op 4.7" finds Claude Opus 4.7.
/// A word found whole scores above one spread out, and earlier above later.
pub fn fuzzy_score(query: &str, name: &str, id: &str) -> Option<i32> {
    let hay = format!("{}{}", squash(name), squash(id));
    let mut total = 0;
    for word in query.split_whitespace() {
        let word = squash(word);
        if word.is_empty() {
            continue;
        }
        if let Some(at) = hay.find(&word) {
            total += 100 - (at as i32).min(60);
            continue;
        }
        // In order, gaps allowed.
        let hay_b = hay.as_bytes();
        let (mut start, mut pos) = (None, 0usize);
        for w in word.bytes() {
            let found = hay_b[pos..].iter().position(|&h| h == w)?;
            start.get_or_insert(pos + found);
            pos += found + 1;
        }
        let span = pos - start.unwrap_or(0);
        total += 40 - (span as i32 - word.len() as i32).min(30);
    }
    Some(total)
}

/// Models matching `query`, best first (ties keep the list's order).
pub fn search(models: &[ModelOption], query: &str) -> Vec<usize> {
    let mut hits: Vec<(i32, usize)> = models
        .iter()
        .enumerate()
        .filter_map(|(i, m)| fuzzy_score(query, &m.name, &m.id).map(|s| (s, i)))
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    hits.into_iter().map(|(_, i)| i).collect()
}

/// `id` picked: to the front of `recent`, at most [`RECENT_MAX`]. True when
/// the list changed.
pub fn push_recent(recent: &mut Vec<String>, id: &str) -> bool {
    if id.is_empty() || recent.first().is_some_and(|r| r == id) {
        return false;
    }
    recent.retain(|r| r != id);
    recent.insert(0, id.to_string());
    recent.truncate(RECENT_MAX);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(id: &str, name: &str) -> ModelOption {
        ModelOption {
            id: id.into(),
            name: name.into(),
            reasoning_levels: Vec::new(),
            context_window: None,
        }
    }

    #[test]
    fn families_come_from_id_or_name() {
        assert_eq!(
            family("anthropic/claude-opus-4-7", "Claude Opus 4.7"),
            "Opus"
        );
        assert_eq!(family("claude-3-5-sonnet-20241022", ""), "Sonnet");
        assert_eq!(
            family("anthropic/claude-haiku-4-5", "Claude Haiku 4.5"),
            "Haiku"
        );
        assert_eq!(family("openai/gpt-5.1", "GPT-5.1"), "GPT");
        assert_eq!(family("o3-mini", "o3-mini"), "GPT");
        assert_eq!(family("gpt-5.1-codex", "GPT-5.1 Codex"), "Codex");
        assert_eq!(family("google/gemini-2.5-pro", "Gemini 2.5 Pro"), "Gemini");
        assert_eq!(family("xai/grok-4", "Grok 4"), "Grok");
        assert_eq!(family("fable-1", "Fable"), "Fable");
        assert_eq!(family("x/claude-3-7-something", "Claude 3.7"), "Claude");
        assert_eq!(family("acme/frobnicator-2", "Frobnicator"), OTHER);
    }

    #[test]
    fn snapshots_are_dated_ids() {
        assert!(is_snapshot("claude-sonnet-4-20250514"));
        assert!(is_snapshot("anthropic/claude-3-5-sonnet-20241022"));
        assert!(is_snapshot("claude-3-5-sonnet@20241022"));
        assert!(is_snapshot("gpt-4o-2024-08-06"));
        assert!(!is_snapshot("claude-opus-4-7"));
        assert!(!is_snapshot("gpt-5.1"));
        assert!(!is_snapshot("model-12345678x"));
        assert!(!is_snapshot("build-99999999"));
    }

    #[test]
    fn versions_parse_and_sort_newest_first() {
        assert_eq!(version("anthropic/claude-opus-4-7"), vec![4, 7]);
        assert_eq!(version("claude-3-5-sonnet-20241022"), vec![3, 5]);
        assert_eq!(version("gpt-5.1-codex"), vec![5, 1]);
        assert_eq!(version("gemini-2.5-pro"), vec![2, 5]);
        assert_eq!(version("gpt-4o-2024-08-06"), vec![4]);
        assert_eq!(version("grok-code-fast"), Vec::<u32>::new());
        assert!(version("claude-opus-4-10") > version("claude-opus-4-9"));
        let models = [
            m("claude-opus-4-5", "Opus 4.5"),
            m("claude-opus-4-7", "Opus 4.7"),
            m("claude-opus-4-6", "Opus 4.6"),
        ];
        let groups = group(&models);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].current, vec![1]);
        assert_eq!(groups[0].older, vec![2, 0]);
    }

    #[test]
    fn groups_split_current_from_older_by_variant_and_snapshot() {
        let models = [
            m("anthropic/claude-sonnet-4-5", "Claude Sonnet 4.5"),
            m("anthropic/claude-opus-4-7", "Claude Opus 4.7"),
            m(
                "anthropic/claude-sonnet-4-20250514",
                "Claude Sonnet 4 (2025-05-14)",
            ),
            m("openai/gpt-5.1", "GPT-5.1"),
            m("openai/gpt-5-mini", "GPT-5 mini"),
            m("openai/gpt-5", "GPT-5"),
            m("acme/thing", "Thing"),
            m("anthropic/claude-opus-4-6", "Claude Opus 4.6"),
        ];
        let groups = group(&models);
        let view: Vec<(&str, Vec<usize>, Vec<usize>)> = groups
            .iter()
            .map(|g| (g.family, g.current.clone(), g.older.clone()))
            .collect();
        assert_eq!(
            view,
            vec![
                ("Sonnet", vec![0], vec![2]),
                ("Opus", vec![1], vec![7]),
                // gpt-5-mini has no newer mini: it stays; gpt-5 is behind gpt-5.1.
                ("GPT", vec![3, 4], vec![5]),
                ("Other", vec![6], vec![]),
            ]
        );
        // Snapshots only: shown, not hidden behind an empty group.
        let only = group(&[m("claude-haiku-3-20240307", "Haiku 3")]);
        assert_eq!(
            (only[0].current.clone(), only[0].older.clone()),
            (vec![0], vec![])
        );
    }

    #[test]
    fn fuzzy_search_matches_name_and_id() {
        assert!(fuzzy_score("op 4.7", "Claude Opus 4.7", "anthropic/claude-opus-4-7").is_some());
        assert!(fuzzy_score("OPUS47", "Claude Opus 4.7", "anthropic/claude-opus-4-7").is_some());
        assert!(fuzzy_score("op 4.7", "Claude Opus 4.5", "anthropic/claude-opus-4-5").is_none());
        assert!(
            fuzzy_score(
                "anthropic/claude-opus",
                "Claude Opus 4.7",
                "anthropic/claude-opus-4-7"
            )
            .is_some()
        );
        assert!(fuzzy_score("sonnet", "Claude Opus 4.7", "anthropic/claude-opus-4-7").is_none());
        assert!(fuzzy_score("", "Anything", "x").is_some());
        let models = [
            m("anthropic/claude-sonnet-4-5", "Claude Sonnet 4.5"),
            m("anthropic/claude-opus-4-7", "Claude Opus 4.7"),
            m("openai/gpt-5", "GPT-5"),
        ];
        assert_eq!(search(&models, "op 4.7"), vec![1]);
        // A whole-word hit ranks above a spread-out one.
        assert_eq!(search(&models, "gpt")[0], 2);
        assert!(search(&models, "zzz").is_empty());
    }

    #[test]
    fn recent_keeps_three_newest_first() {
        let mut recent = Vec::new();
        for id in ["a", "b", "c", "d"] {
            assert!(push_recent(&mut recent, id));
        }
        assert_eq!(recent, vec!["d", "c", "b"]);
        assert!(!push_recent(&mut recent, "d"));
        assert!(push_recent(&mut recent, "b"));
        assert_eq!(recent, vec!["b", "d", "c"]);
        assert!(!push_recent(&mut recent, ""));
    }
    #[test]
    fn providers_come_from_prefix_then_family() {
        assert_eq!(provider("anthropic/claude-opus-4-7", ""), "Anthropic");
        assert_eq!(provider("openai/gpt-5.1", ""), "OpenAI");
        assert_eq!(provider("google/gemini-2.5-pro", ""), "Google");
        assert_eq!(provider("xai/grok-4", ""), "xAI");
        assert_eq!(
            provider("openrouter/anthropic/claude-sonnet-4-5", ""),
            "Anthropic"
        );
        assert_eq!(provider("acme/frob-2", ""), "Acme");
        assert_eq!(provider("claude-opus-4-7", "Claude Opus 4.7"), "Anthropic");
        assert_eq!(provider("gpt-5.1-codex", ""), "OpenAI");
        assert_eq!(provider("o3-mini", ""), "OpenAI");
        assert_eq!(provider("gemini-2.5-flash", ""), "Google");
        assert_eq!(provider("grok-code-fast-1", ""), "xAI");
        assert_eq!(provider("frobnicator", "Frobnicator"), OTHER);
    }

    #[test]
    fn sections_hold_families_and_one_older_fold() {
        let models = [
            m("anthropic/claude-sonnet-4-5", "Claude Sonnet 4.5"),
            m("thing", "Thing"),
            m("openai/gpt-5.1", "GPT-5.1"),
            m("anthropic/claude-opus-4-7", "Claude Opus 4.7"),
            m("anthropic/claude-sonnet-4-20250514", "Claude Sonnet 4"),
            m("openai/gpt-5", "GPT-5"),
            m("anthropic/claude-opus-4-6", "Claude Opus 4.6"),
        ];
        let s = sections(&models);
        let names: Vec<&str> = s.iter().map(|s| s.provider.as_str()).collect();
        assert_eq!(names, ["Anthropic", "OpenAI", "Other"]);
        let fams: Vec<&str> = s[0].groups.iter().map(|g| g.family).collect();
        assert_eq!(fams, ["Sonnet", "Opus"]);
        assert_eq!(s[0].older, vec![4, 6]);
        assert_eq!(s[0].count(), 4);
        assert_eq!(
            (s[1].groups[0].current.clone(), s[1].older.clone()),
            (vec![2], vec![5])
        );
        assert!(s[2].contains(1));
    }

    #[test]
    fn listed_runs_by_family_newest_first_with_breaks() {
        let models = [
            m("openai/gpt-5.1", "GPT-5.1"),
            m("openai/gpt-5-mini", "GPT-5 mini"),
            m("openai/gpt-5.1-codex", "GPT-5.1 Codex"),
            m("openai/gpt-5.2-mini", "GPT-5.2 mini"),
            m("anthropic/claude-fable-5-1", "Claude Fable 5.1"),
        ];
        let s = sections(&models);
        assert_eq!(
            s.iter().map(|s| s.provider.as_str()).collect::<Vec<_>>(),
            ["OpenAI", "Anthropic"]
        );
        // GPT newest first, then Codex after a family break; gpt-5-mini is older.
        assert_eq!(s[0].listed(), vec![(3, false), (0, false), (2, true)]);
        assert_eq!(s[0].older, vec![1]);
        // A lone family has no break, and Fable with a prefix is Anthropic's.
        assert_eq!(s[1].listed(), vec![(4, false)]);
        // Every model is listed once: shown or older.
        let total: usize = s.iter().map(|s| s.listed().len() + s.older.len()).sum();
        assert_eq!(total, models.len());
    }

    #[test]
    fn older_label_reads_as_an_action() {
        assert_eq!(older_label(13, false), "Show 13 older");
        assert_eq!(older_label(13, true), "Hide older");
    }

    #[test]
    fn current_provider_starts_open_unless_saved() {
        let models = [
            m("anthropic/claude-opus-4-7", "Opus"),
            m("openai/gpt-5.1", "GPT"),
            m("xai/grok-4", "Grok"),
        ];
        let s = sections(&models);
        assert_eq!(open_providers(None, &s, Some(1)), ["OpenAI"]);
        assert!(open_providers(None, &s, None).is_empty());
        let saved = vec!["xAI".to_string(), "Gone".to_string()];
        assert_eq!(open_providers(Some(&saved), &s, Some(1)), ["xAI"]);
        // A lone provider is open by default, and stays folded if saved so.
        let one = sections(&models[..1]);
        assert_eq!(open_providers(None, &one, None), ["Anthropic"]);
        assert!(open_providers(Some(&[]), &one, Some(0)).is_empty());
    }

    #[test]
    fn search_ignores_folds_and_groups_by_provider() {
        let models = [
            m("anthropic/claude-opus-4-7", "Claude Opus 4.7"),
            m("openai/gpt-5.1-codex", "GPT-5.1 Codex"),
            m("anthropic/claude-opus-4-20250514", "Claude Opus 4"),
        ];
        // The dated snapshot sits behind Older versions, yet search finds it.
        assert_eq!(sections(&models)[0].older, vec![2]);
        let hits = search_by_provider(&models, "opus");
        assert_eq!(hits, vec![("Anthropic".to_string(), vec![0, 2])]);
        let all = search_by_provider(&models, "c");
        assert_eq!(
            all.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(),
            ["Anthropic", "OpenAI"]
        );
        assert!(search_by_provider(&models, "zzz").is_empty());
    }
}

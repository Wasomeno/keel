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

fn is_short_version_part(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 2
        && s.bytes().all(|b| b.is_ascii_digit())
        && !(s.len() == 2 && s.starts_with('0'))
}

fn is_letter_version_prefix(s: &str) -> bool {
    let b = s.as_bytes();
    (2..=3).contains(&b.len())
        && b[0].is_ascii_alphabetic()
        && b[1..].iter().all(u8::is_ascii_digit)
}

fn is_year(s: &str) -> bool {
    s.len() == 4
        && s.bytes().all(|b| b.is_ascii_digit())
        && (s.starts_with("19") || s.starts_with("20"))
}

fn is_two_digits(s: &str) -> bool {
    s.len() == 2 && s.bytes().all(|b| b.is_ascii_digit())
}

fn is_eight_digit_date(s: &str) -> bool {
    s.len() == 8
        && s.bytes().all(|b| b.is_ascii_digit())
        && (s.starts_with("19") || s.starts_with("20"))
}

/// Splits a token like `"Qwen3.8"` into `("Qwen", "3.8")` when at least three
/// ASCII letters precede a digit.
fn split_word_version(tok: &str, out: &mut Vec<String>) {
    if let Some(split_at) = tok.bytes().position(|b| b.is_ascii_digit())
        && split_at >= 3
        && tok[..split_at].bytes().all(|b| b.is_ascii_alphabetic())
    {
        out.push(tok[..split_at].to_string());
        out.push(tok[split_at..].to_string());
        return;
    }
    out.push(tok.to_string());
}

fn format_token(tok: &str) -> String {
    if tok.is_empty() {
        return String::new();
    }
    match tok.to_ascii_lowercase().as_str() {
        "gpt" => return "GPT".to_string(),
        "glm" => return "GLM".to_string(),
        "vl" => return "VL".to_string(),
        "cc" => return "CC".to_string(),
        "ai" => return "AI".to_string(),
        "oss" => return "OSS".to_string(),
        "xl" => return "XL".to_string(),
        "xs" => return "XS".to_string(),
        "minimax" => return "MiniMax".to_string(),
        "deepseek" => return "DeepSeek".to_string(),
        "mimo" => return "MiMo".to_string(),
        "longcat" => return "LongCat".to_string(),
        "openai" => return "OpenAI".to_string(),
        "opencode" => return "OpenCode".to_string(),
        "xai" => return "xAI".to_string(),
        "chatgpt" => return "ChatGPT".to_string(),
        "stepfun" => return "StepFun".to_string(),
        _ => {}
    }
    let bytes = tok.as_bytes();
    // Parameter sizes like "40b", "122b", "1.5b", "a55b" -> uppercase trailing B.
    if bytes.len() >= 2
        && bytes.last().is_some_and(|b| b.eq_ignore_ascii_case(&b'b'))
        && bytes[bytes.len() - 2].is_ascii_digit()
    {
        let mut out = String::with_capacity(tok.len());
        let mut chars = tok.chars();
        if let Some(first) = chars.next() {
            out.push(if first.is_ascii_alphabetic() {
                first.to_ascii_uppercase()
            } else {
                first
            });
            out.extend(chars);
        }
        out.pop();
        out.push('B');
        return out;
    }
    // Single letter + version ("v4.1", "m2.7", "k3") or OpenAI reasoning ("o3", "o4").
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1].is_ascii_digit() {
        let first = if bytes[0].eq_ignore_ascii_case(&b'o')
            && bytes[1..].iter().all(u8::is_ascii_digit)
        {
            'o'
        } else {
            (bytes[0] as char).to_ascii_uppercase()
        };
        let mut out = String::with_capacity(tok.len());
        out.push(first);
        out.push_str(&tok[1..]);
        return out;
    }
    if bytes[0].is_ascii_digit() {
        return tok.to_string();
    }
    let has_upper = tok.chars().any(|c| c.is_ascii_uppercase());
    let has_lower = tok.chars().any(|c| c.is_ascii_lowercase());
    if has_upper && has_lower {
        return tok.to_string();
    }
    let mut chars = tok.chars();
    match chars.next() {
        Some(first) => first
            .to_uppercase()
            .chain(chars.flat_map(char::to_lowercase))
            .collect(),
        None => String::new(),
    }
}

fn format_plain_segment(seg: &str, tokens: &mut Vec<String>, trailing_parens: &mut Vec<String>) {
    for word in seg.split_whitespace() {
        let mut parts: Vec<&str> = word
            .split(['-', '_', ':', '@'])
            .filter(|p| !p.is_empty())
            .collect();
        if parts.len() >= 4
            && is_year(parts[parts.len() - 3])
            && is_two_digits(parts[parts.len() - 2])
            && is_two_digits(parts[parts.len() - 1])
        {
            let d = parts.pop().unwrap();
            let m = parts.pop().unwrap();
            let y = parts.pop().unwrap();
            trailing_parens.push(format!("({y}-{m}-{d})"));
        } else if parts.len() >= 2 && is_eight_digit_date(parts[parts.len() - 1]) {
            let d = parts.pop().unwrap();
            trailing_parens.push(format!("({}-{}-{})", &d[..4], &d[4..6], &d[6..8]));
        }

        let mut merged: Vec<String> = Vec::with_capacity(parts.len());
        for part in parts {
            let can_join = merged.last().is_some_and(|prev| {
                is_short_version_part(part)
                    && (is_short_version_part(prev) || is_letter_version_prefix(prev))
            });
            if can_join {
                let prev = merged.last_mut().unwrap();
                prev.push('.');
                prev.push_str(part);
            } else {
                split_word_version(part, &mut merged);
            }
        }
        for tok in merged {
            let formatted = format_token(&tok);
            if !formatted.is_empty() {
                tokens.push(formatted);
            }
        }
    }
}

/// Normalizes a model's display name (or raw ID fallback) into one consistent
/// label format: slugs like `"gemini-3.8-flash"` or
/// `"antigravity/gemini-3-8-flash"` become `"Gemini 3.8 Flash"`, hyphenated
/// names like `"MiniMax-M3.1-Flash"` and `"GLM-5.3-Flash"` become
/// `"MiniMax M3.1 Flash"` and `"GLM-5.3 Flash"`, while `"GPT-5.1"` / `"GLM-5.2"`
/// keep their hyphen and parenthetical tags like `"(latest)"` and `"(CC)"`
/// stay intact.
pub fn format_name(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let s = if !trimmed.contains(char::is_whitespace) && trimmed.contains('/') {
        base(trimmed)
    } else {
        trimmed
    };
    let mut out = String::with_capacity(s.len());
    let mut tokens: Vec<String> = Vec::new();
    let mut trailing_parens: Vec<String> = Vec::new();
    let flush_tokens = |tokens: &mut Vec<String>, out: &mut String| {
        let mut prev: Option<String> = None;
        for tok in tokens.drain(..) {
            if !out.is_empty() {
                let hyphen = prev.as_deref().is_some_and(|p| p == "GPT" || p == "GLM")
                    && tok.as_bytes().first().is_some_and(u8::is_ascii_digit);
                out.push(if hyphen { '-' } else { ' ' });
            }
            out.push_str(&tok);
            prev = Some(tok);
        }
    };

    let mut rest = s;
    while let Some(open) = rest.find('(') {
        let before = &rest[..open];
        format_plain_segment(before, &mut tokens, &mut trailing_parens);
        flush_tokens(&mut tokens, &mut out);
        let after_open = &rest[open + 1..];
        let Some(close) = after_open.find(')') else {
            rest = &rest[open..];
            break;
        };
        let inner = after_open[..close].trim();
        if !inner.is_empty() {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push('(');
            out.push_str(inner);
            out.push(')');
        }
        rest = &after_open[close + 1..];
    }
    format_plain_segment(rest, &mut tokens, &mut trailing_parens);
    flush_tokens(&mut tokens, &mut out);
    for paren in trailing_parens {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&paren);
    }
    out
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
    ("minimax", "MiniMax"),
    ("minimaxai", "MiniMax"),
    ("opencode", "OpenCode"),
    ("opencode-go", "OpenCode Go"),
    ("commandcode", "Command Code"),
    ("command-code", "Command Code"),
    ("antigravity", "Antigravity"),
    ("zai-org", "Z.ai"),
    ("xiaomi", "Xiaomi"),
    ("stepfun", "StepFun"),
    ("tencent", "Tencent"),
    ("meituan", "Meituan"),
    ("nvidia", "NVIDIA"),
    ("sakana", "Sakana"),
    ("thinkingmachines", "Thinking Machines"),
    ("inclusionai", "InclusionAI"),
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

    #[test]
    fn provider_names_are_consistently_cased() {
        assert_eq!(provider("minimax/MiniMax-M3", ""), "MiniMax");
        assert_eq!(provider("opencode/beam-free", ""), "OpenCode");
        assert_eq!(provider("opencode-go/hy4", ""), "OpenCode Go");
        assert_eq!(provider("antigravity/gemini-3-8-flash", ""), "Antigravity");
        assert_eq!(provider("commandcode/grok-4.5", ""), "Command Code");
    }

    #[test]
    fn format_name_unifies_slugs_hyphens_and_casing() {
        assert_eq!(format_name("gemini-3.8-flash"), "Gemini 3.8 Flash");
        assert_eq!(
            format_name("antigravity/gemini-3-8-flash"),
            "Gemini 3.8 Flash"
        );
        assert_eq!(
            format_name("antigravity/claude-sonnet-5-5"),
            "Claude Sonnet 5.5"
        );
        assert_eq!(format_name("Grok 4.5"), "Grok 4.5");
        assert_eq!(format_name("grok-build-0.1"), "Grok Build 0.1");
        assert_eq!(format_name("MiniMax-M2.7"), "MiniMax M2.7");
        assert_eq!(
            format_name("MiniMax-M2.7-highspeed"),
            "MiniMax M2.7 Highspeed"
        );
        assert_eq!(format_name("MiniMax-M3.1-Flash"), "MiniMax M3.1 Flash");
        assert_eq!(format_name("GLM-5.2"), "GLM-5.2");
        assert_eq!(format_name("GLM-5.3-Flash"), "GLM-5.3 Flash");
        assert_eq!(format_name("GLM-5.3-Flash Free"), "GLM-5.3 Flash Free");
        assert_eq!(format_name("MiMo-V2.6-Flash"), "MiMo V2.6 Flash");
        assert_eq!(format_name("LongCat-2.0"), "LongCat 2.0");
        assert_eq!(format_name("Hy4 preview"), "Hy4 Preview");
        assert_eq!(format_name("Qwen3.8 Flash"), "Qwen 3.8 Flash");
        assert_eq!(format_name("GPT-5.1 Codex"), "GPT-5.1 Codex");
        assert_eq!(format_name("GPT-5 mini"), "GPT-5 Mini");
        assert_eq!(format_name("gpt-4o-mini"), "GPT-4o Mini");
        assert_eq!(format_name("o3-mini"), "o3 Mini");
        assert_eq!(format_name("flint-40b-free"), "Flint 40B Free");
        assert_eq!(
            format_name("nemotron-3.5-super-vl-free"),
            "Nemotron 3.5 Super VL Free"
        );
        assert_eq!(format_name("stealth/glyph-cluster:free"), "Glyph Cluster Free");
        assert_eq!(
            format_name("Claude Haiku 4.5 (latest)"),
            "Claude Haiku 4.5 (latest)"
        );
        assert_eq!(
            format_name("claude-haiku-4-5-20251001"),
            "Claude Haiku 4.5 (2025-10-01)"
        );
        assert_eq!(
            format_name("gpt-4o-2024-08-06"),
            "GPT-4o (2024-08-06)"
        );
        assert_eq!(
            format_name("DeepSeek V4 Flash Vision (exp) (CC)"),
            "DeepSeek V4 Flash Vision (exp) (CC)"
        );
        assert_eq!(format_name("Kimi K2.7 Code HighSpeed (CC)"), "Kimi K2.7 Code HighSpeed (CC)");
        assert_eq!(format_name("Model A"), "Model A");
        assert_eq!(format_name(""), "");
    }
}

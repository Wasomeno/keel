//! Token usage as the registry mirrors it: per chat, per model, plus the
//! chat's context after its last turn. Totals per harness are summed here.

use serde_json::Value;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelTokens {
    pub model: String,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl ModelTokens {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }

    fn add(&mut self, other: &ModelTokens) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatUsage {
    pub models: Vec<ModelTokens>,
    pub context_used: u64,
    /// 0: the harness doesn't report one.
    pub context_window: u64,
}

impl ChatUsage {
    /// The chat view's `usage`: `{models: {name: {input, ...}}, context_used, context_window}`.
    pub fn from_view(view: &Value) -> Self {
        let n = |v: &Value, k: &str| v[k].as_u64().unwrap_or(0);
        let mut models: Vec<ModelTokens> = view["models"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(model, t)| ModelTokens {
                model: model.clone(),
                input: n(t, "input"),
                output: n(t, "output"),
                cache_read: n(t, "cache_read"),
                cache_write: n(t, "cache_write"),
            })
            .collect();
        models.sort_by(|a, b| b.total().cmp(&a.total()).then(a.model.cmp(&b.model)));
        Self {
            models,
            context_used: n(view, "context_used"),
            context_window: n(view, "context_window"),
        }
    }

    pub fn total(&self) -> u64 {
        self.models.iter().map(ModelTokens::total).sum()
    }

    /// How full the context is, 0..1, when the window is known.
    pub fn fill(&self) -> Option<f32> {
        (self.context_window > 0)
            .then(|| (self.context_used as f64 / self.context_window as f64).min(1.) as f32)
    }
}

/// One harness's lifetime tokens and its models, biggest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessTotals {
    pub harness: String,
    pub models: Vec<ModelTokens>,
}

impl HarnessTotals {
    pub fn total(&self) -> u64 {
        self.models.iter().map(ModelTokens::total).sum()
    }
}

/// Sum chats' usage by harness, then model. Harnesses with nothing spent are left out.
pub fn by_harness<'a>(
    chats: impl IntoIterator<Item = (&'a str, &'a ChatUsage)>,
) -> Vec<HarnessTotals> {
    let mut out: Vec<HarnessTotals> = Vec::new();
    for (harness, usage) in chats {
        for m in usage.models.iter().filter(|m| m.total() > 0) {
            let entry = match out.iter().position(|h| h.harness == harness) {
                Some(i) => &mut out[i],
                None => {
                    out.push(HarnessTotals {
                        harness: harness.to_string(),
                        models: Vec::new(),
                    });
                    out.last_mut().unwrap()
                }
            };
            match entry.models.iter_mut().find(|t| t.model == m.model) {
                Some(t) => t.add(m),
                None => entry.models.push(m.clone()),
            }
        }
    }
    for h in &mut out {
        h.models
            .sort_by(|a, b| b.total().cmp(&a.total()).then(a.model.cmp(&b.model)));
    }
    out.sort_by(|a, b| b.total().cmp(&a.total()).then(a.harness.cmp(&b.harness)));
    out
}

/// "950", "12.4k", "128k", "1.2M".
pub fn compact(n: u64) -> String {
    let f = n as f64;
    if n < 1_000 {
        n.to_string()
    } else if n < 100_000 {
        trim(format!("{:.1}", f / 1e3)) + "k"
    } else if n < 1_000_000 {
        format!("{:.0}k", f / 1e3)
    } else if n < 100_000_000 {
        trim(format!("{:.1}", f / 1e6)) + "M"
    } else {
        format!("{:.0}M", f / 1e6)
    }
}

fn trim(s: String) -> String {
    s.strip_suffix(".0").map(str::to_string).unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_counts() {
        assert_eq!(compact(950), "950");
        assert_eq!(compact(1_000), "1k");
        assert_eq!(compact(12_440), "12.4k");
        assert_eq!(compact(128_000), "128k");
        assert_eq!(compact(1_260_000), "1.3M");
        assert_eq!(compact(250_000_000), "250M");
    }

    #[test]
    fn view_parses_and_fill_needs_a_window() {
        let usage = ChatUsage::from_view(&serde_json::json!({
            "models": {"a": {"input": 1, "output": 2}, "b": {"input": 10, "cache_read": 90}},
            "context_used": 50_000, "context_window": 200_000,
        }));
        assert_eq!(usage.models[0].model, "b");
        assert_eq!(usage.total(), 103);
        assert_eq!(usage.fill(), Some(0.25));
        assert_eq!(
            ChatUsage {
                context_used: 9,
                ..Default::default()
            }
            .fill(),
            None
        );
    }

    #[test]
    fn totals_group_by_harness_then_model() {
        let tokens = |model: &str, input| ModelTokens {
            model: model.into(),
            input,
            ..Default::default()
        };
        let chat = |models| ChatUsage {
            models,
            ..Default::default()
        };
        let (a, b, c) = (
            chat(vec![tokens("opus", 100), tokens("haiku", 5)]),
            chat(vec![tokens("opus", 50)]),
            chat(vec![tokens("gpt", 30), tokens("idle", 0)]),
        );
        let totals = by_harness([("claude_code", &a), ("claude_code", &b), ("codex", &c)]);
        assert_eq!(totals.len(), 2);
        assert_eq!(
            (totals[0].harness.as_str(), totals[0].total()),
            ("claude_code", 155)
        );
        assert_eq!(
            totals[0]
                .models
                .iter()
                .map(|m| (m.model.as_str(), m.input))
                .collect::<Vec<_>>(),
            [("opus", 150), ("haiku", 5)]
        );
        // A model that spent nothing isn't listed.
        assert_eq!(totals[1].models.len(), 1);
    }
}

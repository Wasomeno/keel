//! A chart the engine's generative UI chose for a reply (a `ui` part with
//! `component: "chart"`). This is the model and its axis math; the painter
//! is `transcript::paint_chart`.

use serde_json::Value;

/// Canvas height of an inline chart, axes included.
pub const HEIGHT: f32 = 200.;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bar,
    Line,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    pub name: String,
    /// One per label; `None` is a gap in the table (no bar, a break in the line).
    pub values: Vec<Option<f64>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Chart {
    pub kind: Kind,
    /// The x-axis column's name.
    pub x: String,
    pub labels: Vec<String>,
    pub series: Vec<Series>,
}

// `from_spec` keeps only finite values, so equality is total.
impl Eq for Chart {}

impl Chart {
    /// A chart spec; `None` for another component or a malformed one.
    pub fn from_spec(spec: &Value) -> Option<Self> {
        if spec["component"] != "chart" {
            return None;
        }
        let labels: Vec<String> = spec["labels"].as_array()?.iter().map(|l| l.as_str().unwrap_or_default().to_string()).collect();
        let series: Vec<Series> = spec["series"]
            .as_array()?
            .iter()
            .filter_map(|s| {
                // A number or null (a gap); anything else spoils the series.
                let values: Vec<Option<f64>> = s["values"]
                    .as_array()?
                    .iter()
                    .map(|v| if v.is_null() { Some(None) } else { v.as_f64().filter(|v| v.is_finite()).map(Some) })
                    .collect::<Option<_>>()?;
                (values.len() == labels.len() && values.iter().flatten().count() >= 2)
                    .then(|| Series { name: s["name"].as_str().unwrap_or_default().to_string(), values })
            })
            .collect();
        if labels.is_empty() || series.is_empty() {
            return None;
        }
        let kind = if spec["kind"] == "line" { Kind::Line } else { Kind::Bar };
        Some(Self { kind, x: spec["x"].as_str().unwrap_or_default().to_string(), labels, series })
    }

    /// The value axis: from zero (or the lowest value, if negative) to a
    /// round number at or above the highest, with its tick step.
    pub fn axis(&self) -> (f64, f64, f64) {
        let values = self.series.iter().flat_map(|s| s.values.iter().flatten().copied());
        let (lo, hi) = values.fold((0f64, 0f64), |(lo, hi), v| (lo.min(v), hi.max(v)));
        let step = nice_step((hi - lo).max(f64::EPSILON) / 5.);
        ((lo / step).floor() * step, ((hi / step).ceil() * step).max(step), step)
    }

    /// Whether this chart was drawn from a table with these cells: its x
    /// column holds the labels in order and each series is a column. The
    /// chart stands in for that table; content, not position, so a table
    /// counted differently can never be swapped for another one's chart.
    pub fn is_of(&self, header: &[&str], rows: &[Vec<&str>]) -> bool {
        let Some(x) = header.iter().position(|h| *h == self.x) else {
            return false;
        };
        rows.len() == self.labels.len()
            && rows.iter().zip(&self.labels).all(|(row, label)| row.get(x) == Some(&label.as_str()))
            && self.series.iter().all(|s| header.contains(&s.name.as_str()))
    }

    /// For the transcript's height memo.
    pub fn content_len(&self) -> usize {
        self.labels.iter().map(|l| l.len() + 1).sum::<usize>() + self.series.len() * self.labels.len()
    }

    /// Format this chart as a markdown table.
    pub fn to_markdown(&self) -> String {
        let mut out = format!("| {} |", self.x);
        for s in &self.series {
            out.push_str(&format!(" {} |", s.name));
        }
        out.push_str("\n|");
        out.push_str(&" --- |".repeat(self.series.len() + 1));
        out.push('\n');
        for (ix, label) in self.labels.iter().enumerate() {
            out.push_str(&format!("| {label} |"));
            for s in &self.series {
                let val_str = s.values.get(ix).and_then(|v| *v).map(exact).unwrap_or_default();
                out.push_str(&format!(" {val_str} |"));
            }
            out.push('\n');
        }
        out
    }
}

/// 1, 2 or 5 times a power of ten, at or above `raw`.
fn nice_step(raw: f64) -> f64 {
    let pow = 10f64.powf(raw.log10().floor());
    [1., 2., 5., 10.].into_iter().map(|m| m * pow).find(|&s| s >= raw).unwrap_or(10. * pow)
}

/// Plot box inside a chart canvas: left, top, width, height, in px.
pub fn plot_box(width: f32, height: f32) -> (f32, f32, f32, f32) {
    const LEFT: f32 = 44.;
    const RIGHT: f32 = 12.;
    const TOP: f32 = 10.;
    const BOTTOM: f32 = 26.;
    (LEFT, TOP, (width - LEFT - RIGHT).max(20.), (height - TOP - BOTTOM).max(20.))
}

/// The category under a pointer in the plot, if any.
pub fn category_at(chart: &Chart, plot_w: f32, x: f32) -> Option<usize> {
    let n = chart.labels.len();
    if n == 0 || x < 0. || x >= plot_w {
        return None;
    }
    Some(((x / plot_w) * n as f32) as usize).filter(|&ix| ix < n)
}

/// What a hover shows: `ui · before (s) 41.2`. One line per series that has a value.
pub fn hover_rows(chart: &Chart, ix: usize) -> Option<(String, Vec<(usize, String, String)>)> {
    let name = chart.labels.get(ix)?.clone();
    let rows: Vec<(usize, String, String)> = chart
        .series
        .iter()
        .enumerate()
        .filter_map(|(si, series)| Some((si, series.name.clone(), exact(series.values.get(ix).copied().flatten()?))))
        .collect();
    (!rows.is_empty()).then_some((name, rows))
}

/// A value as stated, for the hover card: `482,345`, `41.2`, `-1,234.5`.
pub fn exact(v: f64) -> String {
    let text = format!("{:.2}", v.abs());
    let text = text.trim_end_matches('0').trim_end_matches('.');
    let (whole, frac) = text.split_once('.').map_or((text, None), |(w, f)| (w, Some(f)));
    let mut grouped = String::new();
    for (i, ch) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    let sign = if v < 0. && text != "0" { "-" } else { "" };
    match frac {
        Some(frac) => format!("{sign}{grouped}.{frac}"),
        None => format!("{sign}{grouped}"),
    }
}

/// A tick or value label: `1.5k`, `40`, `2.5`, `0.25`.
pub fn label(v: f64) -> String {
    let trim = |s: String| if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
    if v.abs() >= 1000. {
        format!("{}k", trim(format!("{:.1}", v / 1000.)))
    } else {
        trim(format!("{:.2}", v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec() -> Value {
        json!({ "component": "chart", "kind": "bar", "x": "crate", "labels": ["ui", "engine"],
                "series": [{ "name": "before", "values": [41.2, 33.5] }, { "name": "bad", "values": [1] }] })
    }

    #[test]
    fn chart_to_markdown_produces_table() {
        let chart = Chart::from_spec(&spec()).unwrap();
        let md = chart.to_markdown();
        assert!(md.contains("| crate | before |"));
        assert!(md.contains("| ui | 41.2 |"));
        assert!(md.contains("| engine | 33.5 |"));
    }

    #[test]
    fn spec_keeps_series_that_match_the_labels() {
        let chart = Chart::from_spec(&spec()).expect("chart");
        assert_eq!(chart.kind, Kind::Bar);
        assert_eq!(chart.series.len(), 1);
        assert!(Chart::from_spec(&json!({ "component": "mr_card" })).is_none());
    }

    #[test]
    fn a_gap_is_kept_as_a_gap() {
        let spec = json!({ "component": "chart", "kind": "bar", "x": "c", "labels": ["a", "b", "c"],
            "series": [{ "name": "kb", "values": [1200.0, null, 900.0] }] });
        let chart = Chart::from_spec(&spec).expect("chart");
        assert_eq!(chart.series[0].values, [Some(1200.), None, Some(900.)]);
    }

    #[test]
    fn axis_starts_at_zero_and_ends_on_a_round_tick() {
        let chart = Chart::from_spec(&spec()).expect("chart");
        assert_eq!(chart.axis(), (0., 50., 10.));
        let mut negative = chart.clone();
        negative.series[0].values = vec![Some(-3.), Some(7.)];
        assert_eq!(negative.axis(), (-4., 8., 2.));
    }

    #[test]
    fn a_chart_is_of_the_table_holding_its_labels_and_series() {
        let chart = Chart::from_spec(&spec()).expect("chart");
        let rows = vec![vec!["ui", "41.2"], vec!["engine", "33.5"]];
        assert!(chart.is_of(&["crate", "before"], &rows));
        assert!(!chart.is_of(&["crate", "after"], &rows));
        assert!(!chart.is_of(&["crate", "before"], &rows[..1]));
        assert!(!chart.is_of(&["crate", "before"], &[vec!["engine", "1"], vec!["ui", "2"]]));
    }

    #[test]
    fn a_hover_names_the_category_and_its_values() {
        let chart = Chart::from_spec(&spec()).unwrap();
        assert_eq!(category_at(&chart, 200., 10.), Some(0));
        assert_eq!(category_at(&chart, 200., 150.), Some(1));
        assert_eq!(category_at(&chart, 200., 200.), None);
        assert_eq!(hover_rows(&chart, 0).unwrap(), ("ui".to_string(), vec![(0, "before".to_string(), "41.2".to_string())]));
    }

    #[test]
    fn hover_values_are_exact() {
        assert_eq!(exact(482_345.), "482,345");
        assert_eq!(exact(1000.), "1,000");
        assert_eq!(exact(999.), "999");
        assert_eq!(exact(41.2), "41.2");
        assert_eq!(exact(-1234.5), "-1,234.5");
        assert_eq!(exact(0.25), "0.25");
        assert_eq!(exact(0.), "0");
    }

    #[test]
    fn labels_are_short() {
        assert_eq!(label(1500.), "1.5k");
        assert_eq!(label(40.), "40");
        assert_eq!(label(2.5), "2.5");
        assert_eq!(label(0.25), "0.25");
    }
}

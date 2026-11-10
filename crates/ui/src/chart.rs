//! A chart the engine's generative UI chose for a reply (a `ui` part with
//! `component: "chart"`). This is the model, its axis math, CSV/image export,
//! and curve math; the painter is `transcript::paint_chart`.

use keel_theme::Tokens;
use serde_json::Value;

/// Canvas height of an inline chart, axes included.
pub const HEIGHT: f32 = 210.;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bar,
    Line,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Headline {
    pub value: String,
    pub label: Option<String>,
    pub delta: Option<String>,
    pub better: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    pub name: String,
    /// One per label; `None` is a gap in the table (no bar, a break in the line).
    pub values: Vec<Option<f64>>,
    pub highlight: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Chart {
    pub kind: Kind,
    pub title: Option<String>,
    pub unit: Option<String>,
    pub headline: Option<Headline>,
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
        let labels: Vec<String> = spec["labels"]
            .as_array()?
            .iter()
            .map(|l| l.as_str().unwrap_or_default().to_string())
            .collect();
        let series: Vec<Series> = spec["series"]
            .as_array()?
            .iter()
            .filter_map(|s| {
                // A number or null (a gap); anything else spoils the series.
                let values: Vec<Option<f64>> = s["values"]
                    .as_array()?
                    .iter()
                    .map(|v| {
                        if v.is_null() {
                            Some(None)
                        } else {
                            v.as_f64().filter(|v| v.is_finite()).map(Some)
                        }
                    })
                    .collect::<Option<_>>()?;
                let highlight = s["highlight"].as_bool().unwrap_or(false)
                    || s["accent"].as_bool().unwrap_or(false);
                (values.len() == labels.len() && values.iter().flatten().count() >= 2).then(|| {
                    Series {
                        name: s["name"].as_str().unwrap_or_default().to_string(),
                        values,
                        highlight,
                    }
                })
            })
            .collect();
        if labels.is_empty() || series.is_empty() {
            return None;
        }
        let kind = if spec["kind"] == "line" {
            Kind::Line
        } else {
            Kind::Bar
        };
        let title = spec["title"]
            .as_str()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(String::from);
        let unit = spec["unit"]
            .as_str()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(String::from);
        let headline = spec["headline"].as_object().and_then(|h| {
            let value = h.get("value")?.as_str()?.trim().to_string();
            if value.is_empty() {
                return None;
            }
            let label = h
                .get("label")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let delta = h
                .get("delta")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let better = h
                .get("better")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            Some(Headline {
                value,
                label,
                delta,
                better,
            })
        });

        Some(Self {
            kind,
            title,
            unit,
            headline,
            x: spec["x"].as_str().unwrap_or_default().to_string(),
            labels,
            series,
        })
    }

    /// Primary display title: payload title if provided, or fallback "<series names> by <x key>" in sentence case.
    /// Never shows the chart type.
    pub fn display_title(&self) -> String {
        if let Some(ref t) = self.title {
            let trimmed = t.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        self.fallback_title()
    }

    /// Fallback title: "<series names> by <x key>" in sentence case, e.g. "Revenue and costs by month".
    pub fn fallback_title(&self) -> String {
        let clean_name = |name: &str| -> String {
            let s = name.trim();
            if let Some(pos) = s.rfind('(') {
                if s.ends_with(')') {
                    return s[..pos].trim().to_string();
                }
            }
            s.to_string()
        };

        let names: Vec<String> = self.series.iter().map(|s| clean_name(&s.name)).collect();
        let series_text = match names.len() {
            0 => "Values".to_string(),
            1 => names[0].clone(),
            2 => {
                let first = &names[0];
                let second = lower_word(&names[1]);
                format!("{first} and {second}")
            }
            _ => {
                let mut out = String::new();
                for (i, name) in names.iter().enumerate() {
                    if i == 0 {
                        out.push_str(name);
                    } else if i == names.len() - 1 {
                        out.push_str(&format!(", and {}", lower_word(name)));
                    } else {
                        out.push_str(&format!(", {}", lower_word(name)));
                    }
                }
                out
            }
        };

        let series_text = sentence_case_first(&series_text);
        let x_name = self.x.trim();
        if x_name.is_empty() {
            series_text
        } else {
            let x_lower = lower_word(x_name);
            format!("{series_text} by {x_lower}")
        }
    }

    /// Effective unit: payload unit if provided, or extracted unit suffix from series (e.g. "$k", "%", "KB").
    pub fn effective_unit(&self) -> Option<String> {
        if let Some(ref u) = self.unit {
            let t = u.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
        for s in &self.series {
            if let Some(pos) = s.name.rfind('(') {
                if s.name.ends_with(')') {
                    let inside = s.name[pos + 1..s.name.len() - 1].trim();
                    if !inside.is_empty() && inside.len() <= 8 {
                        return Some(inside.to_string());
                    }
                }
            }
        }
        None
    }

    /// Format a tick number with unit context (e.g. "$20k", "40%", "840 KB").
    pub fn format_tick(&self, v: f64) -> String {
        let raw = label(v);
        let Some(u) = self.effective_unit() else {
            return raw;
        };
        format_with_unit(&raw, &u)
    }

    /// Format an exact value for the hover tooltip with unit context (e.g. "$41.20k", "81%", "840 KB").
    pub fn format_value(&self, v: f64) -> String {
        let raw = exact(v);
        let Some(u) = self.effective_unit() else {
            return raw;
        };
        format_with_unit(&raw, &u)
    }

    /// The value axis: from zero (or the lowest value, if negative) to a
    /// round number at or above the highest, with its tick step.
    pub fn axis(&self) -> (f64, f64, f64) {
        let all_active = vec![true; self.series.len()];
        self.axis_for(&all_active)
    }

    /// The value axis for a subset of active/visible series.
    pub fn axis_for(&self, active: &[bool]) -> (f64, f64, f64) {
        let values: Vec<f64> = self
            .series
            .iter()
            .enumerate()
            .filter(|(ix, _)| active.get(*ix).copied().unwrap_or(true))
            .flat_map(|(_, s)| s.values.iter().flatten().copied())
            .collect();
        if values.is_empty() {
            return (0., 10., 2.);
        }
        let (lo, hi) = values
            .into_iter()
            .fold((0f64, 0f64), |(lo, hi), v| (lo.min(v), hi.max(v)));
        let span = (hi - lo).max(f64::EPSILON);
        let step = nice_step(span / 5.);
        (
            (lo / step).floor() * step,
            ((hi / step).ceil() * step).max(step),
            step,
        )
    }

    /// Whether this chart was drawn from a table with these cells.
    pub fn is_of(&self, header: &[&str], rows: &[Vec<&str>]) -> bool {
        let Some(x) = header.iter().position(|h| *h == self.x) else {
            return false;
        };
        rows.len() == self.labels.len()
            && rows
                .iter()
                .zip(&self.labels)
                .all(|(row, label)| row.get(x) == Some(&label.as_str()))
            && self
                .series
                .iter()
                .all(|s| header.contains(&s.name.as_str()))
    }

    /// For the transcript's height memo.
    pub fn content_len(&self) -> usize {
        self.labels.iter().map(|l| l.len() + 1).sum::<usize>()
            + self.series.len() * self.labels.len()
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
                let val_str = s
                    .values
                    .get(ix)
                    .and_then(|v| *v)
                    .map(exact)
                    .unwrap_or_default();
                out.push_str(&format!(" {val_str} |"));
            }
            out.push('\n');
        }
        out
    }

    /// Format this chart as CSV for "Copy as CSV".
    pub fn to_csv(&self) -> String {
        let mut out = String::new();
        out.push_str(&csv_escape(&self.x));
        for s in &self.series {
            out.push(',');
            out.push_str(&csv_escape(&s.name));
        }
        out.push('\n');
        for (ix, label) in self.labels.iter().enumerate() {
            out.push_str(&csv_escape(label));
            for s in &self.series {
                out.push(',');
                if let Some(Some(v)) = s.values.get(ix) {
                    out.push_str(&exact(*v).replace(',', ""));
                }
            }
            out.push('\n');
        }
        out
    }

    /// Generate an SVG representation of the chart for image export.
    pub fn to_svg(&self, t: &Tokens) -> String {
        let title = self.display_title();
        let unit_opt = self.effective_unit();
        let (lo, hi, step) = self.axis();
        let (width, height) = (540., 280.);
        let pad = 24.;
        let plot_l = pad + 40.;
        let plot_t = 64.;
        let plot_w = width - plot_l - pad;
        let plot_h = height - plot_t - 36.;

        let bg_hex = format!("#{:06x}", t.code & 0x00FFFFFF);
        let ink_hex = format!("#{:06x}", t.text & 0x00FFFFFF);
        let muted_hex = format!("#{:06x}", t.text_muted & 0x00FFFFFF);
        let faint_hex = format!("#{:06x}", t.text_faint & 0x00FFFFFF);

        let mut svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">
  <rect width="100%" height="100%" fill="{bg_hex}" rx="8"/>
  <text x="{pad}" y="32" font-family="-apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Helvetica, Arial, sans-serif" font-size="13" font-weight="500" fill="{muted_hex}">{title}</text>
"#
        );

        // Legend
        let mut legend_x = pad;
        let legend_y = 48.;
        for (si, s) in self.series.iter().enumerate() {
            let color = series_svg_color(t, si, s.highlight);
            svg.push_str(&format!(
                r#"  <rect x="{legend_x}" y="{}" width="8" height="8" rx="2" fill="{color}"/>
  <text x="{}" y="{legend_y}" font-family="-apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Helvetica, Arial, sans-serif" font-size="11" fill="{muted_hex}">{}</text>
"#,
                legend_y - 7.,
                legend_x + 12.,
                escape_xml(&s.name)
            ));
            legend_x += 20. + s.name.len() as f32 * 6.5;
        }

        // Y-axis grid and ticks
        let mut tick = lo;
        let y_of = |v: f64| plot_t + plot_h * (1. - ((v - lo) / (hi - lo).max(f64::EPSILON)) as f32);
        while tick <= hi + step * 0.1 {
            let y = y_of(tick);
            let stroke_opacity = if tick == 0. { "0.20" } else { "0.08" };
            svg.push_str(&format!(
                r#"  <line x1="{plot_l}" y1="{y:.1}" x2="{}" y2="{y:.1}" stroke="{ink_hex}" stroke-opacity="{stroke_opacity}" stroke-width="1"/>
"#,
                plot_l + plot_w
            ));
            let tick_label = format_with_unit(&label(tick), unit_opt.as_deref().unwrap_or(""));
            svg.push_str(&format!(
                r#"  <text x="{}" y="{:.1}" text-anchor="end" font-family="-apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Helvetica, Arial, sans-serif" font-size="10" fill="{faint_hex}">{tick_label}</text>
"#,
                plot_l - 8.,
                y + 3.5
            ));
            tick += step;
        }

        let n = self.labels.len();
        let slot = plot_w / n as f32;
        let zero_y = y_of(0.0f64.clamp(lo, hi));

        // X-axis labels
        let every = (64. / slot).ceil().max(1.) as usize;
        for (ix, name) in self.labels.iter().enumerate().step_by(every) {
            let x = plot_l + slot * (ix as f32 + 0.5);
            svg.push_str(&format!(
                r#"  <text x="{x:.1}" y="{:.1}" text-anchor="middle" font-family="-apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Helvetica, Arial, sans-serif" font-size="10" fill="{faint_hex}">{}</text>
"#,
                plot_t + plot_h + 16.,
                escape_xml(name)
            ));
        }

        match self.kind {
            Kind::Bar => {
                let k = self.series.len() as f32;
                let group_w = (slot * 0.6).min(48.);
                let bar_w = ((group_w - (k - 1.) * 2.) / k).max(2.);
                for (si, series) in self.series.iter().enumerate() {
                    let color = series_svg_color(t, si, series.highlight);
                    for (ix, v) in series.values.iter().enumerate() {
                        let Some(v) = *v else { continue };
                        let group_l = plot_l + slot * (ix as f32 + 0.5) - group_w / 2.;
                        let bx = group_l + si as f32 * (bar_w + 2.);
                        let (top_y, bottom_y) = (y_of(v).min(zero_y), y_of(v).max(zero_y));
                        let bh = (bottom_y - top_y).max(2.);
                        svg.push_str(&format!(
                            r#"  <rect x="{bx:.1}" y="{top_y:.1}" width="{bar_w:.1}" height="{bh:.1}" rx="4" ry="4" fill="{color}"/>
"#
                        ));
                    }
                }
            }
            Kind::Line => {
                for (si, series) in self.series.iter().enumerate() {
                    let color = series_svg_color(t, si, series.highlight);
                    let points: Vec<Option<(f32, f32)>> = series
                        .values
                        .iter()
                        .enumerate()
                        .map(|(ix, v)| v.map(|v| (plot_l + slot * (ix as f32 + 0.5), y_of(v))))
                        .collect();

                    for run in points.split(Option::is_none) {
                        let pts: Vec<(f32, f32)> = run.iter().flatten().copied().collect();
                        if pts.len() < 2 {
                            continue;
                        }
                        let spline = monotone_spline(&pts);
                        if spline.is_empty() {
                            continue;
                        }
                        // Optional faint area fill for first series
                        if si == 0 {
                            let mut area_path = format!("M {:.1} {:.1}", spline[0].0, zero_y);
                            for &(px, py) in &spline {
                                area_path.push_str(&format!(" L {px:.1} {py:.1}"));
                            }
                            area_path.push_str(&format!(
                                " L {:.1} {:.1} Z",
                                spline.last().unwrap().0,
                                zero_y
                            ));
                            svg.push_str(&format!(
                                r#"  <path d="{area_path}" fill="{color}" fill-opacity="0.06"/>
"#
                            ));
                        }
                        let mut line_path = format!("M {:.1} {:.1}", spline[0].0, spline[0].1);
                        for &(px, py) in &spline[1..] {
                            line_path.push_str(&format!(" L {px:.1} {py:.1}"));
                        }
                        svg.push_str(&format!(
                            r#"  <path d="{line_path}" fill="none" stroke="{color}" stroke-width="1.75"/>
"#
                        ));
                    }
                }
            }
        }

        svg.push_str("</svg>");
        svg
    }

    /// Background PNG export job for "Copy as image" at 2x resolution.
    pub fn png_job(
        &self,
        t: &Tokens,
    ) -> Option<impl FnOnce() -> Option<Vec<u8>> + Send + 'static> {
        let svg = self.to_svg(t);
        let options = resvg::usvg::Options {
            fontdb: crate::mermaid::FONTS.clone(),
            ..Default::default()
        };
        let tree = resvg::usvg::Tree::from_str(&svg, &options).ok()?;
        let (width, height) = (tree.size().width(), tree.size().height());
        let scale = 2.0f32;
        let pw = (width * scale).ceil() as u32;
        let ph = (height * scale).ceil() as u32;

        Some(move || {
            let mut pixmap = resvg::tiny_skia::Pixmap::new(pw, ph)?;
            let transform = resvg::tiny_skia::Transform::from_scale(scale, scale);
            resvg::render(&tree, transform, &mut pixmap.as_mut());
            let rgba = image::RgbaImage::from_raw(pw, ph, pixmap.take())?;
            let mut png = std::io::Cursor::new(Vec::new());
            rgba.write_to(&mut png, image::ImageFormat::Png).ok()?;
            Some(png.into_inner())
        })
    }
}

fn series_svg_color(t: &Tokens, ix: usize, highlight: bool) -> String {
    if highlight {
        return format!("#{:06x}", t.accent & 0x00FFFFFF);
    }
    let ink = format!("#{:06x}", t.text & 0x00FFFFFF);
    match ix {
        0 => ink,
        1 => format!("rgba({}, {}, {}, 0.40)", (t.text >> 16) & 0xFF, (t.text >> 8) & 0xFF, t.text & 0xFF),
        2 => format!("rgba({}, {}, {}, 0.24)", (t.text >> 16) & 0xFF, (t.text >> 8) & 0xFF, t.text & 0xFF),
        _ => format!("rgba({}, {}, {}, 0.15)", (t.text >> 16) & 0xFF, (t.text >> 8) & 0xFF, t.text & 0xFF),
    }
}

fn format_with_unit(raw: &str, unit: &str) -> String {
    let u = unit.trim();
    if u.is_empty() {
        return raw.to_string();
    }
    if u == "$" {
        format!("${raw}")
    } else if u.starts_with('$') {
        let suffix = &u[1..];
        if raw.ends_with(suffix) {
            format!("${raw}")
        } else {
            format!("${raw}{suffix}")
        }
    } else if u == "%" {
        format!("{raw}%")
    } else {
        format!("{raw} {u}")
    }
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn lower_word(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    // If all characters are uppercase (e.g. CPU, API, URL, ID, KB), keep uppercase
    if trimmed.chars().all(|c| !c.is_alphabetic() || c.is_uppercase()) {
        return trimmed.to_string();
    }
    let mut chars = trimmed.chars();
    let first = chars.next().unwrap();
    format!("{}{}", first.to_lowercase(), chars.as_str())
}

fn sentence_case_first(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let mut chars = trimmed.chars();
    let first = chars.next().unwrap();
    format!("{}{}", first.to_uppercase(), chars.as_str())
}

/// Fritsch-Carlson monotone cubic spline interpolation:
/// Evaluates points smoothly between samples with zero overshoot.
pub fn monotone_spline(pts: &[(f32, f32)]) -> Vec<(f32, f32)> {
    let n = pts.len();
    if n < 2 {
        return pts.to_vec();
    }
    if n == 2 {
        return pts.to_vec();
    }

    let mut dx = Vec::with_capacity(n - 1);
    let mut dy = Vec::with_capacity(n - 1);
    let mut m = Vec::with_capacity(n - 1);

    for i in 0..n - 1 {
        let deltax = pts[i + 1].0 - pts[i].0;
        let deltay = pts[i + 1].1 - pts[i].1;
        dx.push(deltax);
        dy.push(deltay);
        m.push(if deltax.abs() < 1e-5 { 0. } else { deltay / deltax });
    }

    let mut d = Vec::with_capacity(n);
    d.push(m[0]);
    for i in 1..n - 1 {
        if m[i - 1] * m[i] <= 0. {
            d.push(0.);
        } else {
            d.push((m[i - 1] + m[i]) / 2.);
        }
    }
    d.push(*m.last().unwrap());

    for i in 0..n - 1 {
        if dy[i].abs() < 1e-5 {
            d[i] = 0.;
            d[i + 1] = 0.;
        } else {
            let a = d[i] / m[i];
            let b = d[i + 1] / m[i];
            let dist = (a * a + b * b).sqrt();
            if dist > 3.0 {
                let tau = 3.0 / dist;
                d[i] = tau * a * m[i];
                d[i + 1] = tau * b * m[i];
            }
        }
    }

    let mut result = Vec::new();
    const STEPS: usize = 10;
    for i in 0..n - 1 {
        let (x0, y0) = pts[i];
        let (_x1, y1) = pts[i + 1];
        let h = dx[i];
        let d0 = d[i];
        let d1 = d[i + 1];
        let num_steps = if i == n - 2 { STEPS } else { STEPS - 1 };

        for step in 0..=num_steps {
            let t = step as f32 / STEPS as f32;
            let t2 = t * t;
            let t3 = t2 * t;

            let h00 = 2. * t3 - 3. * t2 + 1.;
            let h10 = t3 - 2. * t2 + t;
            let h01 = -2. * t3 + 3. * t2;
            let h11 = t3 - t2;

            let x = x0 + t * h;
            let y = h00 * y0 + h10 * h * d0 + h01 * y1 + h11 * h * d1;
            result.push((x, y));
        }
    }
    result
}

/// 1, 2 or 5 times a power of ten, at or above `raw`.
fn nice_step(raw: f64) -> f64 {
    let pow = 10f64.powf(raw.log10().floor());
    [1., 2., 5., 10.]
        .into_iter()
        .map(|m| m * pow)
        .find(|&s| s >= raw)
        .unwrap_or(10. * pow)
}

/// Plot box inside a chart canvas: left, top, width, height, in px.
/// Designed for ample breathing room around tick labels and category names.
pub fn plot_box(width: f32, height: f32) -> (f32, f32, f32, f32) {
    const LEFT: f32 = 48.;
    const RIGHT: f32 = 16.;
    const TOP: f32 = 12.;
    const BOTTOM: f32 = 32.;
    (
        LEFT,
        TOP,
        (width - LEFT - RIGHT).max(20.),
        (height - TOP - BOTTOM).max(20.),
    )
}

/// The category under a pointer in the plot, if any.
pub fn category_at(chart: &Chart, plot_w: f32, x: f32) -> Option<usize> {
    let n = chart.labels.len();
    if n == 0 || x < 0. || x >= plot_w {
        return None;
    }
    Some(((x / plot_w) * n as f32) as usize).filter(|&ix| ix < n)
}

/// What a hover shows: category name and each series' exact value.
pub fn hover_rows(chart: &Chart, ix: usize) -> Option<(String, Vec<(usize, String, String)>)> {
    let name = chart.labels.get(ix)?.clone();
    let rows: Vec<(usize, String, String)> = chart
        .series
        .iter()
        .enumerate()
        .filter_map(|(si, series)| {
            Some((
                si,
                series.name.clone(),
                chart.format_value(series.values.get(ix).copied().flatten()?),
            ))
        })
        .collect();
    (!rows.is_empty()).then_some((name, rows))
}

/// A value as stated, for the hover card: `482,345`, `41.2`, `-1,234.5`.
pub fn exact(v: f64) -> String {
    let text = format!("{:.2}", v.abs());
    let text = text.trim_end_matches('0').trim_end_matches('.');
    let (whole, frac) = text
        .split_once('.')
        .map_or((text, None), |(w, f)| (w, Some(f)));
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
    let trim = |s: String| {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s
        }
    };
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
    fn chart_to_csv_produces_valid_csv() {
        let chart = Chart::from_spec(&spec()).unwrap();
        let csv = chart.to_csv();
        assert_eq!(csv, "crate,before\nui,41.2\nengine,33.5\n");
    }

    #[test]
    fn fallback_title_sentence_case() {
        let spec = json!({
            "component": "chart",
            "kind": "bar",
            "x": "Month",
            "labels": ["Jan", "Feb"],
            "series": [
                { "name": "Revenue ($k)", "values": [42.0, 55.0] },
                { "name": "Costs ($k)", "values": [30.0, 32.0] }
            ]
        });
        let chart = Chart::from_spec(&spec).unwrap();
        assert_eq!(chart.fallback_title(), "Revenue and costs by month");
        assert_eq!(chart.display_title(), "Revenue and costs by month");
    }

    #[test]
    fn explicit_title_is_used() {
        let spec = json!({
            "component": "chart",
            "kind": "bar",
            "title": "Quarterly earnings report",
            "x": "Quarter",
            "labels": ["Q1", "Q2"],
            "series": [{ "name": "Profit", "values": [10.0, 20.0] }]
        });
        let chart = Chart::from_spec(&spec).unwrap();
        assert_eq!(chart.display_title(), "Quarterly earnings report");
    }

    #[test]
    fn effective_unit_extracted_and_formatted() {
        let spec = json!({
            "component": "chart",
            "kind": "bar",
            "x": "Month",
            "labels": ["Jan", "Feb"],
            "series": [{ "name": "Revenue ($k)", "values": [42.0, 55.0] }]
        });
        let chart = Chart::from_spec(&spec).unwrap();
        assert_eq!(chart.effective_unit(), Some("$k".to_string()));
        assert_eq!(chart.format_tick(40.0), "$40k");
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
        assert!(!chart.is_of(
            &["crate", "before"],
            &[vec!["engine", "1"], vec!["ui", "2"]]
        ));
    }

    #[test]
    fn a_hover_names_the_category_and_its_values() {
        let chart = Chart::from_spec(&spec()).unwrap();
        assert_eq!(category_at(&chart, 200., 10.), Some(0));
        assert_eq!(category_at(&chart, 200., 150.), Some(1));
        assert_eq!(category_at(&chart, 200., 200.), None);
        assert_eq!(
            hover_rows(&chart, 0).unwrap(),
            (
                "ui".to_string(),
                vec![(0, "before".to_string(), "41.2".to_string())]
            )
        );
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

    #[test]
    fn monotone_spline_preserves_monotonicity() {
        let pts = vec![(0., 0.), (10., 50.), (20., 50.), (30., 100.)];
        let spline = monotone_spline(&pts);
        assert!(spline.len() > pts.len());
        // Verify monotonically non-decreasing
        for i in 1..spline.len() {
            assert!(spline[i].1 >= spline[i - 1].1 - 1e-4);
        }
    }
}

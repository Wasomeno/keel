//! Box drawings in a `diagram` fence (`┌─┐│└┘├┤▶▼`, or `+-|`) become boxes
//! and arrows; otherwise the fence goes to mermaid (`crate::mermaid`) or stays
//! a code block. Layout is a rank assignment plus a left-to-right sweep.

use std::collections::HashMap;

/// A node the painter can place. `x`/`y` are rank and order, not pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub id: String,
    pub label: String,
    pub x: usize,
    pub y: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Right,
    Down,
    Left,
    Up,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub label: String,
    pub dir: Dir,
}

/// A diagram that fits the painter. `down` is top-to-bottom flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagram {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub down: bool,
}

const MAX_NODES: usize = 64;
const MAX_EDGES: usize = 96;

/// Parse a `diagram` fence drawn with box characters. `None` means "not a
/// box drawing": the fence goes to mermaid or stays a code block.
pub fn parse(language: Option<&str>, source: &str) -> Option<Diagram> {
    if language?.trim() != "diagram" {
        return None;
    }
    let diagram = parse_boxes(source)?;
    (diagram.nodes.len() <= MAX_NODES && diagram.edges.len() <= MAX_EDGES && faithful(&diagram, source)).then_some(diagram)
}

/// Characters of a label the painter draws; longer labels are cut.
pub const LABEL_CHARS: usize = 24;

/// The painter shows one line per box and per arrow. A drawing whose text
/// mostly doesn't survive that (multi-line boxes, notes beside arrows, loose
/// branches, a box with a ragged edge) reads better as the drawing itself.
fn faithful(diagram: &Diagram, source: &str) -> bool {
    // A lone `v` is an arrowhead, not a word.
    let words = |s: &str| s.split_whitespace().filter(|w| *w != "v").map(|w| w.chars().filter(|c| c.is_alphanumeric()).count()).sum::<usize>();
    let drawn: usize = diagram
        .nodes
        .iter()
        .map(|n| n.label.as_str())
        .chain(diagram.edges.iter().map(|e| e.label.as_str()))
        .map(|label| words(&label.chars().take(LABEL_CHARS).collect::<String>()))
        .sum();
    drawn * 10 >= words(source) * 9
}

// --- box drawings -----------------------------------------------------------

fn is_box_char(c: char) -> bool {
    matches!(
        c,
        '┌' | '┐' | '└' | '┘' | '╭' | '╮' | '╰' | '╯' | '├' | '┤' | '┬' | '┴' | '┼' | '─' | '│' | '━' | '┃'
            | '+' | '-' | '|'
    )
}

fn is_horiz(c: char) -> bool {
    matches!(c, '─' | '━' | '-' | '┌' | '┐' | '└' | '┘' | '╭' | '╮' | '╰' | '╯' | '├' | '┤' | '┬' | '┴' | '┼' | '+')
}

fn is_vert(c: char) -> bool {
    matches!(c, '│' | '┃' | '|' | '┌' | '┐' | '└' | '┘' | '╭' | '╮' | '╰' | '╯' | '├' | '┤' | '┬' | '┴' | '┼' | '+')
}

struct Grid {
    rows: Vec<Vec<char>>,
    width: usize,
}

impl Grid {
    fn get(&self, x: usize, y: usize) -> char {
        self.rows.get(y).and_then(|r| r.get(x)).copied().unwrap_or(' ')
    }
}

fn grid_of(source: &str) -> Grid {
    let rows: Vec<Vec<char>> = source.lines().map(|l| l.chars().collect()).collect();
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    Grid { rows, width }
}

/// A rectangle of box-drawing corners with a label inside.
fn parse_boxes(source: &str) -> Option<Diagram> {
    let grid = grid_of(source);
    if grid.rows.len() < 2 || grid.width < 3 {
        return None;
    }
    let mut boxes = Vec::new();
    for y in 0..grid.rows.len() {
        let mut x = 0;
        while x < grid.width {
            let c = grid.get(x, y);
            if !matches!(c, '┌' | '╭' | '+') {
                x += 1;
                continue;
            }
            let mut x2 = x + 1;
            while x2 < grid.width && is_horiz(grid.get(x2, y)) && !matches!(grid.get(x2, y), '┐' | '╮' | '+') {
                x2 += 1;
            }
            if x2 == x + 1 || !matches!(grid.get(x2, y), '┐' | '╮' | '+') {
                x += 1;
                continue;
            }
            let mut y2 = y + 1;
            while y2 < grid.rows.len() && is_vert(grid.get(x, y2)) && !matches!(grid.get(x, y2), '└' | '╰' | '+') {
                y2 += 1;
            }
            let closed = y2 > y + 1
                && matches!(grid.get(x, y2), '└' | '╰' | '+')
                && matches!(grid.get(x2, y2), '┘' | '╯' | '+')
                && (x + 1..x2).all(|i| is_horiz(grid.get(i, y2)))
                && (y + 1..y2).all(|j| is_vert(grid.get(x2, j)));
            if closed {
                let label = box_label(&grid, x, y, x2, y2);
                if !label.is_empty() {
                    boxes.push((x, y, x2, y2, label));
                }
                x = x2 + 1;
            } else {
                x += 1;
            }
        }
    }
    if boxes.is_empty() {
        return None;
    }

    let mut nodes = Vec::new();
    let mut id_at: HashMap<(usize, usize), String> = HashMap::new();
    for (i, (x, y, x2, y2, label)) in boxes.iter().enumerate() {
        let id = format!("n{i}");
        id_at.insert((*x, *y), id.clone());
        nodes.push(Node { id, label: label.clone(), x: *x, y: *y });
        let _ = (x2, y2);
    }

    let mut edges = Vec::new();
    for (x, y, x2, y2, _) in &boxes {
        let id = id_at[&(*x, *y)].clone();
        // Right and left: scan the mid-row for an arrow that lands on another box.
        let mid_y = y + (y2 - y) / 2;
        if let Some((other, dir, label)) = h_link(&grid, &boxes, *x2, mid_y, true) {
            edges.push(Edge { from: id.clone(), to: id_at[&other].clone(), label, dir });
        }
        if let Some((other, dir, label)) = h_link(&grid, &boxes, *x, mid_y, false)
            && matches!(dir, Dir::Left)
        {
            edges.push(Edge { from: id.clone(), to: id_at[&other].clone(), label, dir });
        }
        let mid_x = x + (x2 - x) / 2;
        if let Some((other, dir, label)) = v_link(&grid, &boxes, mid_x, *y2, true) {
            edges.push(Edge { from: id.clone(), to: id_at[&other].clone(), label, dir });
        }
        if let Some((other, dir, label)) = v_link(&grid, &boxes, mid_x, *y, false)
            && matches!(dir, Dir::Up)
        {
            edges.push(Edge { from: id, to: id_at[&other].clone(), label, dir });
        }
    }
    // A box pointed at from both ends of one connector shows up twice. Keep one.
    edges.sort_by(|a, b| (&a.from, &a.to).cmp(&(&b.from, &b.to)));
    edges.dedup_by(|a, b| a.from == b.from && a.to == b.to);

    Some(layout(nodes, edges))
}

fn box_label(grid: &Grid, x: usize, y: usize, x2: usize, y2: usize) -> String {
    let mut best = String::new();
    for row in y + 1..y2 {
        let text: String = (x + 1..x2).map(|i| grid.get(i, row)).collect();
        let text = text.trim().to_string();
        if text.chars().any(|c| !is_box_char(c) && c != ' ') && text.chars().count() > best.chars().count() {
            best = text;
        }
    }
    best
}

fn h_link(
    grid: &Grid,
    boxes: &[(usize, usize, usize, usize, String)],
    mut x: usize,
    y: usize,
    right: bool,
) -> Option<((usize, usize), Dir, String)> {
    let step: isize = if right { 1 } else { -1 };
    let mut saw = false;
    let mut label = String::new();
    let mut dir = if right { Dir::Right } else { Dir::Left };
    loop {
        x = x.checked_add_signed(step)?;
        if x >= grid.width {
            return None;
        }
        let c = grid.get(x, y);
        if matches!(c, '─' | '━' | '-' | '►' | '▶' | '→' | '>' | '◄' | '◀' | '←' | '<') {
            saw = true;
            dir = match c {
                '►' | '▶' | '→' | '>' => Dir::Right,
                '◄' | '◀' | '←' | '<' => Dir::Left,
                _ => dir,
            };
            continue;
        }
        if c == ' ' {
            // A label sits on the row above the connector.
            let above = grid.get(x, y.saturating_sub(1));
            if y > 0 && !above.is_whitespace() && !is_box_char(above) {
                label.push(above);
            }
            if saw {
                continue;
            }
            return None;
        }
        if let Some((bx, by, ..)) = boxes.iter().find(|(bx, by, bx2, by2, _)| {
            x == if right { *bx } else { *bx2 } && y > *by && y < *by2
        }) {
            return saw.then_some(((*bx, *by), dir, label.trim().to_string()));
        }
        return None;
    }
}

fn v_link(
    grid: &Grid,
    boxes: &[(usize, usize, usize, usize, String)],
    x: usize,
    mut y: usize,
    down: bool,
) -> Option<((usize, usize), Dir, String)> {
    let step: isize = if down { 1 } else { -1 };
    let mut saw = false;
    let mut dir = if down { Dir::Down } else { Dir::Up };
    loop {
        y = y.checked_add_signed(step)?;
        if y >= grid.rows.len() {
            return None;
        }
        let c = grid.get(x, y);
        if matches!(c, '│' | '┃' | '|' | '▼' | '↓' | 'v' | '▲' | '↑') {
            saw = true;
            dir = match c {
                '▼' | '↓' | 'v' => Dir::Down,
                '▲' | '↑' => Dir::Up,
                _ => dir,
            };
            continue;
        }
        if c == ' ' && saw {
            continue;
        }
        if let Some((bx, by, ..)) = boxes.iter().find(|(bx, by, bx2, by2, _)| {
            y == if down { *by } else { *by2 } && x > *bx && x < *bx2
        }) {
            return saw.then_some(((*bx, *by), dir, String::new()));
        }
        return None;
    }
}

// --- layout -----------------------------------------------------------------

/// Longest-path ranks, then a sweep that keeps edge sources left of targets
/// when the graph is a DAG. Cycles keep their first-seen rank.
fn layout(nodes: Vec<Node>, edges: Vec<Edge>) -> Diagram {
    let mut rank: HashMap<String, usize> = HashMap::new();
    let mut order: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
    // Stable: declaration order, then edge order.
    for n in &nodes {
        rank.entry(n.id.clone()).or_insert(0);
    }
    for _ in 0..nodes.len() {
        let mut changed = false;
        for e in &edges {
            let from = rank.get(&e.from).copied().unwrap_or(0);
            let to = rank.get(&e.to).copied().unwrap_or(0);
            if to < from + 1 {
                rank.insert(e.to.clone(), from + 1);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    // Cap runaway cycles.
    let max = nodes.len().saturating_sub(1);
    for r in rank.values_mut() {
        *r = (*r).min(max);
    }
    order.sort_by(|a, b| rank[a].cmp(&rank[b]).then(a.cmp(b)));
    let mut slot: HashMap<usize, usize> = HashMap::new();
    let mut placed = Vec::new();
    for id in &order {
        let r = rank[id];
        let y = slot.entry(r).or_insert(0);
        let node = nodes.iter().find(|n| n.id == *id).unwrap();
        placed.push(Node { id: id.clone(), label: node.label.clone(), x: r, y: *y });
        *y += 1;
    }
    let down = edges.iter().filter(|e| matches!(e.dir, Dir::Down | Dir::Up)).count()
        > edges.iter().filter(|e| matches!(e.dir, Dir::Right | Dir::Left)).count();
    Diagram { nodes: placed, edges, down }
}

/// Pixel size of a rendered diagram, for the row-height memo.
/// `width` is the column the diagram has to fit. `0` means "as wide as the graph".
pub fn size(diagram: &Diagram, width: f32) -> (f32, f32) {
    let placed = place(diagram, width);
    let right = placed.nodes.values().map(|b| b.0 + b.2).fold(0_f32, f32::max);
    let bottom = placed.nodes.values().map(|b| b.1 + b.3).fold(0_f32, f32::max);
    ((right + 16.).max(120.), bottom + 16.)
}

const CHAR_W: f32 = 7.2;
const PAD_X: f32 = 14.;
const BOX_MIN_W: f32 = 72.;
const BOX_MAX_W: f32 = 180.;
const BOX_H: f32 = 36.;
const GAP_X: f32 = 56.;
const GAP_Y: f32 = 28.;

/// Where each node sits, in pixels relative to the canvas origin.
pub struct Placed {
    pub nodes: HashMap<String, (f32, f32, f32, f32)>,
}

/// Place nodes inside `width`. A left-to-right graph that does not fit is
/// drawn top to bottom instead of running off the right edge.
pub fn place(diagram: &Diagram, width: f32) -> Placed {
    let mut nodes = HashMap::new();
    let down = flows_down(diagram, width);
    let ranks = diagram.nodes.iter().map(|n| n.x).max().unwrap_or(0);
    let cross = if width > 32. { width - 32. } else { 480. };
    if down {
        let mut top = 12.;
        let _ = cross;
        for r in 0..=ranks {
            let rank: Vec<_> = diagram.nodes.iter().filter(|n| n.x == r).collect();
            let mut x = 16.;
            let mut row_h = BOX_H;
            for n in rank {
                let w = label_w(&n.label);
                if x > 16. && x + w > cross + 16. {
                    x = 16.;
                    top += BOX_H + 8.;
                }
                nodes.insert(n.id.clone(), (x, top, w, BOX_H));
                x += w + 16.;
                row_h = BOX_H;
            }
            top += row_h + GAP_Y;
        }
        return Placed { nodes };
    }
    let mut extent: HashMap<usize, f32> = HashMap::new();
    for n in &diagram.nodes {
        let w = label_w(&n.label);
        extent.entry(n.x).and_modify(|c| *c = c.max(w)).or_insert(w);
    }
    let mut origin: HashMap<usize, f32> = HashMap::new();
    let mut cursor = 16.;
    for r in 0..=ranks {
        origin.insert(r, cursor);
        cursor += extent.get(&r).copied().unwrap_or(BOX_MIN_W) + GAP_X;
    }
    let mut slot: HashMap<usize, f32> = HashMap::new();
    for n in &diagram.nodes {
        let w = label_w(&n.label);
        let y = slot.entry(n.x).or_insert(12.);
        nodes.insert(n.id.clone(), (origin[&n.x], *y, w, BOX_H));
        *y += BOX_H + GAP_Y;
    }
    Placed { nodes }
}

fn flows_down(diagram: &Diagram, width: f32) -> bool {
    if diagram.down {
        return true;
    }
    if width <= 32. {
        return false;
    }
    size_wide(diagram).0 > width
}

fn size_wide(diagram: &Diagram) -> (f32, f32) {
    let mut col_w: HashMap<usize, f32> = HashMap::new();
    for n in &diagram.nodes {
        let w = label_w(&n.label);
        col_w.entry(n.x).and_modify(|c| *c = c.max(w)).or_insert(w);
    }
    let ranks = diagram.nodes.iter().map(|n| n.x).max().unwrap_or(0) + 1;
    let width: f32 = (0..ranks).map(|r| col_w.get(&r).copied().unwrap_or(BOX_MIN_W) + GAP_X).sum::<f32>() + 16.;
    (width, 0.)
}

fn label_w(s: &str) -> f32 {
    (s.chars().count() as f32 * CHAR_W + PAD_X * 2.).clamp(BOX_MIN_W, BOX_MAX_W)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_row_connects_left_to_right() {
        let src = "\
┌────────┐     ┌─────┐
│ Client │────▶│ API │
└────────┘     └─────┘";
        let d = parse(Some("diagram"), src).unwrap();
        assert_eq!(d.nodes.len(), 2);
        assert_eq!(d.nodes[0].label, "Client");
        assert_eq!(d.nodes[1].label, "API");
        assert_eq!(d.edges.len(), 1);
        assert_eq!(d.edges[0].dir, Dir::Right);
        assert!(d.nodes[0].x < d.nodes[1].x);
    }

    #[test]
    fn box_down_and_label() {
        let src = "\
┌───┐
│ A │
└─┬─┘
  │
  ▼
┌───┐
│ B │
└───┘";
        let d = parse(Some("diagram"), src).unwrap();
        assert_eq!(d.edges.len(), 1);
        assert_eq!(d.edges[0].dir, Dir::Down);
        assert!(d.down);
    }

    #[test]
    fn junk_and_other_languages_stay_code() {
        assert!(parse(Some("rust"), "fn main() {}").is_none());
        assert!(parse(Some("mermaid"), "graph TD\n  A --> B").is_none(), "mermaid is merman's");
        assert!(parse(Some("diagram"), "just words").is_none());
    }

    #[test]
    fn ascii_plus_boxes_and_bare_arrows() {
        let src = "+---+\n| A |\n+-+-+\n  |\n  v\n+---+\n| B |\n+---+";
        let d = parse(Some("diagram"), src).unwrap();
        assert_eq!(d.nodes.iter().map(|n| n.label.as_str()).collect::<Vec<_>>(), vec!["A", "B"]);
        assert_eq!(d.edges.len(), 1);
        assert!(d.down);
    }

    #[test]
    fn a_drawing_the_painter_would_gut_stays_text() {
        // Two-line boxes and notes beside the arrows: most words would be lost.
        let src = "┌─────────────────────┐\n│  Protected route    │\n│  (no session cookie)│\n└──────────┬──────────┘\n           │ middleware\n           ▼\n┌─────────────────────┐\n│   /auth/login       │\n│  \"Masuk Identik\"    │\n└─────────────────────┘";
        assert!(parse(Some("diagram"), src).is_none());
    }
}

//! The GitLab page: projects the user can reach on a connected host, and the
//! selected project's issues. Read-only; an issue opens in the browser.

use gpui::{AnyElement, ClickEvent, Context, ElementId, IntoElement, div, prelude::*};
use keel_theme::Tokens;
use crate::style::zpx;

use super::KeelWindow;
use super::sidebar::text_button;
use crate::icons::Icon;
use crate::style::{KeelStyled, MONO, RADIUS_LG, RADIUS_MD, c};

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Project {
    pub id: u64,
    pub name: String,
    pub path: String,
    pub web_url: String,
    pub last_activity_at: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Issue {
    pub iid: u64,
    pub title: String,
    pub state: String,
    pub labels: Vec<String>,
    pub author: String,
    pub comments: u64,
    pub updated_at: String,
    pub web_url: String,
}

/// A board column: open issues with no list label, one per label list,
/// or closed.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Column {
    pub title: String,
    /// `0xRRGGBBAA` from the label, for list columns.
    pub color: Option<u32>,
    pub label: Option<String>,
    pub issues: Vec<Issue>,
}

/// Everything the page shows. `None` = not loaded yet (skeletons).
#[derive(Default)]
pub(super) struct GitlabPage {
    pub host: Option<String>,
    pub projects: Option<Result<Vec<Project>, String>>,
    pub project: Option<Project>,
    pub board: Option<Result<Vec<Column>, String>>,
    /// Bumped per load: names the newest request (a slow older one can't
    /// land late) and the entrance animations (they play once per load).
    pub projects_gen: u64,
    pub board_gen: u64,
}

/// Lay a project's issues out like GitLab's default board: Open (issues in
/// no list), one column per label list in board order (an issue with two
/// list labels shows in both, as on GitLab), then Closed.
pub(super) fn board_columns(lists: &[(String, Option<u32>)], opened: Vec<Issue>, closed: Vec<Issue>) -> Vec<Column> {
    let in_a_list = |issue: &Issue| lists.iter().any(|(label, _)| issue.labels.contains(label));
    let mut columns = vec![Column { title: "Open".into(), color: None, label: None, issues: opened.iter().filter(|i| !in_a_list(i)).cloned().collect() }];
    for (label, color) in lists {
        columns.push(Column {
            title: label.clone(),
            color: *color,
            label: Some(label.clone()),
            issues: opened.iter().filter(|i| i.labels.contains(label)).cloned().collect(),
        });
    }
    columns.push(Column { title: "Closed".into(), color: None, label: None, issues: closed });
    columns
}

/// The repo menu's rows: see [`super::composer_view::filter_rows`]; a repo
/// matches on its name or path.
pub(super) fn filter_projects<'a>(projects: &'a [(String, Project)], query: &str, limit: usize) -> (Vec<&'a (String, Project)>, usize) {
    super::composer_view::filter_rows(projects, query, limit, |(_, p)| [p.name.as_str(), p.path.as_str()])
}

/// `#428BCA` → `0x428BCAFF`.
fn hex_color(hex: &str) -> Option<u32> {
    let hex = hex.strip_prefix('#')?;
    (hex.len() == 6).then(|| u32::from_str_radix(hex, 16).ok().map(|rgb| rgb << 8 | 0xFF)).flatten()
}

fn s(v: &serde_json::Value, key: &str) -> String {
    v[key].as_str().unwrap_or_default().to_string()
}

impl Project {
    fn parse(v: &serde_json::Value) -> Self {
        Self { id: v["id"].as_u64().unwrap_or(0), name: s(v, "name"), path: s(v, "path"), web_url: s(v, "web_url"), last_activity_at: s(v, "last_activity_at") }
    }
}

impl Issue {
    fn parse(v: &serde_json::Value) -> Self {
        Self {
            iid: v["iid"].as_u64().unwrap_or(0),
            title: s(v, "title"),
            state: s(v, "state"),
            labels: v["labels"].as_array().map(|l| l.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default(),
            author: s(v, "author"),
            comments: v["comments"].as_u64().unwrap_or(0),
            updated_at: s(v, "updated_at"),
            web_url: s(v, "web_url"),
        }
    }
}

impl KeelWindow {
    /// Sidebar → GitLab. Picks the first connected host and loads its projects.
    pub(super) fn open_gitlab(&mut self, cx: &mut Context<Self>) {
        self.begin_settings_exit();
        self.finish_settings_exit(cx);
        self.gitlab_open = true;
        let Some(client) = self.client.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let v = client.call("list_connections", serde_json::json!({})).await.unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                this.connections =
                    v["connections"].as_array().map(|r| r.iter().map(super::settings::ConnectionRow::parse).collect()).unwrap_or_default();
                let hosts: Vec<String> = this.connections.iter().filter(|c| c.kind == "gitlab").map(|c| c.host.clone()).collect();
                let keep = this.gitlab.host.clone().filter(|h| hosts.contains(h));
                match keep.or_else(|| hosts.first().cloned()) {
                    Some(host) => {
                        if this.gitlab.host.as_ref() != Some(&host) || this.gitlab.projects.is_none() {
                            this.gitlab_select_host(host, cx);
                        }
                    }
                    None => this.gitlab = GitlabPage::default(),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn gitlab_select_host(&mut self, host: String, cx: &mut Context<Self>) {
        let projects_gen = self.gitlab.projects_gen + 1;
        self.gitlab = GitlabPage { host: Some(host.clone()), projects_gen, board_gen: self.gitlab.board_gen, ..Default::default() };
        let Some(client) = self.client.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let res = client.call("gitlab_projects", serde_json::json!({ "host": host })).await;
            let _ = this.update(cx, |this, cx| {
                if this.gitlab.host.as_ref() != Some(&host) || this.gitlab.projects_gen != projects_gen {
                    return;
                }
                let projects: Result<Vec<Project>, String> =
                    res.map(|v| v["projects"].as_array().map(|r| r.iter().map(Project::parse).collect()).unwrap_or_default());
                // Start on the most recently active project.
                let first = projects.as_ref().ok().and_then(|p| p.first().cloned());
                this.gitlab.projects = Some(projects);
                if let Some(first) = first {
                    this.gitlab_select_project(first, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn gitlab_select_project(&mut self, project: Project, cx: &mut Context<Self>) {
        self.gitlab.project = Some(project);
        self.gitlab_load_board(cx);
    }

    fn gitlab_load_board(&mut self, cx: &mut Context<Self>) {
        let (Some(host), Some(project), Some(client)) = (self.gitlab.host.clone(), self.gitlab.project.clone(), self.client.clone()) else {
            return;
        };
        self.gitlab.board = None;
        self.gitlab.board_gen += 1;
        let generation = self.gitlab.board_gen;
        cx.spawn(async move |this, cx| {
            let res = client.call("gitlab_board", serde_json::json!({ "host": host, "project_id": project.id })).await;
            let _ = this.update(cx, |this, cx| {
                if this.gitlab.board_gen != generation {
                    return;
                }
                this.gitlab.board = Some(res.map(|v| {
                    let issues = |key: &str| v[key].as_array().map(|r| r.iter().map(Issue::parse).collect()).unwrap_or_default();
                    let lists: Vec<(String, Option<u32>)> = v["lists"]
                        .as_array()
                        .map(|r| r.iter().map(|l| (s(l, "label"), hex_color(&s(l, "color")))).collect())
                        .unwrap_or_default();
                    board_columns(&lists, issues("opened"), issues("closed"))
                }));
                cx.notify();
            });
        })
        .detach();
    }

    /// The composer's repo menu: projects across every connected GitLab host,
    /// fetched once when the menu first opens.
    pub(super) fn load_issue_repos(&mut self, cx: &mut Context<Self>) {
        if self.repo_projects.is_some() || self.repo_projects_loading {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        self.repo_projects_loading = true;
        cx.spawn(async move |this, cx| {
            let result = async {
                let v = client.call("list_connections", serde_json::json!({})).await?;
                let rows: Vec<super::settings::ConnectionRow> =
                    v["connections"].as_array().map(|r| r.iter().map(super::settings::ConnectionRow::parse).collect()).unwrap_or_default();
                let mut projects = Vec::new();
                let mut failed = None;
                for host in rows.iter().filter(|c| c.kind == "gitlab").map(|c| c.host.clone()) {
                    match client.call("gitlab_projects", serde_json::json!({ "host": host })).await {
                        Ok(v) => projects.extend(
                            v["projects"].as_array().into_iter().flatten().map(|p| (host.clone(), Project::parse(p))),
                        ),
                        Err(err) => failed = Some(err),
                    }
                }
                // One host down shouldn't hide the others' projects.
                match failed {
                    Some(err) if projects.is_empty() => Err(err),
                    _ => Ok(projects),
                }
            }
            .await;
            let _ = this.update(cx, |this, cx| {
                this.repo_projects_loading = false;
                this.repo_projects = Some(result);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Pick (or clear) the repo whose open issues the `@` menu offers, and
    /// fetch them now so the menu has them by the time `@` is typed.
    pub(super) fn select_issue_repo(&mut self, repo: Option<(String, Project)>, cx: &mut Context<Self>) {
        self.issue_repo = repo.clone();
        self.repo_issues = None;
        let (Some((host, project)), Some(client)) = (repo, self.client.clone()) else {
            cx.notify();
            return;
        };
        cx.spawn(async move |this, cx| {
            let res = client.call("gitlab_board", serde_json::json!({ "host": host, "project_id": project.id })).await;
            let _ = this.update(cx, |this, cx| {
                // A newer pick wins over this one landing late.
                if this.issue_repo.as_ref().map(|(h, p)| (h, p.id)) != Some((&host, project.id)) {
                    return;
                }
                let issues = res
                    .map(|v| v["opened"].as_array().map(|r| r.iter().map(Issue::parse).collect()).unwrap_or_default())
                    .unwrap_or_else(|err| {
                        this.report("gitlab_board failed", err);
                        Vec::new()
                    });
                this.repo_issues = Some(issues);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn render_gitlab(&mut self, t: Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let reduced = self.reduced();
        let hosts: Vec<String> = self.connections.iter().filter(|c| c.kind == "gitlab").map(|c| c.host.clone()).collect();
        let host = self.gitlab.host.clone();
        let body: AnyElement = if hosts.is_empty() {
            empty_state(t, cx).into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_row()
                .gap_4()
                .child(self.render_projects(t, cx))
                .child(self.render_board(t, cx))
                .into_any_element()
        };
        crate::motion::appear(
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .px_6()
                .pt_2()
                .pb_4()
                .flex()
                .flex_col()
                .gap_4()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(div().text_color(c(t.text_muted)).child(Icon::GitLab.el(zpx(18.))))
                        .child(div().t_title().child("GitLab"))
                        .children(hosts.into_iter().map(|h| {
                            let active = host.as_ref() == Some(&h);
                            let pick = h.clone();
                            div()
                                .id(ElementId::Name(format!("gl-host-{h}").into()))
                                .px_2()
                                .py_0p5()
                                .rounded(RADIUS_MD)
                                .t_small()
                                .cursor_pointer()
                                .text_color(c(if active { t.text } else { t.text_muted }))
                                .when(active, |d| d.bg(c(t.selected)))
                                .hover(|d| d.bg(c(t.hover)))
                                .child(h.trim_start_matches("https://").to_string())
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.gitlab_select_host(pick.clone(), cx);
                                    cx.notify();
                                }))
                        }))
                        .child(div().flex_1())
                        .when(host.is_some(), |d| {
                            d.child(text_button("gl-refresh-btn", "Refresh", t).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.gitlab_load_board(cx);
                                cx.notify();
                            })))
                        }),
                )
                .child(body),
            "gitlab-appear",
            crate::motion::BASE,
            6.,
            reduced,
        )
    }

    fn render_projects(&mut self, t: Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let reduced = self.reduced();
        let filter = self.gitlab_filter.read(cx).value().to_lowercase();
        let selected = self.gitlab.project.as_ref().map(|p| p.id);
        let generation = self.gitlab.projects_gen;
        let list: AnyElement = match &self.gitlab.projects {
            None => div()
                .flex()
                .flex_col()
                .gap_1()
                .children((0..8).map(|ix| project_skeleton(ix, t, reduced)))
                .into_any_element(),
            Some(Err(err)) => error(t, err).into_any_element(),
            Some(Ok(projects)) => {
                let shown: Vec<Project> = projects.iter().filter(|p| filter.is_empty() || p.path.to_lowercase().contains(&filter)).cloned().collect();
                if shown.is_empty() {
                    note(t, if projects.is_empty() { "No projects you're a member of." } else { "No match." }).into_any_element()
                } else {
                    div()
                        .flex()
                        .flex_col()
                        .gap_px()
                        .children(shown.into_iter().enumerate().map(|(ix, p)| {
                            let active = selected == Some(p.id);
                            let namespace = p.path.rsplit_once('/').map(|(ns, _)| ns.to_string()).unwrap_or_default();
                            let pick = p.clone();
                            let row = div()
                                .id(ElementId::Name(format!("gl-project-{}", p.id).into()))
                                .px_2()
                                .py_1p5()
                                .rounded(RADIUS_MD)
                                .cursor_pointer()
                                .flex()
                                .flex_col()
                                .when(active, |d| d.bg(c(t.selected)))
                                .when(!active, |d| d.hover(|s| s.bg(c(t.hover))))
                                .child(
                                    div()
                                        .t_body()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .text_color(c(if active { t.text } else { t.text_muted }))
                                        .when(active, |d| d.font_weight(gpui::FontWeight::MEDIUM))
                                        .child(p.name.clone()),
                                )
                                .child(
                                    div()
                                        .t_caption()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .text_color(c(t.text_faint))
                                        .child(match ago(&p.last_activity_at) {
                                            Some(age) if !namespace.is_empty() => format!("{namespace} · {age}"),
                                            Some(age) => age,
                                            None => namespace,
                                        }),
                                )
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.gitlab_select_project(pick.clone(), cx);
                                    cx.notify();
                                }));
                            crate::motion::stagger_in(row, ElementId::Name(format!("gl-project-in-{generation}-{}", p.id).into()), ix, reduced)
                        }))
                        .into_any_element()
                }
            }
        };
        div()
            .w(zpx(260.))
            .flex_none()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .h(zpx(30.))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded(RADIUS_MD)
                    .bg(c(t.input))
                    .border_1()
                    .border_color(c(t.border))
                    .t_body()
                    .child(self.gitlab_filter.clone()),
            )
            .child(div().id("gl-projects").flex_1().min_h_0().overflow_y_scroll().child(list))
    }

    /// The selected project's issues as a board, columns side by side.
    fn render_board(&mut self, t: Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let reduced = self.reduced();
        let Some(project) = self.gitlab.project.clone() else {
            let loading = self.gitlab.projects.is_none();
            return div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_row()
                .gap_3()
                .when(loading, |d| d.children((0..3).map(|col| column_skeleton(col, t, reduced))))
                .when(!loading, |d| d.child(note(t, "Pick a project.")))
                .into_any_element();
        };
        let generation = self.gitlab.board_gen;
        // The columns go straight into the scrolling row: wrapped in another
        // div, that div is held to the row's width and nothing overflows.
        let columns: Vec<AnyElement> = match &self.gitlab.board {
            None => (0..3).map(|col| column_skeleton(col, t, reduced)).collect(),
            Some(Err(err)) => vec![error(t, err).into_any_element()],
            Some(Ok(columns)) => columns
                .iter()
                .enumerate()
                .map(|(ix, column)| {
                    crate::motion::stagger_in(
                        render_column(column, ix, generation, t, reduced),
                        ElementId::Name(format!("gl-col-in-{generation}-{ix}").into()),
                        ix,
                        reduced,
                    )
                })
                .collect(),
        };
        let web = project.web_url.clone();
        let _ = cx;
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .id("gl-project-title")
                    .flex_none()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .t_body()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .cursor_pointer()
                    .hover(|s| s.text_color(c(t.accent)))
                    .child(project.path)
                    .on_click(move |_, _, cx| cx.open_url(&web)),
            )
            .child(axis_locked(
                div()
                    .id("gl-board")
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .gap_3()
                    .pb_2()
                    .overflow_x_scroll()
                    .children(columns),
            ))
            .into_any_element()
    }
}

/// One board column: a header with the label color and count, then cards.
fn render_column(column: &Column, ix: usize, generation: u64, t: Tokens, reduced: bool) -> gpui::Div {
    let label = column.label.clone();
    div()
        .w(zpx(280.))
        .flex_none()
        .flex()
        .flex_col()
        .rounded(RADIUS_LG)
        .bg(c(t.surface))
        .border_1()
        .border_color(c(t.border))
        .child(
            div()
                .h(zpx(38.))
                .flex_none()
                .px_3()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .border_b_1()
                .border_color(c(t.border))
                .child(
                    div()
                        .size(zpx(9.))
                        .flex_none()
                        .rounded_full()
                        .bg(c(column.color.unwrap_or(if column.title == "Closed" { t.text_faint } else { t.success }))),
                )
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().t_body().font_weight(gpui::FontWeight::MEDIUM).child(column.title.clone()))
                .child(
                    div()
                        .px_1p5()
                        .rounded(RADIUS_MD)
                        .bg(c(t.hover))
                        .t_caption()
                        .font_family(MONO)
                        .text_color(c(t.text_muted))
                        .child(column.issues.len().to_string()),
                ),
        )
        .child(axis_locked(
            div()
                .id(ElementId::Name(format!("gl-col-{ix}").into()))
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .p_2()
                .flex()
                .flex_col()
                .gap_2()
                .when(column.issues.is_empty(), |d| d.child(div().py_4().t_small().text_center().text_color(c(t.text_faint)).child("Nothing here")))
                .children(column.issues.iter().enumerate().map(|(n, issue)| {
                    crate::motion::stagger_in(
                        issue_card(issue, label.as_deref(), t),
                        ElementId::Name(format!("gl-card-in-{generation}-{ix}-{}", issue.iid).into()),
                        ix + n,
                        reduced,
                    )
                })),
        ))
}

/// Scroll only along the axis this element overflows on. By default gpui
/// turns a sideways swipe into vertical scrolling for a vertical-only list
/// (and vice versa), so a swipe over a long column scrolled its cards
/// instead of the board, and scrolling the cards also slid the board.
fn axis_locked<E: Styled>(mut el: E) -> E {
    el.style().restrict_scroll_to_axis = Some(true);
    el
}

/// One issue on the board. The column's own label is left off its chips.
fn issue_card(issue: &Issue, column_label: Option<&str>, t: Tokens) -> gpui::Stateful<gpui::Div> {
    let url = issue.web_url.clone();
    let open = issue.state == "opened";
    div()
        .id(ElementId::Name(format!("gl-issue-{}-{}", column_label.unwrap_or(""), issue.iid).into()))
        .p_2p5()
        .flex()
        .flex_col()
        .gap_1p5()
        .rounded(RADIUS_MD)
        .bg(c(t.window))
        .border_1()
        .border_color(c(t.border))
        .cursor_pointer()
        .hover(|s| s.border_color(c(t.text_faint)))
        .on_click(move |_, _, cx| cx.open_url(&url))
        .child(div().t_body().text_color(c(if open { t.text } else { t.text_muted })).child(issue.title.clone()))
        .when(issue.labels.iter().any(|l| Some(l.as_str()) != column_label), |d| {
            d.child(div().flex().flex_row().flex_wrap().gap_1().children(issue.labels.iter().filter(|l| Some(l.as_str()) != column_label).take(4).map(|label| {
                div().px_1p5().rounded(RADIUS_MD).border_1().border_color(c(t.border)).t_caption().text_color(c(t.text_muted)).child(label.clone())
            })))
        })
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1p5()
                .t_caption()
                .text_color(c(t.text_faint))
                .child(div().text_color(c(if open { t.success } else { t.text_faint })).child(Icon::Issue.el(zpx(11.))))
                .child(format!("#{}", issue.iid))
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(match ago(&issue.updated_at) {
                    Some(age) => format!("@{} · {age}", issue.author),
                    None => format!("@{}", issue.author),
                }))
                .when(issue.comments > 0, |d| {
                    d.child(div().flex().flex_row().items_center().gap_0p5().child(Icon::Comment.el(zpx(11.))).child(issue.comments.to_string()))
                }),
        )
}

/// A project row while the list loads: name and path bars, sweeping.
fn project_skeleton(ix: usize, t: Tokens, reduced: bool) -> AnyElement {
    let phase = ix as f32 * 0.08;
    let hl = c(t.hover);
    let bar = |w: f32, h: f32, id: String| crate::motion::shimmer(div().h(zpx(h)).w(gpui::relative(w)).rounded(RADIUS_MD).bg(c(t.surface)), ElementId::Name(id.into()), phase, hl, reduced);
    let widths = [0.7, 0.5, 0.82, 0.6, 0.75, 0.45, 0.66, 0.55];
    div()
        .px_2()
        .py_1p5()
        .flex()
        .flex_col()
        .gap_1p5()
        .child(bar(widths[ix % widths.len()], 12., format!("gl-skel-p{ix}a")))
        .child(bar(widths[(ix + 3) % widths.len()] * 0.7, 9., format!("gl-skel-p{ix}b")))
        .into_any_element()
}

/// A board column while the board loads: header and a few card shapes.
fn column_skeleton(col: usize, t: Tokens, reduced: bool) -> AnyElement {
    let hl = c(t.hover);
    let bar = |w: f32, h: f32, id: String, phase: f32| {
        crate::motion::shimmer(div().h(zpx(h)).w(gpui::relative(w)).rounded(RADIUS_MD).bg(c(t.hover)), ElementId::Name(id.into()), phase, hl, reduced)
    };
    let cards = [3, 2, 4][col % 3];
    div()
        .w(zpx(280.))
        .flex_none()
        .flex()
        .flex_col()
        .rounded(RADIUS_LG)
        .bg(c(t.surface))
        .border_1()
        .border_color(c(t.border))
        .child(div().h(zpx(38.)).px_3().flex().items_center().border_b_1().border_color(c(t.border)).child(bar(0.35, 11., format!("gl-skel-c{col}h"), col as f32 * 0.1)))
        .child(div().p_2().flex().flex_col().gap_2().children((0..cards).map(|n| {
            let phase = (col as f32 * 0.1 + n as f32 * 0.07).fract();
            div()
                .p_2p5()
                .flex()
                .flex_col()
                .gap_2()
                .rounded(RADIUS_MD)
                .bg(c(t.window))
                .border_1()
                .border_color(c(t.border))
                .child(bar(0.9, 11., format!("gl-skel-c{col}-{n}a"), phase))
                .child(bar(0.6, 11., format!("gl-skel-c{col}-{n}b"), phase))
                .child(bar(0.35, 8., format!("gl-skel-c{col}-{n}c"), phase))
        })))
        .into_any_element()
}

fn empty_state(t: Tokens, cx: &mut Context<KeelWindow>) -> impl IntoElement {
    div().flex_1().flex().flex_col().items_center().justify_center().gap_2().child(
        div()
            .max_w(zpx(340.))
            .flex()
            .flex_col()
            .items_center()
            .gap_2()
            .child(div().text_color(c(t.text_faint)).child(Icon::GitLab.el(zpx(28.))))
            .child(div().t_body().font_weight(gpui::FontWeight::MEDIUM).child("Connect GitLab first"))
            .child(div().t_small().text_center().text_color(c(t.text_muted)).child("Add gitlab.com or your own instance in Settings → Connections."))
            .child(text_button("gl-open-settings-btn", "Open Settings", t).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.open_settings(cx)))),
    )
}

fn note(t: Tokens, text: &'static str) -> gpui::Div {
    div().px_2().py_3().t_small().text_color(c(t.text_faint)).child(text)
}

fn error(t: Tokens, err: &str) -> gpui::Div {
    div().px_2().py_3().flex().flex_row().items_center().gap_1p5().t_small().text_color(c(t.danger)).child(Icon::Alert.el(zpx(12.))).child(err.to_string())
}

/// "5m ago", "3h ago", "2d ago", "4mo ago" for a GitLab ISO time, or `None`.
fn ago(iso: &str) -> Option<String> {
    let secs = (crate::state::AppState::now_ms() / 1000).checked_sub(unix_secs(iso)?)?.max(0);
    Some(match secs {
        s if s < 60 => "just now".to_string(),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s if s < 86_400 * 30 => format!("{}d ago", s / 86_400),
        s if s < 86_400 * 365 => format!("{}mo ago", s / (86_400 * 30)),
        s => format!("{}y ago", s / (86_400 * 365)),
    })
}

/// Seconds since the epoch for `YYYY-MM-DDTHH:MM:SS…` in UTC (GitLab's `Z`
/// times). Offsets other than `Z` are read as UTC; close enough for "ago".
fn unix_secs(iso: &str) -> Option<i64> {
    let num = |range: std::ops::Range<usize>| iso.get(range)?.parse::<i64>().ok();
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hh, mm, ss) = (num(11..13)?, num(14..16)?, num(17..19)?);
    // Days from civil (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hh * 3600 + mm * 60 + ss)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_menu_shows_a_few_then_filters_by_name_or_path() {
        let project = |name: &str, path: &str| {
            ("h".to_string(), Project { id: 0, name: name.into(), path: path.into(), web_url: String::new(), last_activity_at: String::new() })
        };
        let all = [project("CMS", "bank/cms"), project("k8s", "pama/k8s"), project("RIF Specs", "bank/rif-specs")];
        let names = |(rows, more): (Vec<&(String, Project)>, usize)| (rows.iter().map(|(_, p)| p.name.clone()).collect::<Vec<_>>(), more);
        assert_eq!(names(filter_projects(&all, "", 2)), (vec!["CMS".to_string(), "k8s".into()], 1));
        assert_eq!(names(filter_projects(&all, "bank", 5)), (vec!["CMS".to_string(), "RIF Specs".into()], 0));
        assert_eq!(names(filter_projects(&all, "specs", 5)), (vec!["RIF Specs".to_string()], 0));
        assert!(filter_projects(&all, "zzz", 5).0.is_empty());
    }

    #[test]
    fn gitlab_times_turn_into_unix_seconds() {
        assert_eq!(unix_secs("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(unix_secs("2026-10-08T09:58:00.123Z"), Some(1_791_453_480));
        assert_eq!(unix_secs("2000-02-29T12:00:00Z"), Some(951_825_600));
        assert_eq!(unix_secs("garbage"), None);
    }

    #[test]
    fn board_puts_issues_in_open_label_and_closed_columns() {
        let issue = |iid: u64, labels: &[&str]| Issue {
            iid,
            title: String::new(),
            state: "opened".into(),
            labels: labels.iter().map(|l| l.to_string()).collect(),
            author: String::new(),
            comments: 0,
            updated_at: String::new(),
            web_url: String::new(),
        };
        let lists = vec![("Doing".to_string(), hex_color("#428BCA")), ("Review".to_string(), None)];
        let cols = board_columns(&lists, vec![issue(1, &[]), issue(2, &["Doing"]), issue(3, &["Doing", "Review"]), issue(4, &["bug"])], vec![issue(9, &[])]);
        let titles: Vec<_> = cols.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, ["Open", "Doing", "Review", "Closed"]);
        let iids = |ix: usize| cols[ix].issues.iter().map(|i| i.iid).collect::<Vec<_>>();
        assert_eq!((iids(0), iids(1), iids(2), iids(3)), (vec![1, 4], vec![2, 3], vec![3], vec![9]));
        assert_eq!(cols[1].color, Some(0x428BCAFF));
        assert_eq!(hex_color("nope"), None);
    }

    #[test]
    fn rows_parse_from_the_engine_shape() {
        let p = Project::parse(&serde_json::json!({"id": 7, "name": "keel", "path": "kev/keel", "web_url": "u", "last_activity_at": "x"}));
        assert_eq!((p.id, p.path.as_str()), (7, "kev/keel"));
        let i = Issue::parse(&serde_json::json!({"iid": 3, "title": "Bug", "state": "opened", "labels": ["a", "b"], "author": "kev", "comments": 2}));
        assert_eq!((i.iid, i.labels.len(), i.comments), (3, 2, 2));
    }
}

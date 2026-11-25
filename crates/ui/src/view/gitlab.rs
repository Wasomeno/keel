//! GitLab project and issue data for the composer's issue-repo and `@` pickers.

use gpui::Context;

use super::KeelWindow;

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

/// The repo menu's rows: see [`super::composer_view::filter_rows`]; a repo
/// matches on its name or path.
pub(super) fn filter_projects<'a>(
    projects: &'a [(String, Project)],
    query: &str,
    limit: usize,
) -> (Vec<&'a (String, Project)>, usize) {
    super::composer_view::filter_rows(projects, query, limit, |(_, p)| {
        [p.name.as_str(), p.path.as_str()]
    })
}

fn s(v: &serde_json::Value, key: &str) -> String {
    v[key].as_str().unwrap_or_default().to_string()
}

impl Project {
    fn parse(v: &serde_json::Value) -> Self {
        Self {
            id: v["id"].as_u64().unwrap_or(0),
            name: s(v, "name"),
            path: s(v, "path"),
            web_url: s(v, "web_url"),
            last_activity_at: s(v, "last_activity_at"),
        }
    }
}

impl Issue {
    fn parse(v: &serde_json::Value) -> Self {
        Self {
            iid: v["iid"].as_u64().unwrap_or(0),
            title: s(v, "title"),
            state: s(v, "state"),
            labels: v["labels"]
                .as_array()
                .map(|l| {
                    l.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            author: s(v, "author"),
            comments: v["comments"].as_u64().unwrap_or(0),
            updated_at: s(v, "updated_at"),
            web_url: s(v, "web_url"),
        }
    }
}

impl KeelWindow {
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
                let v = client
                    .call("list_connections", serde_json::json!({}))
                    .await?;
                let mut projects = Vec::new();
                let mut failed = None;
                for host in v["connections"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|c| c["kind"].as_str() == Some("gitlab"))
                    .filter_map(|c| c["host"].as_str().map(str::to_string))
                {
                    match client
                        .call("gitlab_projects", serde_json::json!({ "host": host }))
                        .await
                    {
                        Ok(v) => projects.extend(
                            v["projects"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .map(|p| (host.clone(), Project::parse(p))),
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
    pub(super) fn select_issue_repo(
        &mut self,
        repo: Option<(String, Project)>,
        cx: &mut Context<Self>,
    ) {
        self.issue_repo = repo.clone();
        self.repo_issues = None;
        let (Some((host, project)), Some(client)) = (repo, self.client.clone()) else {
            cx.notify();
            return;
        };
        cx.spawn(async move |this, cx| {
            let res = client
                .call(
                    "gitlab_board",
                    serde_json::json!({ "host": host, "project_id": project.id }),
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                // A newer pick wins over this one landing late.
                if this.issue_repo.as_ref().map(|(h, p)| (h, p.id)) != Some((&host, project.id)) {
                    return;
                }
                let issues = res
                    .map(|v| {
                        v["opened"]
                            .as_array()
                            .map(|r| r.iter().map(Issue::parse).collect())
                            .unwrap_or_default()
                    })
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_menu_shows_a_few_then_filters_by_name_or_path() {
        let project = |name: &str, path: &str| {
            (
                "h".to_string(),
                Project {
                    id: 0,
                    name: name.into(),
                    path: path.into(),
                    web_url: String::new(),
                    last_activity_at: String::new(),
                },
            )
        };
        let all = [
            project("CMS", "bank/cms"),
            project("k8s", "pama/k8s"),
            project("RIF Specs", "bank/rif-specs"),
        ];
        let names = |(rows, more): (Vec<&(String, Project)>, usize)| {
            (
                rows.iter().map(|(_, p)| p.name.clone()).collect::<Vec<_>>(),
                more,
            )
        };
        assert_eq!(
            names(filter_projects(&all, "", 2)),
            (vec!["CMS".to_string(), "k8s".into()], 1)
        );
        assert_eq!(
            names(filter_projects(&all, "bank", 5)),
            (vec!["CMS".to_string(), "RIF Specs".into()], 0)
        );
        assert_eq!(
            names(filter_projects(&all, "specs", 5)),
            (vec!["RIF Specs".to_string()], 0)
        );
        assert!(filter_projects(&all, "zzz", 5).0.is_empty());
    }

    #[test]
    fn rows_parse_from_the_engine_shape() {
        let p = Project::parse(
            &serde_json::json!({"id": 7, "name": "keel", "path": "kev/keel", "web_url": "u", "last_activity_at": "x"}),
        );
        assert_eq!((p.id, p.path.as_str()), (7, "kev/keel"));
        let i = Issue::parse(
            &serde_json::json!({"iid": 3, "title": "Bug", "state": "opened", "labels": ["a", "b"], "author": "kev", "comments": 2}),
        );
        assert_eq!((i.iid, i.labels.len(), i.comments), (3, 2, 2));
    }
}

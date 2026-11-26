//! Settings → Skills: inline first-class tab for browsing, filtering,
//! toggling, adding, and inspecting agent skills.

use gpui::{
    AnimationExt, AnyElement, ClickEvent, Context, ElementId, IntoElement, SpringAnimation, div,
    prelude::*,
};
use keel_theme::Tokens;

use super::super::KeelWindow;
use super::controls::{ButtonKind, button, icon_button, open_browser_url, reveal_in_finder, toggle};
use super::{
    CatalogSkillRow, MemoryTab, SkillConfirm, SkillRow, SkillStatusFilter, SkillsViewMode,
};
use crate::hover_fade::HoverFadeExt;
use crate::icons::Icon;
use crate::style::{INPUT_H, KeelStyled, MONO, c, r_lg, r_md, r_sm, zpx};

impl KeelWindow {
    pub(in crate::view) fn load_skills(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.skills.load_gen += 1;
        let ticket = self.skills.load_gen;
        cx.spawn(async move |this, cx| {
            let res = client
                .call(
                    "list_skills",
                    serde_json::json!({ "cwd": serde_json::Value::Null }),
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.skills.load_gen != ticket {
                    return;
                }
                this.skills.rows = Some(match res {
                    Ok(v) => {
                        let parsed: Vec<SkillRow> = v["skills"]
                            .as_array()
                            .map(|rows| {
                                rows.iter()
                                    .map(SkillRow::parse)
                                    .filter(|r| r.scope != "project")
                                    .collect()
                            })
                            .unwrap_or_default();
                        let mut missing_repos: Vec<String> = Vec::new();
                        for r in &parsed {
                            if let Some(repo) = installed_skill_repo(r) {
                                if let Some((owner, _)) = repo.split_once('/') {
                                    let key = owner.to_ascii_lowercase();
                                    if !this.skills.catalog_avatars.contains_key(&key) {
                                        missing_repos.push(repo);
                                    }
                                }
                            }
                        }
                        if !missing_repos.is_empty() && !this.skills.catalog_avatars_fetching {
                            this.prefetch_catalog_avatars(missing_repos, cx);
                        }
                        if let Some(open) = this.skills.modal_skill.as_mut() {
                            if let Some(updated) = parsed.iter().find(|r| r.path == open.path) {
                                *open = updated.clone();
                            }
                        }
                        Ok(parsed)
                    }
                    Err(err) => Err(err),
                });
                cx.notify();
            });
        })
        .detach();
    }

    pub(in crate::view) fn open_skills(&mut self, cx: &mut Context<Self>) {
        self.close_pages();
        self.skills.open = true;
        self.skills.notice = None;
        self.skills.confirm = None;
        self.skills.added = None;
        self.skills.author_filter = None;
        self.skills.installed_usage_filter = None;
        self.skills.category_filter = None;
        self.skills.catalog_author_filter = None;
        self.skills.catalog_filter_menu_open = false;
        self.skills.status_filter = SkillStatusFilter::All;
        self.skills.filter_epoch = 0;
        self.skills.expanded_skill = None;
        self.skills.modal_skill = None;
        self.skills.modal_leaving = None;
        self.skills.modal_catalog = None;
        self.skills.modal_catalog_leaving = None;
        self.skills.detail_nav_at = None;
        self.skills
            .installed_scroll
            .set_offset(gpui::point(gpui::px(0.), gpui::px(0.)));
        self.skills
            .catalog_scroll
            .set_offset(gpui::point(gpui::px(0.), gpui::px(0.)));
        self.load_skills(cx);
        self.load_catalog(false, cx);
        cx.notify();
    }

    /// Stale-while-revalidate catalog load: paints cached/bundled skills right away
    /// when `catalog_rows` is empty, then refreshes from `skills.json` if stale
    /// (or always when `force` is true).
    pub(in crate::view) fn load_catalog(&mut self, force: bool, cx: &mut Context<Self>) {
        if self.client.is_none() {
            return;
        }
        if !force && self.skills.catalog_rows.is_none() {
            self.load_catalog_request(false, true, cx);
        }
        self.skills.catalog_refreshing = true;
        if force {
            self.skills.catalog_warning = None;
        }
        cx.notify();
        self.load_catalog_request(force, false, cx);
    }

    /// Re-reads the local disk cache / bundled snapshot and re-checks installed
    /// status without hitting the network.
    pub(in crate::view) fn load_catalog_cache_only(&mut self, cx: &mut Context<Self>) {
        self.load_catalog_request(false, true, cx);
    }

    fn load_catalog_request(&mut self, force: bool, cache_only: bool, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.skills.catalog_gen += 1;
        let ticket = self.skills.catalog_gen;
        cx.spawn(async move |this, cx| {
            let res = client
                .call(
                    "browse_skills",
                    serde_json::json!({ "force": force, "cache_only": cache_only }),
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                // Allow a later network response to overwrite an earlier cache_only
                // paint, while dropping responses older than the latest non-cache ticket.
                if !cache_only && this.skills.catalog_gen != ticket {
                    return;
                }
                if !cache_only {
                    this.skills.catalog_refreshing = false;
                }
                match res {
                    Ok(v) => {
                        let parsed: Vec<CatalogSkillRow> = v["skills"]
                            .as_array()
                            .map(|rows| rows.iter().map(CatalogSkillRow::parse).collect())
                            .unwrap_or_default();
                        this.skills.catalog_source =
                            v["source"].as_str().unwrap_or_default().to_string();
                        this.skills.catalog_warning = if force {
                            v["warning"].as_str().map(str::to_string)
                        } else {
                            None
                        };
                        // Seed cached avatar paths returned by browse_skills and
                        // prefetch any missing GitHub creator logos in the background.
                        let mut missing_repos: Vec<String> = Vec::new();
                        for s in &parsed {
                            if let Some((owner, _)) = s.repo.split_once('/') {
                                let key = owner.to_ascii_lowercase();
                                if let Some(ref p) = s.avatar_path {
                                    this.skills
                                        .catalog_avatars
                                        .insert(key, std::path::PathBuf::from(p));
                                } else if !this.skills.catalog_avatars.contains_key(&key) {
                                    missing_repos.push(s.repo.clone());
                                }
                            }
                        }
                        if !missing_repos.is_empty() && !this.skills.catalog_avatars_fetching {
                            this.prefetch_catalog_avatars(missing_repos, cx);
                        }
                        // Keep any open catalog modal synced with fresh installed flags.
                        if let Some(open) = this.skills.modal_catalog.as_mut() {
                            if let Some(updated) = parsed.iter().find(|s| s.id == open.id) {
                                *open = updated.clone();
                            }
                        }
                        this.skills.catalog_rows = Some(Ok(parsed));
                    }
                    Err(err) => {
                        if this.skills.catalog_rows.is_none() || force {
                            this.skills.catalog_rows = Some(Err(err));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn prefetch_catalog_avatars(&mut self, repos: Vec<String>, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.skills.catalog_avatars_fetching = true;
        cx.spawn(async move |this, cx| {
            let res = client
                .call(
                    "prefetch_skill_avatars",
                    serde_json::json!({ "repos": repos }),
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                this.skills.catalog_avatars_fetching = false;
                if let Ok(v) = res {
                    if let Some(map) = v["avatars"].as_object() {
                        for (owner, path_val) in map {
                            if let Some(p) = path_val.as_str() {
                                this.skills
                                    .catalog_avatars
                                    .insert(owner.clone(), std::path::PathBuf::from(p));
                            }
                        }
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    pub(in crate::view) fn open_catalog_modal(
        &mut self,
        skill: &CatalogSkillRow,
        cx: &mut Context<Self>,
    ) {
        self.skills.modal_skill = None;
        self.skills.modal_leaving = None;
        self.skills.modal_catalog = Some(skill.clone());
        self.skills.modal_catalog_leaving = None;
        self.skills.creator_menu_open = false;
        self.skills.category_menu_open = false;
        self.skills.detail_nav_gen = self.skills.detail_nav_gen.wrapping_add(1);
        self.skills.detail_nav_forward = true;
        self.skills.detail_nav_at = Some(std::time::Instant::now());
        self.skills
            .modal_scroll
            .set_offset(gpui::point(gpui::px(0.), gpui::px(0.)));
        if !self.skills.catalog_readmes.contains_key(&skill.id)
            && !self.skills.catalog_readme_loading.contains(&skill.id)
        {
            self.fetch_catalog_readme(skill, cx);
        }
        cx.notify();
    }

    fn fetch_catalog_readme(&mut self, skill: &CatalogSkillRow, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let id = skill.id.clone();
        let repo = skill.repo.clone();
        let git_ref = skill.git_ref().to_string();
        let path = skill.path.clone();
        self.skills.catalog_readme_loading.insert(id.clone());
        cx.spawn(async move |this, cx| {
            let res = client
                .call(
                    "fetch_catalog_skill_readme",
                    serde_json::json!({ "repo": repo, "ref": git_ref, "path": path }),
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                this.skills.catalog_readme_loading.remove(&id);
                let entry = match res {
                    Ok(v) => Ok(v["content"].as_str().unwrap_or_default().to_string()),
                    Err(err) => Err(err),
                };
                this.skills.catalog_readmes.insert(id, entry);
                cx.notify();
            });
        })
        .detach();
    }

    pub(in crate::view) fn dismiss_catalog_modal(&mut self, cx: &mut Context<Self>) {
        if self.skills.modal_catalog.take().is_some() {
            self.skills.modal_catalog_leaving = None;
            self.skills.detail_nav_gen = self.skills.detail_nav_gen.wrapping_add(1);
            self.skills.detail_nav_forward = false;
            self.skills.detail_nav_at = Some(std::time::Instant::now());
            cx.notify();
        }
    }

    fn install_catalog_skill(
        &mut self,
        skill: CatalogSkillRow,
        replace: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let id = skill.id.clone();
        if self.skills.installing.contains(&id) {
            return;
        }
        self.skills.installing.insert(id.clone());
        self.skills.install_errors.remove(&id);
        self.skills.notice = None;
        let payload = skill.to_entry_json();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = client
                .call(
                    "install_catalog_skill",
                    serde_json::json!({ "skill": payload, "replace": replace }),
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                this.skills.installing.remove(&id);
                match res {
                    Ok(_) => {
                        this.skills.install_errors.remove(&id);
                        this.skills.notice = None;
                        this.commands.clear();
                        this.load_skills(cx);
                        this.load_catalog_cache_only(cx);
                    }
                    Err(err) => {
                        let msg = if let Some(name) = err.strip_prefix("exists:") {
                            format!("“{name}” is already installed.")
                        } else {
                            err
                        };
                        this.skills.install_errors.insert(id, msg.clone());
                        this.skills.notice = Some(msg);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(in crate::view) fn open_skill_modal(&mut self, skill: &SkillRow, cx: &mut Context<Self>) {
        self.skills.modal_catalog = None;
        self.skills.modal_catalog_leaving = None;
        self.skills.modal_skill = Some(skill.clone());
        self.skills.modal_leaving = None;
        self.skills.expanded_skill = Some(skill.path.clone());
        self.skills.creator_menu_open = false;
        self.skills.category_menu_open = false;
        self.skills.detail_nav_gen = self.skills.detail_nav_gen.wrapping_add(1);
        self.skills.detail_nav_forward = true;
        self.skills.detail_nav_at = Some(std::time::Instant::now());
        self.skills
            .modal_scroll
            .set_offset(gpui::point(gpui::px(0.), gpui::px(0.)));
        if !self.skills.expanded_content.contains_key(&skill.path) {
            let file = std::path::Path::new(&skill.path).join("SKILL.md");
            let text = std::fs::read_to_string(&file).unwrap_or_default();
            self.skills
                .expanded_content
                .insert(skill.path.clone(), text);
        }
        cx.notify();
    }

    pub(in crate::view) fn dismiss_skill_modal(&mut self, cx: &mut Context<Self>) {
        if self.skills.modal_skill.take().is_some() {
            self.skills.expanded_skill = None;
            self.skills.modal_leaving = None;
            self.skills.detail_nav_gen = self.skills.detail_nav_gen.wrapping_add(1);
            self.skills.detail_nav_forward = false;
            self.skills.detail_nav_at = Some(std::time::Instant::now());
            cx.notify();
        }
    }

    fn recent_skill_detail_nav(&self) -> bool {
        !self.reduced()
            && self
                .skills
                .detail_nav_at
                .is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(220))
    }

    fn recent_skill_tab_nav(&self) -> bool {
        !self.reduced()
            && self
                .skills
                .tab_nav_at
                .is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(200))
    }

    fn recent_skill_filter_nav(&self) -> bool {
        !self.reduced()
            && self
                .skills
                .filter_at
                .is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(180))
    }

    /// Full-pane Skills workspace, opened from the main sidebar.
    pub(in crate::view) fn render_skills_page(
        &mut self,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let animating = self.recent_skill_detail_nav();
        let nav_gen = self.skills.detail_nav_gen;
        let forward = self.skills.detail_nav_forward;

        let page: gpui::Div = if let Some(skill) = self.skills.modal_skill.clone() {
            self.render_installed_skill_detail_page(skill, t, cx)
        } else if let Some(skill) = self.skills.modal_catalog.clone() {
            self.render_catalog_skill_detail_page(skill, t, cx)
        } else {
            self.render_skills_body(t, cx)
        };

        let sign = if forward { 1.0 } else { -1.0 };
        let animated = page.with_animation(
            ElementId::Name(format!("skills-nav-{nav_gen}").into()),
            gpui::Animation::new(std::time::Duration::from_millis(180))
                .with_easing(crate::motion::ease_out_strong),
            move |el, delta| {
                if animating {
                    el.relative()
                        .left(zpx(sign * 14.0 * (1.0 - delta)))
                        .opacity(delta)
                } else {
                    el
                }
            },
        );

        div()
            .flex_1()
            .min_h_0()
            .w_full()
            .bg(c(t.window))
            .flex()
            .flex_col()
            .items_center()
            .child(
                div()
                    .w_full()
                    .max_w(zpx(960.))
                    .flex_1()
                    .min_h_0()
                    .px(zpx(32.))
                    .pt(crate::style::HEADER_HEIGHT + zpx(10.))
                    .pb(zpx(24.))
                    .flex()
                    .flex_col()
                    .child(animated),
            )
            .into_any_element()
    }

    fn render_skills_body(&mut self, t: Tokens, cx: &mut Context<Self>) -> gpui::Div {
        let missing = self.pi_missing();
        let mut body = div().size_full().flex().flex_col().gap(zpx(12.));

        if missing {
            return body.child(
                div()
                    .py(zpx(12.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(8.))
                    .t_body()
                    .text_color(c(t.danger))
                    .child(Icon::Alert.el(zpx(14.)))
                    .child("Keel harness is unavailable"),
            );
        }

        let mode = self.skills.view_mode;
        let tab_animating = self.recent_skill_tab_nav();
        let tab_gen = self.skills.tab_nav_gen;
        let tab_sign = if self.skills.tab_nav_forward { 1.0 } else { -1.0 };
        body = body.child(self.render_skills_top_bar(t, missing, cx));

        let tab_content = if mode == SkillsViewMode::Discover {
            self.render_discover_body(div().flex_1().min_h_0().w_full().flex().flex_col().gap(zpx(12.)), t, cx)
        } else {
            self.render_installed_body(div().flex_1().min_h_0().w_full().flex().flex_col().gap(zpx(12.)), t, cx)
        };

        // Emil directional crossfade on Installed ↔ Discover switch (160ms ease-out-strong, 10px hint).
        body.child(
            tab_content.with_animation(
                ElementId::Name(format!("skills-tab-body-{tab_gen}").into()),
                gpui::Animation::new(std::time::Duration::from_millis(160))
                    .with_easing(crate::motion::ease_out_strong),
                move |el, delta| {
                    if tab_animating {
                        el.relative()
                            .left(zpx(tab_sign * 10.0 * (1.0 - delta)))
                            .opacity(delta)
                    } else {
                        el
                    }
                },
            ),
        )
    }

    fn render_installed_body(
        &mut self,
        mut body: gpui::Div,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let query = self.skills_filter.read(cx).value().to_string();
        let needle = query.trim().to_lowercase();
        let rows = self.skills.rows.clone();
        let reduced = self.reduced();

        let all_skills = match rows {
            None => return body,
            Some(Err(err)) => {
                return body.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(zpx(8.))
                        .py(zpx(8.))
                        .child(
                            div()
                                .t_body()
                                .text_color(c(t.text))
                                .child("Couldn't read skills."),
                        )
                        .child(div().t_small().text_color(c(t.danger)).child(err))
                        .child(
                            button("skills-retry", "Retry", ButtonKind::Ghost, true, t).on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| this.load_skills(cx)),
                            ),
                        ),
                );
            }
            Some(Ok(skills)) => skills,
        };

        if let Some(notice) = self.skills.notice.clone() {
            body = body.child(
                div()
                    .pb(zpx(8.))
                    .t_small()
                    .text_color(c(t.danger))
                    .child(notice),
            );
        }
        // The banner rises in, and drops out on Cancel, quicker than it came.
        if let Some(confirm) = self.skills.confirm.clone() {
            let banner = self.render_skill_confirm(&confirm, t, cx);
            body = body.child(crate::motion::appear(
                banner,
                ("skills-confirm-in", self.skills.confirm_gen as usize),
                crate::motion::BASE,
                6.,
                reduced,
            ));
        } else if let Some((n, confirm)) = self.skills.confirm_leaving.clone() {
            let banner = self.render_skill_confirm(&confirm, t, cx);
            body = body.child(crate::motion::dismiss(
                banner,
                ("skills-confirm-out", n as usize),
                reduced,
            ));
        }

        // Collect author stats & usage category stats for the merged Filter menu and pill strip.
        let mut author_counts: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        let mut usage_counts: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        for s in &all_skills {
            let auth = installed_creator_label(s);
            *author_counts.entry(auth).or_insert(0) += 1;
            *usage_counts
                .entry(infer_skill_usage(s).to_string())
                .or_insert(0) += 1;
        }
        let mut authors: Vec<(String, usize)> = author_counts.into_iter().collect();
        authors.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let mut usage_pills: Vec<(String, usize)> = usage_counts.into_iter().collect();
        usage_pills.sort_by(|a, b| {
            let a_gen = a.0 == "General";
            let b_gen = b.0 == "General";
            a_gen
                .cmp(&b_gen)
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| a.0.cmp(&b.0))
        });

        body = body.child(self.render_skill_controls(&authors, &usage_pills, t, cx));

        // Filtering
        let status_filter = self.skills.status_filter;
        let author_filter = self.skills.author_filter.clone();
        let usage_filter = self.skills.installed_usage_filter.clone();

        let matched: Vec<SkillRow> = all_skills
            .into_iter()
            .filter(|row| {
                // Status filter
                match status_filter {
                    SkillStatusFilter::All => {}
                    SkillStatusFilter::Enabled => {
                        if !row.enabled {
                            return false;
                        }
                    }
                    SkillStatusFilter::Disabled => {
                        if row.enabled {
                            return false;
                        }
                    }
                }
                // Usage filter
                if let Some(ref selected_usage) = usage_filter {
                    if infer_skill_usage(row) != selected_usage {
                        return false;
                    }
                }
                // Author filter
                if let Some(ref selected_auth) = author_filter {
                    if installed_creator_label(row) != *selected_auth {
                        return false;
                    }
                }
                // Search query
                if !needle.is_empty() {
                    let matches_name = row.name.to_lowercase().contains(&needle);
                    let matches_desc = row.description.to_lowercase().contains(&needle);
                    let matches_auth = row.author.to_lowercase().contains(&needle);
                    let matches_coll = row.collection.to_lowercase().contains(&needle);
                    let display_insert = row
                        .insert
                        .strip_prefix("/skill:")
                        .map(|n| format!("/{n}"))
                        .unwrap_or_else(|| row.insert.clone());
                    let matches_insert = display_insert.to_lowercase().contains(&needle)
                        || row.insert.to_lowercase().contains(&needle);
                    if !matches_name
                        && !matches_desc
                        && !matches_auth
                        && !matches_coll
                        && !matches_insert
                    {
                        return false;
                    }
                }
                true
            })
            .collect();

        if matched.is_empty() {
            let is_filtering = !needle.is_empty()
                || author_filter.is_some()
                || usage_filter.is_some()
                || status_filter != SkillStatusFilter::All;
            let empty_box = if is_filtering {
                div()
                    .py(zpx(32.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(zpx(12.))
                    .child(Icon::Search.el(zpx(24.)).text_color(c(t.text_faint)))
                    .child(
                        div()
                            .t_body()
                            .text_color(c(t.text_muted))
                            .child("No skills match your search or filter."),
                    )
                    .child(
                        button(
                            "skills-reset-filters",
                            "Reset filters",
                            ButtonKind::Ghost,
                            true,
                            t,
                        )
                        .on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| {
                                this.skills.author_filter = None;
                                this.skills.installed_usage_filter = None;
                                this.skills.status_filter = SkillStatusFilter::All;
                                this.skills
                                    .installed_scroll
                                    .set_offset(gpui::point(gpui::px(0.), gpui::px(0.)));
                                cx.notify();
                            },
                        )),
                    )
            } else {
                div()
                    .py(zpx(32.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(zpx(8.))
                    .child(Icon::Puzzle.el(zpx(26.)).text_color(c(t.text_faint)))
                    .child(
                        div()
                            .t_body()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(c(t.text))
                            .child("No skills yet"),
                    )
                    .child(
                        div()
                            .t_small()
                            .text_color(c(t.text_muted))
                            .child("Click “+ Add skill…” to install a skill folder into Keel."),
                    )
            };
            return body.child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(empty_box),
            );
        }

        let weak = cx.entity().downgrade();
        let row_errors = self.skills.row_errors.clone();
        let avatars = self.skills.catalog_avatars.clone();

        // Always group installed skills by Usage category (matching Discover).
        let mut group_map: std::collections::BTreeMap<String, Vec<SkillRow>> =
            std::collections::BTreeMap::new();
        for skill in matched {
            group_map
                .entry(infer_skill_usage(&skill).to_string())
                .or_default()
                .push(skill);
        }
        let mut groups: Vec<(String, Vec<SkillRow>)> = group_map.into_iter().collect();
        groups.sort_by(|a, b| {
            let a_other = a.0 == "General" || a.0 == "Other";
            let b_other = b.0 == "General" || b.0 == "Other";
            a_other
                .cmp(&b_other)
                .then_with(|| b.1.len().cmp(&a.1.len()))
                .then_with(|| a.0.cmp(&b.0))
        });

        let mut sections = div().flex().flex_col().gap(zpx(24.)).pb(zpx(20.));
        for (group_name, items) in groups {
            let group_count = items.len();

            let mut grid = div().w_full().flex().flex_col().gap(zpx(8.));
            for pair in items.chunks(2) {
                let mut row_el = div().w_full().flex().flex_row().gap(zpx(8.));
                for skill in pair {
                    let path = skill.path.clone();
                    let name = skill.name.clone();
                    let description = skill.description.clone();
                    let scope = skill.scope.clone();
                    let enabled = skill.enabled;
                    let packaged = scope == "package";
                    let error = row_errors.get(&path).cloned();
                    let creator = installed_creator_label(skill);
                    let repo = installed_skill_repo(skill).unwrap_or_default();
                    let cmd = skill
                        .insert
                        .strip_prefix("/skill:")
                        .map(|n| format!("/{n}"))
                        .unwrap_or_else(|| skill.insert.clone());
                    let card_avatar = render_creator_avatar(&creator, &repo, &avatars, 18.0);

                    let weak_row = weak.clone();
                    let skill_for_click = skill.clone();
                    let path_for_toggle = path.clone();
                    let scope_for_toggle = scope.clone();

                    let card = div()
                        .id(ElementId::Name(format!("skill-card-{path}").into()))
                        .flex_1()
                        .min_w_0()
                        .p(zpx(16.))
                        .rounded(r_lg())
                        .border_1()
                        .border_color(c(t.border))
                        .hover_fade(c(t.hover), None)
                        .active(|s| s.opacity(0.84))
                        .cursor_pointer()
                        .flex()
                        .flex_col()
                        .justify_between()
                        .gap(zpx(10.))
                        .on_click(move |_, _, cx| {
                            let skill = skill_for_click.clone();
                            let _ = weak_row.update(cx, |this, cx| {
                                this.open_skill_modal(&skill, cx);
                            });
                        })
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_start()
                                .justify_between()
                                .gap(zpx(10.))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .gap(zpx(4.))
                                        .child(
                                            div()
                                                .min_w_0()
                                                .truncate()
                                                .t_body()
                                                .font_weight(gpui::FontWeight::MEDIUM)
                                                .text_color(c(if enabled {
                                                    t.text
                                                } else {
                                                    t.text_muted
                                                }))
                                                .child(name),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .flex_row()
                                                .items_center()
                                                .gap(zpx(6.))
                                                .min_w_0()
                                                .overflow_hidden()
                                                .children(card_avatar)
                                                .child(
                                                    div()
                                                        .min_w_0()
                                                        .truncate()
                                                        .t_caption()
                                                        .text_color(c(t.text_faint))
                                                        .child(format!("{creator} · {cmd}")),
                                                ),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap(zpx(8.))
                                        .when(error.is_some(), |d| {
                                            d.child(
                                                div()
                                                    .px(zpx(6.))
                                                    .py(zpx(2.))
                                                    .rounded(r_sm())
                                                    .t_caption()
                                                    .text_color(c(t.danger))
                                                    .child("Error"),
                                            )
                                        })
                                        .child(if packaged {
                                            div()
                                                .t_caption()
                                                .text_color(c(t.text_faint))
                                                .child("Package")
                                                .into_any_element()
                                        } else {
                                            let weak_toggle = weak.clone();
                                            let path_t = path_for_toggle.clone();
                                            let scope_t = scope_for_toggle.clone();
                                            toggle(format!("skill-{path_t}"), enabled, t)
                                                .on_click(move |_, _, cx| {
                                                    cx.stop_propagation();
                                                    let path = path_t.clone();
                                                    let scope = scope_t.clone();
                                                    let _ = weak_toggle.update(cx, |this, cx| {
                                                        this.toggle_skill(
                                                            path, !enabled, scope, cx,
                                                        );
                                                    });
                                                })
                                                .into_any_element()
                                        }),
                                ),
                        )
                        .child(
                            div()
                                .t_small()
                                .text_color(c(if enabled {
                                    t.text_muted
                                } else {
                                    t.text_faint
                                }))
                                .child(description),
                        );

                    row_el = row_el.child(card);
                }
                if pair.len() == 1 {
                    row_el = row_el.child(div().flex_1().min_w_0());
                }
                grid = grid.child(row_el);
            }

            sections = sections.child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(zpx(10.))
                    .child(render_section_header(
                        &group_name,
                        Some(category_icon(&group_name)),
                        Some(category_color(&group_name, t)),
                        group_count,
                        t,
                    ))
                    .child(grid),
            );
        }

        let filter_epoch = self.skills.filter_epoch as usize;
        let filter_anim = self.recent_skill_filter_nav();
        let filter_sign = if self.skills.filter_forward { 1.0 } else { -1.0 };
        let scroll_list = div()
            .id("skills-installed-scroll")
            .track_scroll(&self.skills.installed_scroll)
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .pr(zpx(4.))
            .with_animation(
                ("skills-filter-list", filter_epoch),
                gpui::Animation::new(std::time::Duration::from_millis(160))
                    .with_easing(crate::motion::ease_out_strong),
                move |el, delta| {
                    if filter_anim {
                        el.relative()
                            .left(zpx(filter_sign * 12.0 * (1.0 - delta)))
                            .opacity(delta)
                    } else {
                        el
                    }
                },
            )
            .child(sections);

        body.child(scroll_list)
    }

    fn render_skills_top_bar(
        &mut self,
        t: Tokens,
        missing: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let mode = self.skills.view_mode;
        let installed_count = self
            .skills
            .rows
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .map(|rows| rows.len());
        let enabled_count = self
            .skills
            .rows
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .map(|rows| rows.iter().filter(|r| r.enabled).count());
        let catalog_count = self
            .skills
            .catalog_rows
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .map(|rows| rows.len());

        let right_actions = match mode {
            SkillsViewMode::Installed => div()
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(8.))
                .when(!missing, |d| {
                    d.child(
                        button(
                            "skills-add-btn",
                            "+ Add skill…",
                            ButtonKind::Secondary,
                            true,
                            t,
                        )
                        .on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.choose_skill_folder(cx)),
                        ),
                    )
                }),
            SkillsViewMode::Discover => {
                let refreshing = self.skills.catalog_refreshing;
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(6.))
                    .when(refreshing, |d| {
                        d.child(
                            div()
                                .t_caption()
                                .text_color(c(t.text_faint))
                                .child("Refreshing…"),
                        )
                    })
                    .child(
                        icon_button("skills-catalog-refresh", Icon::Reload, t)
                            .when(refreshing, |d| d.opacity(0.45))
                            .tooltip(move |_, cx| {
                                cx.new(|_| {
                                    crate::view::dotify_ui::Tip::new("Refresh skills.json", t)
                                })
                                .into()
                            })
                            .tooltip_show_delay(crate::view::tooltip::delay())
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                if !this.skills.catalog_refreshing {
                                    this.load_catalog(true, cx);
                                }
                            })),
                    )
            }
        };

        const TAB_W: f32 = 104.0;
        const TAB_GAP: f32 = 12.0;
        let underline_x = match mode {
            SkillsViewMode::Installed => 0.0,
            SkillsViewMode::Discover => TAB_W + TAB_GAP,
        };

        let make_tab = |id: &'static str,
                        label: &'static str,
                        count: Option<usize>,
                        target: SkillsViewMode| {
            let active = mode == target;
            div()
                .id(id)
                .w(zpx(TAB_W))
                .pb(zpx(10.))
                .pt(zpx(4.))
                .cursor_pointer()
                .flex()
                .flex_row()
                .items_center()
                .justify_center()
                .gap(zpx(6.))
                .t_body()
                .font_weight(if active {
                    gpui::FontWeight::MEDIUM
                } else {
                    gpui::FontWeight::NORMAL
                })
                .text_color(c(if active { t.text } else { t.text_muted }))
                .when(!active, |d| d.hover(|s| s.text_color(c(t.text))))
                .active(|s| s.opacity(0.8))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    if this.skills.view_mode == target {
                        return;
                    }
                    let forward = target == SkillsViewMode::Discover;
                    this.skills.view_mode = target;
                    this.skills.creator_menu_open = false;
                    this.skills.category_menu_open = false;
                    this.skills.notice = None;
                    this.skills.tab_nav_gen = this.skills.tab_nav_gen.wrapping_add(1);
                    this.skills.tab_nav_forward = forward;
                    this.skills.tab_nav_at = Some(std::time::Instant::now());
                    if target == SkillsViewMode::Discover {
                        this.load_catalog(false, cx);
                    } else {
                        this.load_skills(cx);
                    }
                    cx.notify();
                }))
                .child(label)
                .when_some(count, |d, n| {
                    d.child(
                        div()
                            .font_family(MONO)
                            .t_caption()
                            .text_color(c(if active { t.text_muted } else { t.text_faint }))
                            .child(n.to_string()),
                    )
                })
        };

        // Critically damped spring underline bar (Emil / Apple Fluid Interfaces).
        let spring_underline = div()
            .absolute()
            .bottom(zpx(-1.))
            .w(zpx(TAB_W))
            .h(zpx(2.))
            .rounded_full()
            .bg(c(t.accent))
            .with_spring(
                "skills-tab-underline",
                SpringAnimation::new(super::controls::PILL_SPRING).to(zpx(underline_x)),
                |el, x| el.left(x),
            );

        let active_stat = match (enabled_count, installed_count) {
            (Some(en), Some(tot)) if mode == SkillsViewMode::Installed => {
                format!("{en} of {tot} active")
            }
            _ => String::new(),
        };

        div()
            .flex()
            .flex_col()
            .gap(zpx(12.))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_end()
                    .justify_between()
                    .gap(zpx(16.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(zpx(3.))
                            .child(
                                div()
                                    .text_size(zpx(22.))
                                    .line_height(zpx(26.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(c(t.text))
                                    .child("Skills"),
                            )
                            .child(
                                div()
                                    .t_small()
                                    .text_color(c(t.text_muted))
                                    .child("Reusable workflows, domain rules, and capabilities for your agent."),
                            ),
                    )
                    .child(right_actions),
            )
            .child(
                div()
                    .w_full()
                    .border_b_1()
                    .border_color(c(t.border))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .relative()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(zpx(TAB_GAP))
                            .child(make_tab(
                                "skills-tab-installed",
                                "Installed",
                                installed_count,
                                SkillsViewMode::Installed,
                            ))
                            .child(make_tab(
                                "skills-tab-discover",
                                "Discover",
                                catalog_count,
                                SkillsViewMode::Discover,
                            ))
                            .child(spring_underline),
                    )
                    .when(!active_stat.is_empty(), |d| {
                        d.child(
                            div()
                                .pb(zpx(8.))
                                .t_caption()
                                .text_color(c(t.text_faint))
                                .child(active_stat),
                        )
                    }),
            )
    }

    fn render_discover_body(
        &mut self,
        mut body: gpui::Div,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let query = self.skills_filter.read(cx).value().to_string();
        let needle = query.trim().to_lowercase();
        let catalog = self.skills.catalog_rows.clone();

        if let Some(notice) = self.skills.notice.clone() {
            body = body.child(
                div()
                    .pb(zpx(4.))
                    .t_small()
                    .text_color(c(t.danger))
                    .child(notice),
            );
        } else if let Some(warning) = self.skills.catalog_warning.clone() {
            body = body.child(
                div()
                    .pb(zpx(4.))
                    .t_small()
                    .text_color(c(t.text_muted))
                    .child(format!("Showing cached catalog ({warning})")),
            );
        }

        let all_entries = match catalog {
            None => {
                return body.child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .t_small()
                        .text_color(c(t.text_muted))
                        .child("Loading skill catalog…"),
                );
            }
            Some(Err(err)) => {
                return body.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(zpx(8.))
                        .py(zpx(8.))
                        .child(
                            div()
                                .t_body()
                                .text_color(c(t.text))
                                .child("Couldn't load the skills catalog."),
                        )
                        .child(div().t_small().text_color(c(t.danger)).child(err))
                        .child(
                            button("skills-catalog-retry", "Retry", ButtonKind::Ghost, true, t)
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.load_catalog(true, cx);
                                })),
                        ),
                );
            }
            Some(Ok(entries)) => entries,
        };

        let mut pill_counts: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        let mut author_counts: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        for s in &all_entries {
            let key = if s.category.is_empty() {
                "Other".to_string()
            } else {
                s.category.clone()
            };
            *pill_counts.entry(key).or_insert(0) += 1;
            let auth = if s.author.is_empty() {
                "Other".to_string()
            } else {
                s.author.clone()
            };
            *author_counts.entry(auth).or_insert(0) += 1;
        }
        let mut pills: Vec<(String, usize)> = pill_counts.into_iter().collect();
        pills.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let mut authors: Vec<(String, usize)> = author_counts.into_iter().collect();
        authors.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

        body = body.child(self.render_discover_controls(&pills, &authors, t, cx));

        let category_filter = self.skills.category_filter.clone();
        let catalog_author_filter = self.skills.catalog_author_filter.clone();
        let matched: Vec<CatalogSkillRow> = all_entries
            .into_iter()
            .filter(|row| {
                if let Some(ref selected_auth) = catalog_author_filter {
                    let auth = if row.author.is_empty() {
                        "Other"
                    } else {
                        row.author.as_str()
                    };
                    if auth != selected_auth {
                        return false;
                    }
                }
                if let Some(ref selected) = category_filter {
                    if selected == "__trending__" {
                        if !row.trending {
                            return false;
                        }
                    } else {
                        let key = if row.category.is_empty() {
                            "Other"
                        } else {
                            row.category.as_str()
                        };
                        if key != selected {
                            return false;
                        }
                    }
                }
                if !needle.is_empty() {
                    let matches_name = row.name.to_lowercase().contains(&needle);
                    let matches_desc = row.description.to_lowercase().contains(&needle);
                    let matches_auth = row.author.to_lowercase().contains(&needle);
                    let matches_cat = row.category.to_lowercase().contains(&needle);
                    let matches_repo = row.repo.to_lowercase().contains(&needle);
                    if !matches_name
                        && !matches_desc
                        && !matches_auth
                        && !matches_cat
                        && !matches_repo
                    {
                        return false;
                    }
                }
                true
            })
            .collect();

        if matched.is_empty() {
            return body.child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .py(zpx(32.))
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .gap(zpx(12.))
                            .child(Icon::Search.el(zpx(24.)).text_color(c(t.text_faint)))
                            .child(
                                div()
                                    .t_body()
                                    .text_color(c(t.text_muted))
                                    .child("No catalog skills match your search."),
                            )
                            .when(category_filter.is_some() || catalog_author_filter.is_some(), |d| {
                                d.child(
                                    button(
                                        "skills-discover-reset-filters",
                                        "Reset filters",
                                        ButtonKind::Ghost,
                                        true,
                                        t,
                                    )
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.skills.category_filter = None;
                                        this.skills.catalog_author_filter = None;
                                        this.skills.catalog_scroll.set_offset(gpui::point(
                                            gpui::px(0.),
                                            gpui::px(0.),
                                        ));
                                        cx.notify();
                                    })),
                                )
                            }),
                    ),
            );
        }

        let weak = cx.entity().downgrade();
        let installing = self.skills.installing.clone();
        let install_errors = self.skills.install_errors.clone();
        let avatars = self.skills.catalog_avatars.clone();

        let render_action = |skill: &CatalogSkillRow| -> AnyElement {
            let id = skill.id.clone();
            let is_installing = installing.contains(&id);
            let weak_install = weak.clone();
            let skill_for_install = skill.clone();

            if is_installing {
                button(
                    format!("catalog-installing-{id}"),
                    "Installing…",
                    ButtonKind::Secondary,
                    false,
                    t,
                )
                .into_any_element()
            } else if skill.update_available {
                button(
                    format!("catalog-update-{id}"),
                    "Update",
                    ButtonKind::Primary,
                    true,
                    t,
                )
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    let s = skill_for_install.clone();
                    let _ = weak_install.update(cx, |this, cx| {
                        this.install_catalog_skill(s, true, cx);
                    });
                })
                .into_any_element()
            } else if skill.installed {
                div()
                    .h(zpx(crate::style::CONTROL_H))
                    .px(zpx(10.))
                    .rounded(r_md())
                    .border_1()
                    .border_color(c(t.border))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(5.))
                    .t_small()
                    .text_color(c(t.text_muted))
                    .child(Icon::Check.el(zpx(12.)).text_color(c(t.accent)))
                    .child("Installed")
                    .into_any_element()
            } else {
                button(
                    format!("catalog-install-{id}"),
                    "Install",
                    ButtonKind::Secondary,
                    true,
                    t,
                )
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    let s = skill_for_install.clone();
                    let _ = weak_install.update(cx, |this, cx| {
                        this.install_catalog_skill(s, false, cx);
                    });
                })
                .into_any_element()
            }
        };

        let mut sections = div().flex().flex_col().gap(zpx(24.)).pb(zpx(24.));

        // Asymmetric editorial spotlight for Trending: 1 featured lead card on
        // the left paired with a tight stack of trending items on the right.
        let show_trending_spotlight =
            category_filter.is_none() && catalog_author_filter.is_none() && needle.is_empty();
        if show_trending_spotlight {
            let trending_items: Vec<CatalogSkillRow> =
                matched.iter().filter(|s| s.trending).cloned().collect();
            if let Some((lead, rest)) = trending_items.split_first() {
                let lead_id = lead.id.clone();
                let lead_name = lead.name.clone();
                let lead_repo = lead.repo.clone();
                let lead_meta = match (lead.author.is_empty(), lead.category.is_empty()) {
                    (false, false) => format!("{} · {}", lead.author, lead.category),
                    (false, true) => lead.author.clone(),
                    (true, false) => lead.category.clone(),
                    (true, true) => String::new(),
                };
                let lead_desc = lead.description.clone();
                let weak_lead = weak.clone();
                let lead_for_modal = lead.clone();

                let lead_cat_color = category_color(&lead.category, t);
                let lead_tint_bg = gpui::hsla(lead_cat_color.h, lead_cat_color.s, lead_cat_color.l, 0.12);
                let lead_tint_border = gpui::hsla(lead_cat_color.h, lead_cat_color.s, lead_cat_color.l, 0.30);
                let lead_cmd = format!("/{}", lead.name);
                let lead_avatar = render_creator_avatar(&lead.author, &lead.repo, &avatars, 18.0);

                // Bespoke layered mini composer graphic showing how the skill runs in Keel.
                let mini_graphic = div()
                    .w_full()
                    .rounded(r_md())
                    .bg(c(t.window))
                    .border_1()
                    .border_color(c(t.border))
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .px(zpx(10.))
                            .py(zpx(6.))
                            .border_b_1()
                            .border_color(c(t.border))
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(zpx(5.))
                                    .child(div().size(zpx(6.)).rounded_full().bg(c(t.border)))
                                    .child(div().size(zpx(6.)).rounded_full().bg(c(t.border)))
                                    .child(div().size(zpx(6.)).rounded_full().bg(c(t.border))),
                            )
                            .child(
                                div()
                                    .font_family(MONO)
                                    .t_caption()
                                    .text_color(c(t.text_faint))
                                    .child("SKILL.md"),
                            ),
                    )
                    .child(
                        div()
                            .px(zpx(12.))
                            .py(zpx(10.))
                            .flex()
                            .flex_col()
                            .gap(zpx(8.))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(zpx(6.))
                                    .child(
                                        div()
                                            .px(zpx(7.))
                                            .py(zpx(2.))
                                            .rounded(r_sm())
                                            .bg(lead_tint_bg)
                                            .border_1()
                                            .border_color(lead_tint_border)
                                            .font_family(MONO)
                                            .t_caption()
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .text_color(lead_cat_color)
                                            .child(lead_cmd),
                                    )
                                    .child(
                                        div()
                                            .t_caption()
                                            .text_color(c(t.text_faint))
                                            .child("Invoke in any chat"),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(zpx(5.))
                                    .child(
                                        div()
                                            .w(gpui::relative(0.82))
                                            .h(zpx(5.))
                                            .rounded_full()
                                            .bg(c(t.input)),
                                    )
                                    .child(
                                        div()
                                            .w(gpui::relative(0.56))
                                            .h(zpx(5.))
                                            .rounded_full()
                                            .bg(c(t.input)),
                                    ),
                            ),
                    );

                let featured_card = div()
                    .id(ElementId::Name(format!("trending-lead-{lead_id}").into()))
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .p(zpx(20.))
                    .rounded(r_lg())
                    .bg(c(t.input))
                    .border_1()
                    .border_color(c(t.border))
                    .hover_fade(c(t.hover), None)
                    .active(|s| s.opacity(0.88))
                    .cursor_pointer()
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .justify_between()
                    .gap(zpx(14.))
                    .on_click(move |_, _, cx| {
                        let s = lead_for_modal.clone();
                        let _ = weak_lead.update(cx, |this, cx| {
                            this.open_catalog_modal(&s, cx);
                        });
                    })
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(zpx(10.))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .justify_between()
                                    .gap(zpx(8.))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .gap(zpx(6.))
                                            .px(zpx(8.))
                                            .h(zpx(22.))
                                            .rounded_full()
                                            .bg(lead_tint_bg)
                                            .border_1()
                                            .border_color(lead_tint_border)
                                            .child(
                                                Icon::ChartLine
                                                    .el(zpx(11.))
                                                    .text_color(lead_cat_color),
                                            )
                                            .child(
                                                div()
                                                    .t_caption()
                                                    .font_weight(gpui::FontWeight::MEDIUM)
                                                    .text_color(lead_cat_color)
                                                    .child("Featured"),
                                            ),
                                    )
                                    .when(!lead_repo.is_empty(), |d| {
                                        d.child(
                                            div()
                                                .font_family(MONO)
                                                .t_caption()
                                                .text_color(c(t.text_faint))
                                                .child(lead_repo),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(zpx(3.))
                                    .child(
                                        div()
                                            .text_size(zpx(18.))
                                            .line_height(zpx(23.))
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .text_color(c(t.text))
                                            .child(lead_name),
                                    )
                                    .when(!lead_meta.is_empty(), |d| {
                                        d.child(
                                            div()
                                                .flex()
                                                .flex_row()
                                                .items_center()
                                                .gap(zpx(6.))
                                                .children(lead_avatar)
                                                .child(
                                                    div()
                                                        .t_caption()
                                                        .text_color(c(t.text_muted))
                                                        .child(lead_meta),
                                                ),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .t_small()
                                    .text_color(c(t.text_muted))
                                    .child(lead_desc),
                            )
                            .child(mini_graphic),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .gap(zpx(12.))
                            .child(
                                div()
                                    .t_caption()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(c(t.text_muted))
                                    .child("Inspect SKILL.md →"),
                            )
                            .child(render_action(lead)),
                    );

                let mut stack_card = div()
                    .flex_1()
                    .min_w_0()
                    .rounded(r_lg())
                    .border_1()
                    .border_color(c(t.border))
                    .overflow_hidden()
                    .flex()
                    .flex_col();

                for (ix, skill) in rest.iter().take(4).enumerate() {
                    let id = skill.id.clone();
                    let name = skill.name.clone();
                    let meta = match (skill.author.is_empty(), skill.category.is_empty()) {
                        (false, false) => format!("{} · {}", skill.author, skill.category),
                        (false, true) => skill.author.clone(),
                        (true, false) => skill.category.clone(),
                        (true, true) => String::new(),
                    };
                    let desc = skill.description.clone();
                    let stack_avatar = render_creator_avatar(&skill.author, &skill.repo, &avatars, 16.0);
                    let weak_item = weak.clone();
                    let skill_for_modal = skill.clone();

                    stack_card = stack_card.child(
                        div()
                            .id(ElementId::Name(format!("trending-stack-{id}").into()))
                            .flex_1()
                            .w_full()
                            .px(zpx(14.))
                            .py(zpx(10.))
                            .when(ix > 0, |d| d.border_t_1().border_color(c(t.border)))
                            .hover_fade(c(t.hover), None)
                            .cursor_pointer()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .gap(zpx(12.))
                            .on_click(move |_, _, cx| {
                                let s = skill_for_modal.clone();
                                let _ = weak_item.update(cx, |this, cx| {
                                    this.open_catalog_modal(&s, cx);
                                });
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .flex()
                                    .flex_col()
                                    .gap(zpx(3.))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .gap(zpx(6.))
                                            .min_w_0()
                                            .overflow_hidden()
                                            .children(stack_avatar)
                                            .child(
                                                div()
                                                    .flex_none()
                                                    .t_body()
                                                    .font_weight(gpui::FontWeight::MEDIUM)
                                                    .text_color(c(t.text))
                                                    .child(name),
                                            )
                                            .when(!meta.is_empty(), |d| {
                                                d.child(
                                                    div()
                                                        .min_w_0()
                                                        .truncate()
                                                        .t_caption()
                                                        .text_color(c(t.text_faint))
                                                        .child(meta),
                                                )
                                            }),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .t_small()
                                            .text_color(c(t.text_muted))
                                            .child(desc),
                                    ),
                            )
                            .child(render_action(skill)),
                    );
                }

                let spotlight = div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_stretch()
                    .gap(zpx(12.))
                    .child(featured_card)
                    .when(!rest.is_empty(), |d| d.child(stack_card));

                sections = sections.child(
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap(zpx(10.))
                        .child(render_section_header(
                            "Trending",
                            Some(Icon::ChartLine),
                            Some(category_color("trending", t)),
                            trending_items.len(),
                            t,
                        ))
                        .child(spotlight),
                );
            }
        }

        // Categorized sections by usage.
        let mut group_map: std::collections::BTreeMap<String, Vec<CatalogSkillRow>> =
            std::collections::BTreeMap::new();
        for skill in matched {
            let key = if skill.category.is_empty() {
                "Other".to_string()
            } else {
                skill.category.clone()
            };
            group_map.entry(key).or_default().push(skill);
        }
        let mut groups: Vec<(String, Vec<CatalogSkillRow>)> = group_map.into_iter().collect();
        groups.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));

        for (group_name, items) in groups {
            let group_count = items.len();
            // Sleek 2-column bento card grid per usage category.
            let mut grid = div().w_full().flex().flex_col().gap(zpx(8.));
            for pair in items.chunks(2) {
                let mut row_el = div().w_full().flex().flex_row().gap(zpx(8.));
                for skill in pair {
                    let id = skill.id.clone();
                    let name = skill.name.clone();
                    let description = skill.description.clone();
                    let error = install_errors.get(&id).cloned();
                    let author = skill.author.clone();
                    let repo = skill.repo.clone();

                    let weak_row = weak.clone();
                    let skill_for_modal = skill.clone();

                    let card_avatar = render_creator_avatar(&author, &repo, &avatars, 18.0);

                    let card = div()
                        .id(ElementId::Name(format!("catalog-card-{id}").into()))
                        .flex_1()
                        .min_w_0()
                        .p(zpx(16.))
                        .rounded(r_lg())
                        .border_1()
                        .border_color(c(t.border))
                        .hover_fade(c(t.hover), None)
                        .active(|s| s.opacity(0.84))
                        .cursor_pointer()
                        .flex()
                        .flex_col()
                        .justify_between()
                        .gap(zpx(10.))
                        .on_click(move |_, _, cx| {
                            let s = skill_for_modal.clone();
                            let _ = weak_row.update(cx, |this, cx| {
                                this.open_catalog_modal(&s, cx);
                            });
                        })
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_start()
                                .justify_between()
                                .gap(zpx(10.))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .gap(zpx(4.))
                                        .child(
                                            div()
                                                .min_w_0()
                                                .truncate()
                                                .t_body()
                                                .font_weight(gpui::FontWeight::MEDIUM)
                                                .text_color(c(t.text))
                                                .child(name),
                                        )
                                        .when(!author.is_empty() || !repo.is_empty(), |d| {
                                            d.child(
                                                div()
                                                    .flex()
                                                    .flex_row()
                                                    .items_center()
                                                    .gap(zpx(6.))
                                                    .min_w_0()
                                                    .overflow_hidden()
                                                    .children(card_avatar)
                                                    .child(
                                                        div()
                                                            .min_w_0()
                                                            .truncate()
                                                            .t_caption()
                                                            .text_color(c(t.text_faint))
                                                            .child(if !author.is_empty()
                                                                && !repo.is_empty()
                                                            {
                                                                format!("{author} · {repo}")
                                                            } else if !author.is_empty() {
                                                                author
                                                            } else {
                                                                repo
                                                            }),
                                                    ),
                                            )
                                        }),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap(zpx(6.))
                                        .when(error.is_some(), |d| {
                                            d.child(
                                                div()
                                                    .px(zpx(6.))
                                                    .py(zpx(2.))
                                                    .rounded(r_sm())
                                                    .t_caption()
                                                    .text_color(c(t.danger))
                                                    .child("Failed"),
                                            )
                                        })
                                        .child(render_action(skill)),
                                ),
                        )
                        .child(
                            div()
                                .t_small()
                                .text_color(c(t.text_muted))
                                .child(description),
                        );

                    row_el = row_el.child(card);
                }
                if pair.len() == 1 {
                    row_el = row_el.child(div().flex_1().min_w_0());
                }
                grid = grid.child(row_el);
            }

            let group_color = category_color(&group_name, t);
            sections = sections.child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(zpx(10.))
                    .child(render_section_header(
                        &group_name,
                        Some(category_icon(&group_name)),
                        Some(group_color),
                        group_count,
                        t,
                    ))
                    .child(grid),
            );
        }

        let filter_epoch = self.skills.filter_epoch as usize;
        let filter_anim = self.recent_skill_filter_nav();
        let filter_sign = if self.skills.filter_forward { 1.0 } else { -1.0 };
        let scroll_list = div()
            .id("skills-catalog-scroll")
            .track_scroll(&self.skills.catalog_scroll)
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .pr(zpx(4.))
            .with_animation(
                ("skills-catalog-filter-list", filter_epoch),
                gpui::Animation::new(std::time::Duration::from_millis(160))
                    .with_easing(crate::motion::ease_out_strong),
                move |el, delta| {
                    if filter_anim {
                        el.relative()
                            .left(zpx(filter_sign * 12.0 * (1.0 - delta)))
                            .opacity(delta)
                    } else {
                        el
                    }
                },
            )
            .child(sections);

        body.child(scroll_list)
    }

    fn render_discover_controls(
        &mut self,
        pills: &[(String, usize)],
        authors: &[(String, usize)],
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let active_filter = self.skills.category_filter.clone();

        let top_row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(8.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h(zpx(INPUT_H))
                    .px(zpx(8.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(8.))
                    .rounded(r_md())
                    .border_1()
                    .border_color(c(t.border))
                    .t_body()
                    .text_color(c(t.text_muted))
                    .child(Icon::Search.el(zpx(14.)).text_color(c(t.text_faint)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(self.skills_filter.clone().into_any_element()),
                    ),
            )
            .when(authors.len() > 1, |d| {
                d.child(self.render_discover_creator_menu(authors, t, cx))
            });

        // Ordered tab keys for directional content glide (left vs right).
        let mut ordered_keys: Vec<Option<String>> = vec![None, Some("__trending__".into())];
        for (label, _) in pills {
            ordered_keys.push(Some(label.clone()));
        }
        let current_ix = ordered_keys
            .iter()
            .position(|k| *k == active_filter)
            .unwrap_or(0);

        let active_key = active_filter
            .clone()
            .unwrap_or_else(|| "__all__".to_string());
        let active_tint: Option<gpui::Hsla> = match active_filter.as_deref() {
            None => None,
            Some("__trending__") => Some(category_color("trending", t)),
            Some(cat) => Some(category_color(cat, t)),
        };

        // Measure the active chip's bounds relative to the strip so one shared
        // spring pill glides smoothly in X, Y, and width across variable-width tabs.
        let strip_origin = self.skills.category_strip_origin.clone();
        let chip_bounds_map = self.skills.category_chip_bounds.clone();
        let measured_pill = strip_origin.get().and_then(|origin| {
            chip_bounds_map.borrow().get(&active_key).map(|b| {
                (
                    b.origin.x - origin.x,
                    b.origin.y - origin.y,
                    b.size.width,
                    b.size.height,
                )
            })
        });
        let has_spring_pill = measured_pill.is_some();

        let spring_pill = measured_pill.map(|(tx, ty, tw, th)| {
            let fill = active_tint
                .map(|col| gpui::hsla(col.h, col.s, col.l, 0.14))
                .unwrap_or_else(|| c(t.selected));
            let edge = active_tint
                .map(|col| gpui::hsla(col.h, col.s, col.l, 0.38))
                .unwrap_or_else(|| c(t.border));

            div()
                .absolute()
                .top_0()
                .left_0()
                .child(
                    div()
                        .relative()
                        .with_spring(
                            "discover-cat-pill-x",
                            SpringAnimation::new(super::controls::PILL_SPRING).to(tx),
                            |el, x| el.left(x),
                        )
                        .child(
                            div()
                                .relative()
                                .with_spring(
                                    "discover-cat-pill-y",
                                    SpringAnimation::new(super::controls::PILL_SPRING).to(ty),
                                    |el, y| el.top(y),
                                )
                                .child(
                                    div()
                                        .h(th)
                                        .rounded_full()
                                        .bg(fill)
                                        .border_1()
                                        .border_color(edge)
                                        .with_spring(
                                            "discover-cat-pill-w",
                                            SpringAnimation::new(super::controls::PILL_SPRING)
                                                .to(tw),
                                            |el, w| el.w(w),
                                        ),
                                ),
                        ),
                )
        });

        // Tactile category filter pills: generous 32px height, 13px tinted icons, and counts.
        let make_chip = |id: String,
                         key: String,
                         ix: usize,
                         label: String,
                         count: Option<usize>,
                         value: Option<String>,
                         icon: Option<Icon>,
                         tint: Option<gpui::Hsla>,
                         selected: bool| {
            let fallback_bg = tint
                .map(|col| gpui::hsla(col.h, col.s, col.l, 0.14))
                .unwrap_or_else(|| c(t.selected));
            let fallback_border = tint
                .map(|col| gpui::hsla(col.h, col.s, col.l, 0.36))
                .unwrap_or_else(|| c(t.border));
            let icon_color = if selected {
                tint.unwrap_or_else(|| c(t.text))
            } else {
                tint.map(|col| gpui::hsla(col.h, col.s * 0.85, col.l, 0.78))
                    .unwrap_or_else(|| c(t.text_faint))
            };
            let bounds_ref = chip_bounds_map.clone();
            let key_for_measure = key.clone();

            div()
                .id(ElementId::Name(id.into()))
                .relative()
                .h(zpx(32.))
                .px(zpx(12.))
                .rounded_full()
                .border_1()
                .border_color(if selected && !has_spring_pill {
                    fallback_border
                } else if selected {
                    gpui::transparent_black()
                } else {
                    c(t.border)
                })
                .when(selected && !has_spring_pill, |d| d.bg(fallback_bg))
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(6.))
                .cursor_pointer()
                .t_small()
                .font_weight(if selected {
                    gpui::FontWeight::MEDIUM
                } else {
                    gpui::FontWeight::NORMAL
                })
                .text_color(c(if selected { t.text } else { t.text_muted }))
                .hover_fade_when(!selected, c(t.hover), None)
                .active(|s| s.opacity(0.8))
                .child(
                    gpui::canvas(
                        move |bounds, _, _| {
                            bounds_ref.borrow_mut().insert(key_for_measure.clone(), bounds);
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .inset_0(),
                )
                .when_some(icon, |d, ic| {
                    d.child(ic.el(zpx(13.)).text_color(icon_color))
                })
                .child(label)
                .when_some(count, |d, n| {
                    d.child(
                        div()
                            .font_family(MONO)
                            .t_caption()
                            .text_color(c(if selected { t.text_muted } else { t.text_faint }))
                            .child(n.to_string()),
                    )
                })
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    if this.skills.category_filter == value {
                        return;
                    }
                    this.skills.filter_forward = ix >= current_ix;
                    this.skills.category_filter = value.clone();
                    this.skills.filter_epoch = this.skills.filter_epoch.wrapping_add(1);
                    this.skills.filter_at = Some(std::time::Instant::now());
                    this.skills
                        .catalog_scroll
                        .set_offset(gpui::point(gpui::px(0.), gpui::px(0.)));
                    cx.notify();
                }))
        };

        let strip_origin_probe = strip_origin.clone();
        let mut chip_row = div()
            .relative()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(zpx(6.))
            .child(
                gpui::canvas(
                    move |bounds, _, _| {
                        strip_origin_probe.set(Some(bounds.origin));
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .children(spring_pill)
            .child(make_chip(
                "discover-pill-all".into(),
                "__all__".into(),
                0,
                "All".into(),
                None,
                None,
                None,
                None,
                active_filter.is_none(),
            ))
            .child(make_chip(
                "discover-pill-trending".into(),
                "__trending__".into(),
                1,
                "Trending".into(),
                None,
                Some("__trending__".into()),
                Some(Icon::ChartLine),
                Some(category_color("trending", t)),
                active_filter.as_deref() == Some("__trending__"),
            ));

        for (i, (label, count)) in pills.iter().enumerate() {
            let selected = active_filter.as_deref() == Some(label.as_str());
            chip_row = chip_row.child(make_chip(
                format!("discover-pill-{label}"),
                label.clone(),
                i + 2,
                label.clone(),
                Some(*count),
                Some(label.clone()),
                Some(category_icon(label)),
                Some(category_color(label, t)),
                selected,
            ));
        }

        div()
            .flex()
            .flex_col()
            .gap(zpx(10.))
            .pb(zpx(4.))
            .child(top_row)
            .child(chip_row)
    }

    /// Creator filter dropdown for Discover, matching Installed's Filter trigger.
    fn render_discover_creator_menu(
        &mut self,
        authors: &[(String, usize)],
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        const MENU_ID: &str = "discover-creator-menu";
        let author_filter = self.skills.catalog_author_filter.clone();
        let open = self.skills.catalog_filter_menu_open;
        let bounds_cell = self.skills.catalog_filter_btn_bounds.clone();
        let has_active = author_filter.is_some();
        let label = author_filter
            .clone()
            .unwrap_or_else(|| "Filter".to_string());

        let menu = open.then(|| {
            let total: usize = authors.iter().map(|(_, n)| n).sum();
            let pick_author = |this: &mut Self, value: Option<String>, cx: &mut Context<Self>| {
                this.skills.catalog_author_filter = value;
                this.skills.catalog_filter_menu_open = false;
                this.skills.filter_epoch = this.skills.filter_epoch.wrapping_add(1);
                this.skills.filter_at = Some(std::time::Instant::now());
                this.skills
                    .catalog_scroll
                    .set_offset(gpui::point(gpui::px(0.), gpui::px(0.)));
                cx.notify();
            };

            let mut items = vec![
                div()
                    .px(zpx(8.))
                    .pt(zpx(6.))
                    .pb(zpx(2.))
                    .t_caption()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(c(t.text_faint))
                    .child("Creator")
                    .into_any_element(),
                super::super::menu_item(
                    "discover-creator-all",
                    t,
                    true,
                    author_filter.is_none(),
                )
                .child(div().flex_1().child("All creators"))
                .child(
                    div()
                        .flex_none()
                        .text_color(c(t.text_faint))
                        .child(total.to_string()),
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    pick_author(this, None, cx)
                }))
                .into_any_element(),
            ];

            items.extend(authors.iter().map(|(author, count)| {
                let value = author.clone();
                super::super::menu_item(
                    format!("discover-creator-{author}"),
                    t,
                    true,
                    author_filter.as_deref() == Some(author.as_str()),
                )
                .child(div().flex_1().min_w_0().truncate().child(author.clone()))
                .child(
                    div()
                        .flex_none()
                        .text_color(c(t.text_faint))
                        .child(count.to_string()),
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    pick_author(this, Some(value.clone()), cx)
                }))
                .into_any_element()
            }));

            let dismiss = cx.listener(|this, _: &gpui::MouseDownEvent, _, cx| {
                this.note_menu_dismissed(MENU_ID);
                this.skills.catalog_filter_menu_open = false;
                cx.notify();
            });
            let at = self
                .skills
                .catalog_filter_btn_bounds
                .get()
                .map(|b| gpui::point(b.right(), b.bottom()));
            super::super::floating_menu_positioned(
                MENU_ID,
                t,
                220.,
                gpui::Anchor::TopRight,
                at,
                false,
                None,
                self.reduced(),
                dismiss,
                items,
            )
        });

        div()
            .relative()
            .flex_none()
            .child(
                gpui::canvas(move |bounds, _, _| bounds_cell.set(Some(bounds)), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .children(menu)
            .child(
                div()
                    .id("discover-creator-btn")
                    .h(zpx(INPUT_H))
                    .max_w(zpx(200.))
                    .pl(zpx(12.))
                    .pr(zpx(10.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(6.))
                    .rounded(r_md())
                    .border_1()
                    .border_color(c(t.border))
                    .t_small()
                    .font_weight(if has_active {
                        gpui::FontWeight::MEDIUM
                    } else {
                        gpui::FontWeight::NORMAL
                    })
                    .text_color(c(if has_active { t.text } else { t.text_muted }))
                    .cursor_pointer()
                    .when(open || has_active, |d| d.bg(c(t.selected)))
                    .hover_fade_when(!open && !has_active, c(t.hover), None)
                    .active(|s| s.opacity(0.8))
                    .child(div().min_w_0().truncate().child(label))
                    .child(Icon::ChevronDown.el(zpx(12.)).text_color(c(t.text_faint)))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        if this.menu_just_dismissed(MENU_ID) {
                            return;
                        }
                        this.skills.catalog_filter_menu_open =
                            !this.skills.catalog_filter_menu_open;
                        cx.notify();
                    })),
            )
    }

    fn render_catalog_skill_detail_page(
        &mut self,
        skill: CatalogSkillRow,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let id = skill.id.clone();
        let name = skill.name.clone();
        let author = skill.author.clone();
        let category = skill.category.clone();
        let repo = skill.repo.clone();
        let source_url = skill.source_url.clone();
        let description = skill.description.clone();
        let installed = skill.installed;
        let update_available = skill.update_available;
        let is_installing = self.skills.installing.contains(&id);
        let install_error = self.skills.install_errors.get(&id).cloned();
        let avatars = self.skills.catalog_avatars.clone();
        let cat_color = category_color(&category, t);
        let cat_bg = gpui::hsla(cat_color.h, cat_color.s, cat_color.l, 0.12);
        let cat_border = gpui::hsla(cat_color.h, cat_color.s, cat_color.l, 0.30);
        let creator_avatar = render_creator_avatar(&author, &repo, &avatars, 18.0);
        let slash_cmd = format!("/{name}");

        let readme_loading = self.skills.catalog_readme_loading.contains(&id);
        let readme_state = self.skills.catalog_readmes.get(&id).cloned();

        let skill_for_install = skill.clone();
        let primary_action: AnyElement = if is_installing {
            button(
                "catalog-detail-installing",
                "Installing…",
                ButtonKind::Primary,
                false,
                t,
            )
            .into_any_element()
        } else if update_available {
            button(
                "catalog-detail-update",
                "Update skill",
                ButtonKind::Primary,
                true,
                t,
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.install_catalog_skill(skill_for_install.clone(), true, cx);
            }))
            .into_any_element()
        } else if installed {
            div()
                .h(zpx(crate::style::CONTROL_H))
                .px(zpx(10.))
                .rounded(r_md())
                .border_1()
                .border_color(c(t.border))
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(5.))
                .t_small()
                .text_color(c(t.text_muted))
                .child(Icon::Check.el(zpx(12.)).text_color(c(t.accent)))
                .child("Installed")
                .into_any_element()
        } else {
            button(
                "catalog-detail-install",
                "Install skill",
                ButtonKind::Primary,
                true,
                t,
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.install_catalog_skill(skill_for_install.clone(), false, cx);
            }))
            .into_any_element()
        };

        // Top navigation bar: Back button on left, actions on right.
        let nav_bar = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(zpx(12.))
            .pb(zpx(12.))
            .border_b_1()
            .border_color(c(t.border))
            .child(
                div()
                    .id("catalog-detail-back")
                    .h(zpx(crate::style::CONTROL_H))
                    .pl(zpx(6.))
                    .pr(zpx(10.))
                    .rounded(r_md())
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(6.))
                    .cursor_pointer()
                    .t_small()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(c(t.text_muted))
                    .hover(|s| s.text_color(c(t.text)))
                    .hover_fade(c(t.hover), None)
                    .active(|s| s.opacity(0.8))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.dismiss_catalog_modal(cx);
                    }))
                    .child(Icon::Back.el(zpx(13.)))
                    .child("Discover"),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(8.))
                    .when(!source_url.is_empty(), |d| {
                        let url = source_url.clone();
                        d.child(
                            button(
                                "catalog-detail-source",
                                "View source ↗",
                                ButtonKind::Ghost,
                                true,
                                t,
                            )
                            .on_click(cx.listener(move |_, _: &ClickEvent, _, _| {
                                open_browser_url(&url);
                            })),
                        )
                    })
                    .child(primary_action),
            );

        // Hero card: matches Discover's clean flat surface, tinted category pill,
        // real creator logo, and inset /command bar.
        let hero = div()
            .p(zpx(20.))
            .rounded(r_lg())
            .bg(c(t.input))
            .border_1()
            .border_color(c(t.border))
            .flex()
            .flex_col()
            .gap(zpx(14.))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap(zpx(12.))
                    .flex_wrap()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(zpx(8.))
                            .when(!category.is_empty(), |d| {
                                d.child(
                                    div()
                                        .h(zpx(24.))
                                        .px(zpx(10.))
                                        .rounded_full()
                                        .bg(cat_bg)
                                        .border_1()
                                        .border_color(cat_border)
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap(zpx(5.))
                                        .child(
                                            category_icon(&category)
                                                .el(zpx(11.))
                                                .text_color(cat_color),
                                        )
                                        .child(
                                            div()
                                                .t_caption()
                                                .font_weight(gpui::FontWeight::MEDIUM)
                                                .text_color(cat_color)
                                                .child(category),
                                        ),
                                )
                            })
                            .when(!author.is_empty(), |d| {
                                d.child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap(zpx(6.))
                                        .children(creator_avatar)
                                        .child(
                                            div()
                                                .t_small()
                                                .text_color(c(t.text_muted))
                                                .child(author),
                                        ),
                                )
                            }),
                    )
                    .when(!repo.is_empty(), |d| {
                        d.child(
                            div()
                                .font_family(MONO)
                                .t_caption()
                                .text_color(c(t.text_faint))
                                .child(repo),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(zpx(6.))
                    .child(
                        div()
                            .text_size(zpx(24.))
                            .line_height(zpx(28.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(c(t.text))
                            .child(name),
                    )
                    .child(
                        div()
                            .t_body()
                            .text_color(c(t.text_muted))
                            .child(description),
                    ),
            )
            .child(
                div()
                    .px(zpx(12.))
                    .py(zpx(9.))
                    .rounded(r_md())
                    .bg(c(t.window))
                    .border_1()
                    .border_color(c(t.border))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap(zpx(12.))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(zpx(8.))
                            .child(
                                div()
                                    .px(zpx(8.))
                                    .py(zpx(2.))
                                    .rounded(r_sm())
                                    .bg(cat_bg)
                                    .border_1()
                                    .border_color(cat_border)
                                    .font_family(MONO)
                                    .t_caption()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(cat_color)
                                    .child(slash_cmd),
                            )
                            .child(
                                div()
                                    .t_caption()
                                    .text_color(c(t.text_faint))
                                    .child("Invoke directly in any chat"),
                            ),
                    )
                    .child(
                        div()
                            .t_caption()
                            .text_color(c(if installed { t.accent } else { t.text_faint }))
                            .child(if installed {
                                "Installed"
                            } else {
                                "Not installed"
                            }),
                    ),
            )
            .when_some(install_error, |d, err| {
                d.child(div().t_small().text_color(c(t.danger)).child(err))
            });

        let readme_el: AnyElement = match readme_state {
            Some(Ok(text)) => render_skill_instructions(&text, t).into_any_element(),
            Some(Err(err)) => {
                let skill_retry = skill.clone();
                div()
                    .py(zpx(8.))
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap(zpx(8.))
                    .child(
                        div()
                            .t_small()
                            .text_color(c(t.text_muted))
                            .child(format!("Couldn't load SKILL.md preview: {err}")),
                    )
                    .child(
                        button(
                            "catalog-readme-retry",
                            "Retry preview",
                            ButtonKind::Ghost,
                            true,
                            t,
                        )
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.skills.catalog_readmes.remove(&skill_retry.id);
                            this.fetch_catalog_readme(&skill_retry, cx);
                        })),
                    )
                    .into_any_element()
            }
            None if readme_loading => div()
                .py(zpx(12.))
                .t_small()
                .text_color(c(t.text_faint))
                .child("Loading SKILL.md…")
                .into_any_element(),
            None => div().into_any_element(),
        };

        let doc_section = div()
            .flex()
            .flex_col()
            .gap(zpx(12.))
            .child(render_detail_doc_header(cat_color, t))
            .child(
                div()
                    .p(zpx(22.))
                    .rounded(r_lg())
                    .border_1()
                    .border_color(c(t.border))
                    .child(readme_el),
            );

        let scroll_body = div()
            .id("catalog-detail-scroll")
            .track_scroll(&self.skills.modal_scroll)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .pt(zpx(16.))
            .pb(zpx(24.))
            .pr(zpx(4.))
            .flex()
            .flex_col()
            .gap(zpx(22.))
            .child(hero)
            .child(doc_section);

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(nav_bar)
            .child(scroll_body)
    }

    /// Uncluttered Installed controls: Search + one merged Filter menu (Status + Creator),
    /// with spring-gliding Usage category pills below (matching Discover).
    fn render_skill_controls(
        &mut self,
        authors: &[(String, usize)],
        usage_pills: &[(String, usize)],
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let active_usage = self.skills.installed_usage_filter.clone();

        let top_row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(zpx(8.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h(zpx(INPUT_H))
                    .px(zpx(8.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(8.))
                    .rounded(r_md())
                    .border_1()
                    .border_color(c(t.border))
                    .t_body()
                    .text_color(c(t.text_muted))
                    .child(Icon::Search.el(zpx(14.)).text_color(c(t.text_faint)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(self.skills_filter.clone().into_any_element()),
                    ),
            )
            .child(self.render_creator_menu(authors, t, cx));

        let mut ordered_keys: Vec<Option<String>> = vec![None];
        for (label, _) in usage_pills {
            ordered_keys.push(Some(label.clone()));
        }
        let current_ix = ordered_keys
            .iter()
            .position(|k| *k == active_usage)
            .unwrap_or(0);
        let active_key = active_usage
            .clone()
            .unwrap_or_else(|| "__all__".to_string());
        let active_tint: Option<gpui::Hsla> = active_usage
            .as_deref()
            .map(|cat| category_color(cat, t));

        let strip_origin = self.skills.installed_strip_origin.clone();
        let chip_bounds_map = self.skills.installed_chip_bounds.clone();
        let measured_pill = strip_origin.get().and_then(|origin| {
            chip_bounds_map.borrow().get(&active_key).map(|b| {
                (
                    b.origin.x - origin.x,
                    b.origin.y - origin.y,
                    b.size.width,
                    b.size.height,
                )
            })
        });
        let has_spring_pill = measured_pill.is_some();

        let spring_pill = measured_pill.map(|(tx, ty, tw, th)| {
            let fill = active_tint
                .map(|col| gpui::hsla(col.h, col.s, col.l, 0.14))
                .unwrap_or_else(|| c(t.selected));
            let edge = active_tint
                .map(|col| gpui::hsla(col.h, col.s, col.l, 0.38))
                .unwrap_or_else(|| c(t.border));

            div()
                .absolute()
                .top_0()
                .left_0()
                .child(
                    div()
                        .relative()
                        .with_spring(
                            "installed-cat-pill-x",
                            SpringAnimation::new(super::controls::PILL_SPRING).to(tx),
                            |el, x| el.left(x),
                        )
                        .child(
                            div()
                                .relative()
                                .with_spring(
                                    "installed-cat-pill-y",
                                    SpringAnimation::new(super::controls::PILL_SPRING).to(ty),
                                    |el, y| el.top(y),
                                )
                                .child(
                                    div()
                                        .h(th)
                                        .rounded_full()
                                        .bg(fill)
                                        .border_1()
                                        .border_color(edge)
                                        .with_spring(
                                            "installed-cat-pill-w",
                                            SpringAnimation::new(super::controls::PILL_SPRING)
                                                .to(tw),
                                            |el, w| el.w(w),
                                        ),
                                ),
                        ),
                )
        });

        let make_chip = |id: String,
                         key: String,
                         ix: usize,
                         label: String,
                         count: Option<usize>,
                         value: Option<String>,
                         icon: Option<Icon>,
                         tint: Option<gpui::Hsla>,
                         selected: bool| {
            let fallback_bg = tint
                .map(|col| gpui::hsla(col.h, col.s, col.l, 0.14))
                .unwrap_or_else(|| c(t.selected));
            let fallback_border = tint
                .map(|col| gpui::hsla(col.h, col.s, col.l, 0.36))
                .unwrap_or_else(|| c(t.border));
            let icon_color = if selected {
                tint.unwrap_or_else(|| c(t.text))
            } else {
                tint.map(|col| gpui::hsla(col.h, col.s * 0.85, col.l, 0.78))
                    .unwrap_or_else(|| c(t.text_faint))
            };
            let bounds_ref = chip_bounds_map.clone();
            let key_for_measure = key.clone();

            div()
                .id(ElementId::Name(id.into()))
                .relative()
                .h(zpx(32.))
                .px(zpx(12.))
                .rounded_full()
                .border_1()
                .border_color(if selected && !has_spring_pill {
                    fallback_border
                } else if selected {
                    gpui::transparent_black()
                } else {
                    c(t.border)
                })
                .when(selected && !has_spring_pill, |d| d.bg(fallback_bg))
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(6.))
                .cursor_pointer()
                .t_small()
                .font_weight(if selected {
                    gpui::FontWeight::MEDIUM
                } else {
                    gpui::FontWeight::NORMAL
                })
                .text_color(c(if selected { t.text } else { t.text_muted }))
                .hover_fade_when(!selected, c(t.hover), None)
                .active(|s| s.opacity(0.8))
                .child(
                    gpui::canvas(
                        move |bounds, _, _| {
                            bounds_ref.borrow_mut().insert(key_for_measure.clone(), bounds);
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .inset_0(),
                )
                .when_some(icon, |d, ic| {
                    d.child(ic.el(zpx(13.)).text_color(icon_color))
                })
                .child(label)
                .when_some(count, |d, n| {
                    d.child(
                        div()
                            .font_family(MONO)
                            .t_caption()
                            .text_color(c(if selected { t.text_muted } else { t.text_faint }))
                            .child(n.to_string()),
                    )
                })
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    if this.skills.installed_usage_filter == value {
                        return;
                    }
                    this.skills.filter_forward = ix >= current_ix;
                    this.skills.installed_usage_filter = value.clone();
                    this.skills.filter_epoch = this.skills.filter_epoch.wrapping_add(1);
                    this.skills.filter_at = Some(std::time::Instant::now());
                    this.skills
                        .installed_scroll
                        .set_offset(gpui::point(gpui::px(0.), gpui::px(0.)));
                    cx.notify();
                }))
        };

        let strip_origin_probe = strip_origin.clone();
        let mut chip_row = div()
            .relative()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(zpx(6.))
            .child(
                gpui::canvas(
                    move |bounds, _, _| {
                        strip_origin_probe.set(Some(bounds.origin));
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .children(spring_pill)
            .child(make_chip(
                "installed-pill-all".into(),
                "__all__".into(),
                0,
                "All".into(),
                None,
                None,
                None,
                None,
                active_usage.is_none(),
            ));

        for (i, (label, count)) in usage_pills.iter().enumerate() {
            let selected = active_usage.as_deref() == Some(label.as_str());
            chip_row = chip_row.child(make_chip(
                format!("installed-pill-{label}"),
                label.clone(),
                i + 1,
                label.clone(),
                Some(*count),
                Some(label.clone()),
                Some(category_icon(label)),
                Some(category_color(label, t)),
                selected,
            ));
        }

        div()
            .flex()
            .flex_col()
            .gap(zpx(10.))
            .pb(zpx(4.))
            .child(top_row)
            .when(usage_pills.len() > 1, |d| d.child(chip_row))
    }

    /// Merged Filter menu (Status + Creator) so Installed controls stay clean and minimal.
    fn render_creator_menu(
        &mut self,
        authors: &[(String, usize)],
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        const MENU_ID: &str = "skill-creator-menu";
        let author_filter = self.skills.author_filter.clone();
        let status_filter = self.skills.status_filter;
        let open = self.skills.creator_menu_open;
        let bounds_cell = self.skills.creator_btn_bounds.clone();

        let status_text = match status_filter {
            SkillStatusFilter::All => None,
            SkillStatusFilter::Enabled => Some("Enabled"),
            SkillStatusFilter::Disabled => Some("Disabled"),
        };
        let has_active = status_text.is_some() || author_filter.is_some();
        let label = match (status_text, author_filter.as_deref()) {
            (None, None) => "Filter".to_string(),
            (Some(s), None) => s.to_string(),
            (None, Some(a)) => a.to_string(),
            (Some(s), Some(a)) => format!("{s} · {a}"),
        };

        let menu = open.then(|| {
            let total: usize = authors.iter().map(|(_, n)| n).sum();
            let pick_status = |this: &mut Self, value: SkillStatusFilter, cx: &mut Context<Self>| {
                this.skills.status_filter = value;
                this.skills.creator_menu_open = false;
                this.skills.filter_epoch = this.skills.filter_epoch.wrapping_add(1);
                this.skills.filter_at = Some(std::time::Instant::now());
                cx.notify();
            };
            let pick_author = |this: &mut Self, value: Option<String>, cx: &mut Context<Self>| {
                this.skills.author_filter = value;
                this.skills.creator_menu_open = false;
                this.skills.filter_epoch = this.skills.filter_epoch.wrapping_add(1);
                this.skills.filter_at = Some(std::time::Instant::now());
                cx.notify();
            };

            let section_label = |text: &'static str| {
                div()
                    .px(zpx(8.))
                    .pt(zpx(6.))
                    .pb(zpx(2.))
                    .t_caption()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(c(t.text_faint))
                    .child(text)
                    .into_any_element()
            };

            let mut items = vec![
                section_label("Status"),
                super::super::menu_item(
                    "skill-status-all",
                    t,
                    true,
                    status_filter == SkillStatusFilter::All,
                )
                .child(div().flex_1().child("All statuses"))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    pick_status(this, SkillStatusFilter::All, cx)
                }))
                .into_any_element(),
                super::super::menu_item(
                    "skill-status-enabled",
                    t,
                    true,
                    status_filter == SkillStatusFilter::Enabled,
                )
                .child(div().flex_1().child("Enabled"))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    pick_status(this, SkillStatusFilter::Enabled, cx)
                }))
                .into_any_element(),
                super::super::menu_item(
                    "skill-status-disabled",
                    t,
                    true,
                    status_filter == SkillStatusFilter::Disabled,
                )
                .child(div().flex_1().child("Disabled"))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    pick_status(this, SkillStatusFilter::Disabled, cx)
                }))
                .into_any_element(),
            ];

            if authors.len() > 1 {
                items.push(
                    div()
                        .my(zpx(4.))
                        .h(zpx(1.))
                        .w_full()
                        .bg(c(t.border))
                        .into_any_element(),
                );
                items.push(section_label("Creator"));
                items.push(
                    super::super::menu_item("skill-creator-all", t, true, author_filter.is_none())
                        .child(div().flex_1().child("All creators"))
                        .child(
                            div()
                                .flex_none()
                                .text_color(c(t.text_faint))
                                .child(total.to_string()),
                        )
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            pick_author(this, None, cx)
                        }))
                        .into_any_element(),
                );
                items.extend(authors.iter().map(|(author, count)| {
                    let value = author.clone();
                    super::super::menu_item(
                        format!("skill-creator-{author}"),
                        t,
                        true,
                        author_filter.as_deref() == Some(author.as_str()),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(author.clone()))
                    .child(
                        div()
                            .flex_none()
                            .text_color(c(t.text_faint))
                            .child(count.to_string()),
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        pick_author(this, Some(value.clone()), cx)
                    }))
                    .into_any_element()
                }));
            }

            let dismiss = cx.listener(|this, _: &gpui::MouseDownEvent, _, cx| {
                this.note_menu_dismissed(MENU_ID);
                this.skills.creator_menu_open = false;
                cx.notify();
            });
            let at = self
                .skills
                .creator_btn_bounds
                .get()
                .map(|b| gpui::point(b.right(), b.bottom()));
            super::super::floating_menu_positioned(
                MENU_ID,
                t,
                220.,
                gpui::Anchor::TopRight,
                at,
                false,
                None,
                self.reduced(),
                dismiss,
                items,
            )
        });

        div()
            .relative()
            .flex_none()
            .child(
                gpui::canvas(move |bounds, _, _| bounds_cell.set(Some(bounds)), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .children(menu)
            .child(
                div()
                    .id("skill-creator-btn")
                    .h(zpx(INPUT_H))
                    .max_w(zpx(220.))
                    .pl(zpx(12.))
                    .pr(zpx(10.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(6.))
                    .rounded(r_md())
                    .border_1()
                    .border_color(c(t.border))
                    .t_small()
                    .font_weight(if has_active {
                        gpui::FontWeight::MEDIUM
                    } else {
                        gpui::FontWeight::NORMAL
                    })
                    .text_color(c(if has_active { t.text } else { t.text_muted }))
                    .cursor_pointer()
                    .when(open || has_active, |d| d.bg(c(t.selected)))
                    .hover_fade_when(!open && !has_active, c(t.hover), None)
                    .active(|s| s.opacity(0.8))
                    .child(div().min_w_0().truncate().child(label))
                    .child(Icon::ChevronDown.el(zpx(12.)).text_color(c(t.text_faint)))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        if this.menu_just_dismissed(MENU_ID) {
                            return;
                        }
                        this.skills.creator_menu_open = !this.skills.creator_menu_open;
                        cx.notify();
                    })),
            )
    }

    fn render_installed_skill_detail_page(
        &mut self,
        skill: SkillRow,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let name = skill.name.clone();
        let path = skill.path.clone();
        let scope = skill.scope.clone();
        let enabled = skill.enabled;
        let packaged = scope == "package";
        let author = installed_creator_label(&skill);
        let repo = installed_skill_repo(&skill).unwrap_or_default();
        let usage = infer_skill_usage(&skill);
        let usage_color = category_color(usage, t);
        let usage_bg = gpui::hsla(usage_color.h, usage_color.s, usage_color.l, 0.12);
        let usage_border = gpui::hsla(usage_color.h, usage_color.s, usage_color.l, 0.30);
        let avatars = self.skills.catalog_avatars.clone();
        let creator_avatar = render_creator_avatar(&author, &repo, &avatars, 18.0);
        let source_url = skill.source_url.clone();
        let slash_cmd = skill
            .insert
            .strip_prefix("/skill:")
            .map(|n| format!("/{n}"))
            .unwrap_or_else(|| skill.insert.clone());
        let description = skill.description.clone();
        let removable = skill.removable && skill.scope != "package" && skill.scope != "shared";

        let instructions = self
            .skills
            .expanded_content
            .get(&path)
            .cloned()
            .unwrap_or_else(|| {
                let file = std::path::Path::new(&path).join("SKILL.md");
                std::fs::read_to_string(&file).unwrap_or_default()
            });

        let path_finder = path.clone();
        let path_toggle = path.clone();
        let scope_toggle = scope.clone();

        let nav_bar = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(zpx(12.))
            .pb(zpx(12.))
            .border_b_1()
            .border_color(c(t.border))
            .child(
                div()
                    .id("skill-detail-back")
                    .h(zpx(crate::style::CONTROL_H))
                    .pl(zpx(6.))
                    .pr(zpx(10.))
                    .rounded(r_md())
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(6.))
                    .cursor_pointer()
                    .t_small()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(c(t.text_muted))
                    .hover(|s| s.text_color(c(t.text)))
                    .hover_fade(c(t.hover), None)
                    .active(|s| s.opacity(0.8))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.dismiss_skill_modal(cx);
                    }))
                    .child(Icon::Back.el(zpx(13.)))
                    .child("Installed"),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(8.))
                    .child({
                        let path_finder = path_finder.clone();
                        button(
                            "skill-detail-finder",
                            "Reveal in Finder",
                            ButtonKind::Ghost,
                            true,
                            t,
                        )
                        .on_click(cx.listener(move |_, _: &ClickEvent, _, _| {
                            reveal_in_finder(&path_finder);
                        }))
                    })
                    .when(!source_url.is_empty(), |d| {
                        let url = source_url.clone();
                        d.child(
                            button(
                                "skill-detail-source",
                                "View source ↗",
                                ButtonKind::Ghost,
                                true,
                                t,
                            )
                            .on_click(cx.listener(move |_, _: &ClickEvent, _, _| {
                                open_browser_url(&url);
                            })),
                        )
                    })
                    .when(removable, |d| {
                        let path_remove = path_finder.clone();
                        let name_remove = name.clone();
                        d.child(
                            button(
                                "skill-detail-remove",
                                "Remove",
                                ButtonKind::Danger,
                                true,
                                t,
                            )
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.dismiss_skill_modal(cx);
                                this.skills.confirm_gen += 1;
                                this.skills.confirm_leaving = None;
                                this.skills.confirm = Some(SkillConfirm::Remove {
                                    path: path_remove.clone(),
                                    name: name_remove.clone(),
                                });
                                cx.notify();
                            })),
                        )
                    })
                    .when(!packaged, |d| {
                        d.child(
                            toggle(format!("skill-detail-toggle-{path_toggle}"), enabled, t)
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.toggle_skill(
                                        path_toggle.clone(),
                                        !enabled,
                                        scope_toggle.clone(),
                                        cx,
                                    );
                                })),
                        )
                    }),
            );

        let hero = div()
            .p(zpx(20.))
            .rounded(r_lg())
            .bg(c(t.input))
            .border_1()
            .border_color(c(t.border))
            .flex()
            .flex_col()
            .gap(zpx(14.))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap(zpx(12.))
                    .flex_wrap()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(zpx(8.))
                            .child(
                                div()
                                    .h(zpx(24.))
                                    .px(zpx(10.))
                                    .rounded_full()
                                    .bg(usage_bg)
                                    .border_1()
                                    .border_color(usage_border)
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(zpx(5.))
                                    .child(category_icon(usage).el(zpx(11.)).text_color(usage_color))
                                    .child(
                                        div()
                                            .t_caption()
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .text_color(usage_color)
                                            .child(usage),
                                    ),
                            )
                            .when(!author.is_empty(), |d| {
                                d.child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap(zpx(6.))
                                        .children(creator_avatar)
                                        .child(
                                            div()
                                                .t_small()
                                                .text_color(c(t.text_muted))
                                                .child(author),
                                        ),
                                )
                            }),
                    )
                    .when(!repo.is_empty(), |d| {
                        d.child(
                            div()
                                .font_family(MONO)
                                .t_caption()
                                .text_color(c(t.text_faint))
                                .child(repo),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(zpx(6.))
                    .child(
                        div()
                            .text_size(zpx(24.))
                            .line_height(zpx(28.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(c(t.text))
                            .child(name),
                    )
                    .child(
                        div()
                            .t_body()
                            .text_color(c(t.text_muted))
                            .child(description),
                    ),
            )
            .child(
                div()
                    .px(zpx(12.))
                    .py(zpx(9.))
                    .rounded(r_md())
                    .bg(c(t.window))
                    .border_1()
                    .border_color(c(t.border))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap(zpx(12.))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(zpx(8.))
                            .child(
                                div()
                                    .px(zpx(8.))
                                    .py(zpx(2.))
                                    .rounded(r_sm())
                                    .bg(usage_bg)
                                    .border_1()
                                    .border_color(usage_border)
                                    .font_family(MONO)
                                    .t_caption()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(usage_color)
                                    .child(slash_cmd),
                            )
                            .child(
                                div()
                                    .t_caption()
                                    .text_color(c(t.text_faint))
                                    .child("Invoke directly in any chat"),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(zpx(6.))
                            .child(
                                div()
                                    .size(zpx(6.))
                                    .rounded_full()
                                    .bg(c(if enabled { t.accent } else { t.text_faint })),
                            )
                            .child(
                                div()
                                    .t_caption()
                                    .text_color(c(if enabled { t.text_muted } else { t.text_faint }))
                                    .child(if enabled { "Active" } else { "Disabled" }),
                            ),
                    ),
            );

        let doc_section = div()
            .flex()
            .flex_col()
            .gap(zpx(12.))
            .child(render_detail_doc_header(usage_color, t))
            .child(
                div()
                    .p(zpx(22.))
                    .rounded(r_lg())
                    .border_1()
                    .border_color(c(t.border))
                    .child(render_skill_instructions(&instructions, t)),
            );

        let scroll_body = div()
            .id("skill-detail-scroll")
            .track_scroll(&self.skills.modal_scroll)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .pt(zpx(16.))
            .pb(zpx(24.))
            .pr(zpx(4.))
            .flex()
            .flex_col()
            .gap(zpx(22.))
            .child(hero)
            .child(doc_section);

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(nav_bar)
            .child(scroll_body)
    }

    fn render_skill_confirm(
        &mut self,
        confirm: &SkillConfirm,
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let (text, action, danger) = match confirm {
            SkillConfirm::Replace { name, .. } => (
                format!("“{name}” is already in your skills. Replace the existing folder?"),
                "Replace",
                true,
            ),
            SkillConfirm::Remove { name, .. } => (
                format!("Remove “{name}”? This permanently deletes its folder from your skills."),
                "Remove",
                true,
            ),
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(zpx(12.))
            .p(zpx(12.))
            .mb(zpx(12.))
            .rounded(r_md())
            .border_1()
            .border_color(c(if danger { t.danger } else { t.border }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .t_small()
                    .text_color(c(if danger { t.danger } else { t.text }))
                    .child(text),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(zpx(8.))
                    .child(
                        button(
                            "skills-confirm-go",
                            action,
                            if danger {
                                ButtonKind::Danger
                            } else {
                                ButtonKind::Primary
                            },
                            true,
                            t,
                        )
                        .on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.confirm_skill(cx)),
                        ),
                    )
                    .child(
                        button(
                            "skills-confirm-cancel",
                            "Cancel",
                            ButtonKind::Ghost,
                            true,
                            t,
                        )
                        .on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.dismiss_skill_confirm(cx)
                            }),
                        ),
                    ),
            )
    }

    /// Cancel: the banner plays its exit, then goes.
    fn dismiss_skill_confirm(&mut self, cx: &mut Context<Self>) {
        let Some(confirm) = self.skills.confirm.take() else {
            return;
        };
        if !self.reduced() {
            self.skills.confirm_gen += 1;
            let n = self.skills.confirm_gen;
            self.skills.confirm_leaving = Some((n, confirm));
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(crate::motion::FAST + std::time::Duration::from_millis(20))
                    .await;
                let _ = this.update(cx, |this, cx| {
                    if this
                        .skills
                        .confirm_leaving
                        .as_ref()
                        .is_some_and(|(m, _)| *m == n)
                    {
                        this.skills.confirm_leaving = None;
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        cx.notify();
    }

    fn toggle_skill_expanded(&mut self, path: &str, cx: &mut Context<Self>) {
        if self.skills.modal_skill.as_ref().map(|s| s.path.as_str()) == Some(path) {
            self.dismiss_skill_modal(cx);
        } else if let Some(Ok(rows)) = self.skills.rows.as_ref() {
            if let Some(skill) = rows.iter().find(|r| r.path == path) {
                let skill_clone = skill.clone();
                self.open_skill_modal(&skill_clone, cx);
            }
        }
    }

    /// Debug socket: filter to the pack/author, or expand a skill by name.
    pub(in crate::view) fn dev_toggle_skill_pack(&mut self, label: &str, cx: &mut Context<Self>) {
        // Dev socket `skills memory` / `skills memory:project`: Settings → Memory.
        if let Some(tab) = match label {
            "memory" => Some(MemoryTab::You),
            "memory:project" => Some(MemoryTab::Project),
            _ => None,
        } {
            self.skills.open = false;
            self.skills.transition = None;
            self.open_memory(cx);
            self.memory.transition = None;
            self.memory.tab = tab;
            cx.notify();
            return;
        }
        if let Some(Ok(rows)) = self.skills.rows.as_ref() {
            if rows.iter().any(|r| {
                r.author.eq_ignore_ascii_case(label) || r.collection.eq_ignore_ascii_case(label)
            }) {
                self.skills.author_filter = if self.skills.author_filter.as_deref() == Some(label) {
                    None
                } else {
                    Some(label.to_string())
                };
                self.skills.filter_epoch = self.skills.filter_epoch.wrapping_add(1);
                cx.notify();
                return;
            }
            if let Some(skill) = rows.iter().find(|r| r.name.eq_ignore_ascii_case(label)) {
                let path = skill.path.clone();
                self.toggle_skill_expanded(&path, cx);
            }
        }
    }

    fn toggle_skill(&mut self, path: String, enabled: bool, scope: String, cx: &mut Context<Self>) {
        if let Some(Ok(rows)) = self.skills.rows.as_mut() {
            if let Some(row) = rows.iter_mut().find(|row| row.path == path) {
                row.enabled = enabled;
            }
        }
        self.skills.row_errors.remove(&path);
        let Some(client) = self.client.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let res = client
                .call(
                    "set_skill_enabled",
                    serde_json::json!({ "path": path, "enabled": enabled, "scope": scope, "cwd": serde_json::Value::Null }),
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                match res {
                    Ok(_) => {
                        this.skills.row_errors.remove(&path);
                        this.commands.clear();
                        this.load_skills(cx);
                    }
                    Err(_) => {
                        if let Some(Ok(rows)) = this.skills.rows.as_mut() {
                            if let Some(row) = rows.iter_mut().find(|row| row.path == path) {
                                row.enabled = !enabled;
                            }
                        }
                        this.skills.row_errors.insert(path, "Couldn't update settings.".into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn choose_skill_folder(&mut self, cx: &mut Context<Self>) {
        if self.pi_missing() {
            return;
        }
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add skill".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.skills.notice = None;
                this.skills.confirm = None;
                let source = path.to_string_lossy().into_owned();
                this.submit_add_skill(source, "user", false, cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn submit_add_skill(
        &mut self,
        source: String,
        dest: &str,
        replace: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let dest = dest.to_string();
        let folder = std::path::Path::new(&source)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        cx.spawn(async move |this, cx| {
            let res = client
                .call(
                    "add_skill",
                    serde_json::json!({ "source": source, "dest": dest, "cwd": serde_json::Value::Null, "replace": replace }),
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                match res {
                    Ok(_) => {
                        this.skills.confirm = None;
                        this.skills.notice = None;
                        this.skills.added = Some((folder, "user".to_string()));
                        this.commands.clear();
                        this.load_skills(cx);
                        this.load_catalog_cache_only(cx);
                    }
                    Err(err) => {
                        if let Some(name) = err.strip_prefix("exists:").map(str::to_string) {
                            this.skills.confirm_gen += 1;
                            this.skills.confirm_leaving = None;
                            this.skills.confirm = Some(SkillConfirm::Replace { source, dest, name });
                        } else {
                            this.skills.notice = Some(err);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn confirm_skill(&mut self, cx: &mut Context<Self>) {
        let Some(confirm) = self.skills.confirm.take() else {
            return;
        };
        match confirm {
            SkillConfirm::Replace { source, dest, .. } => {
                self.submit_add_skill(source, &dest, true, cx)
            }
            SkillConfirm::Remove { path, .. } => self.remove_skill(path, cx),
        }
    }

    fn remove_skill(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let res = client
                .call(
                    "remove_skill",
                    serde_json::json!({ "path": path, "cwd": serde_json::Value::Null }),
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                match res {
                    Ok(_) => {
                        this.skills.notice = None;
                        this.commands.clear();
                        this.load_skills(cx);
                        this.load_catalog_cache_only(cx);
                    }
                    Err(err) => this.skills.notice = Some(err),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

fn category_icon(category: &str) -> Icon {
    match category.trim().to_ascii_lowercase().as_str() {
        "trending" => Icon::ChartLine,
        "documents" => Icon::FileText,
        "design" | "design & motion" => Icon::PenLine,
        "frontend" => Icon::AppWindow,
        "workflow" => Icon::ListChecks,
        "engineering" => Icon::Code,
        "cloud" | "cloud & infra" => Icon::Globe,
        _ => Icon::BookOpen,
    }
}

/// Signature dark-mode accent hue per category (restrained, high-legibility).
fn category_color(category: &str, t: Tokens) -> gpui::Hsla {
    match category.trim().to_ascii_lowercase().as_str() {
        "trending" => gpui::hsla(38. / 360., 0.85, 0.62, 1.0),
        "design" | "design & motion" => gpui::hsla(262. / 360., 0.80, 0.72, 1.0),
        "engineering" => gpui::hsla(199. / 360., 0.86, 0.62, 1.0),
        "frontend" => gpui::hsla(156. / 360., 0.68, 0.56, 1.0),
        "workflow" => gpui::hsla(348. / 360., 0.82, 0.68, 1.0),
        "cloud" | "cloud & infra" => gpui::hsla(24. / 360., 0.88, 0.62, 1.0),
        "documents" => gpui::hsla(234. / 360., 0.80, 0.72, 1.0),
        _ => c(t.accent),
    }
}

/// Render the creator's cached GitHub logo/avatar when available, falling back
/// cleanly to a tinted single-letter circular badge.
fn render_creator_avatar(
    author: &str,
    repo: &str,
    avatars: &std::collections::HashMap<String, std::path::PathBuf>,
    size: f32,
) -> Option<AnyElement> {
    if let Some((owner, _)) = repo.split_once('/') {
        let key = owner.to_ascii_lowercase();
        if let Some(path) = avatars.get(&key) {
            if path.is_file() {
                return Some(
                    div()
                        .size(zpx(size))
                        .flex_none()
                        .rounded_full()
                        .overflow_hidden()
                        .child(
                            gpui::img(path.clone())
                                .size(zpx(size))
                                .object_fit(gpui::ObjectFit::Cover),
                        )
                        .into_any_element(),
                );
            }
        }
    }
    let initial = author
        .chars()
        .next()
        .or_else(|| repo.chars().next())
        .map(|ch| ch.to_ascii_uppercase().to_string())?;
    let tint = author_badge_color(if author.is_empty() { repo } else { author });
    let bg = gpui::hsla(tint.h, tint.s, tint.l, 0.14);
    Some(
        div()
            .size(zpx(size))
            .flex_none()
            .rounded_full()
            .bg(bg)
            .flex()
            .items_center()
            .justify_center()
            .text_size(zpx((size * 0.56).max(9.)))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(tint)
            .child(initial)
            .into_any_element(),
    )
}

/// Stable muted identity hue per creator initial badge.
fn author_badge_color(author: &str) -> gpui::Hsla {
    let mut hash: u32 = 2166136261;
    for b in author.bytes() {
        hash ^= b as u32;
        hash = hash.wrapping_mul(16777619);
    }
    let hues = [262., 199., 156., 38., 348., 24., 234., 178.];
    let h = hues[(hash as usize) % hues.len()] / 360.;
    gpui::hsla(h, 0.72, 0.68, 1.0)
}

fn render_detail_doc_header(accent: gpui::Hsla, t: Tokens) -> gpui::Div {
    let badge_bg = gpui::hsla(accent.h, accent.s, accent.l, 0.12);
    let badge_border = gpui::hsla(accent.h, accent.s, accent.l, 0.26);
    div()
        .w_full()
        .flex()
        .flex_row()
        .items_center()
        .gap(zpx(10.))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(8.))
                .child(
                    div()
                        .size(zpx(24.))
                        .rounded(r_sm())
                        .bg(badge_bg)
                        .border_1()
                        .border_color(badge_border)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(Icon::ScrollText.el(zpx(13.)).text_color(accent)),
                )
                .child(
                    div()
                        .t_body()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(c(t.text))
                        .child("Instructions"),
                ),
        )
        .child(div().flex_1().h(zpx(1.)).bg(c(t.border)).opacity(0.65))
        .child(
            div()
                .font_family(MONO)
                .t_caption()
                .text_color(c(t.text_faint))
                .child("SKILL.md"),
        )
}

/// Editorial section header: tinted icon emblem, semibold title, hairline rule, and mono count.
fn render_section_header(
    title: &str,
    icon: Option<Icon>,
    color: Option<gpui::Hsla>,
    count: usize,
    t: Tokens,
) -> gpui::Div {
    let badge_color = color.unwrap_or_else(|| c(t.text_muted));
    let badge_bg = gpui::hsla(badge_color.h, badge_color.s, badge_color.l, 0.12);
    let badge_border = gpui::hsla(badge_color.h, badge_color.s, badge_color.l, 0.26);

    div()
        .w_full()
        .flex()
        .flex_row()
        .items_center()
        .gap(zpx(10.))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(zpx(8.))
                .when_some(icon, |d, ic| {
                    d.child(
                        div()
                            .size(zpx(24.))
                            .rounded(r_sm())
                            .bg(badge_bg)
                            .border_1()
                            .border_color(badge_border)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(ic.el(zpx(13.)).text_color(badge_color)),
                    )
                })
                .child(
                    div()
                        .t_body()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(c(t.text))
                        .child(title.to_string()),
                ),
        )
        .child(div().flex_1().h(zpx(1.)).bg(c(t.border)).opacity(0.65))
        .child(
            div()
                .font_family(MONO)
                .t_caption()
                .text_color(c(t.text_faint))
                .child(count.to_string()),
        )
}

/// Extract `owner/repo` from an installed skill's `collection` or GitHub `source_url`
/// so we can fetch & display the real GitHub creator logo.
fn installed_skill_repo(skill: &SkillRow) -> Option<String> {
    if let Some((owner, repo)) = skill.collection.split_once('/') {
        if !owner.is_empty() && !repo.is_empty() && !owner.contains(' ') {
            return Some(format!("{owner}/{repo}"));
        }
    }
    if let Some(rest) = skill
        .source_url
        .strip_prefix("https://github.com/")
        .or_else(|| skill.source_url.strip_prefix("http://github.com/"))
    {
        let mut parts = rest.split('/');
        if let (Some(owner), Some(repo)) = (parts.next(), parts.next()) {
            let repo = repo.trim_end_matches(".git");
            if !owner.is_empty() && !repo.is_empty() {
                return Some(format!("{owner}/{repo}"));
            }
        }
    }
    None
}

fn installed_creator_label(skill: &SkillRow) -> String {
    if !skill.author.is_empty() {
        skill.author.clone()
    } else if !skill.collection.is_empty() {
        skill.collection.clone()
    } else {
        "Other".to_string()
    }
}

/// Infer a clean usage category for locally installed skills that don't carry
/// catalog metadata.
fn infer_skill_usage(skill: &SkillRow) -> &'static str {
    let hay = format!(
        "{} {} {}",
        skill.name.to_lowercase(),
        skill.description.to_lowercase(),
        skill.collection.to_lowercase()
    );
    if hay.contains("animat")
        || hay.contains("motion")
        || hay.contains("design")
        || hay.contains("gesture")
        || hay.contains("css")
        || hay.contains("taste")
        || hay.contains("apple")
        || hay.contains("ui ")
        || hay.contains("sonner")
    {
        "Design & Motion"
    } else if hay.contains("pdf")
        || hay.contains("docx")
        || hay.contains("xlsx")
        || hay.contains("pptx")
        || hay.contains("document")
        || hay.contains("spreadsheet")
    {
        "Documents"
    } else if hay.contains("cloudflare")
        || hay.contains("worker")
        || hay.contains("wrangler")
        || hay.contains("durable")
        || hay.contains("sandbox")
        || hay.contains("deploy")
    {
        "Cloud & Infra"
    } else if hay.contains("react")
        || hay.contains("next")
        || hay.contains("frontend")
        || hay.contains("web-perf")
        || hay.contains("lighthouse")
        || hay.contains("tailwind")
    {
        "Frontend"
    } else if hay.contains("plan")
        || hay.contains("brainstorm")
        || hay.contains("debug")
        || hay.contains("review")
        || hay.contains("workflow")
        || hay.contains("grill")
    {
        "Workflow"
    } else if hay.contains("test")
        || hay.contains("mcp")
        || hay.contains("cli")
        || hay.contains("sdk")
        || hay.contains("code")
    {
        "Engineering"
    } else {
        "General"
    }
}

fn skill_markdown_body(raw: &str) -> String {
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    if !raw.starts_with("---") {
        return raw.trim().to_string();
    }
    let rest = raw[3..].trim_start_matches(['\r', '\n']);
    if let Some(end) = rest.find("\n---") {
        rest[end + 4..].trim().to_string()
    } else {
        raw.trim().to_string()
    }
}

fn styled_inline(text: &str, spans: &[crate::markdown::InlineSpan], t: Tokens) -> gpui::StyledText {
    let mut highlights: Vec<(std::ops::Range<usize>, gpui::HighlightStyle)> = Vec::new();
    for span in spans {
        if span.range.start >= span.range.end || span.range.end > text.len() {
            continue;
        }
        let mut style = gpui::HighlightStyle::default();
        if span.style.bold {
            style.font_weight = Some(gpui::FontWeight::SEMIBOLD);
            style.color = Some(c(t.text));
        }
        if span.style.italic {
            style.font_style = Some(gpui::FontStyle::Italic);
        }
        if span.style.strike {
            style.strikethrough = Some(gpui::StrikethroughStyle {
                thickness: gpui::px(1.),
                color: Some(c(t.text_faint)),
            });
        }
        if span.style.code {
            style.color = Some(c(t.text));
            style.background_color = Some(c(t.input));
        }
        if span.link.is_some() {
            style.color = Some(c(t.accent));
            style.underline = Some(gpui::UnderlineStyle {
                thickness: gpui::px(1.),
                color: Some(c(t.accent)),
                wavy: false,
            });
        }
        highlights.push((span.range.clone(), style));
    }
    gpui::StyledText::new(text.to_string()).with_highlights(highlights)
}

fn render_skill_instructions(text: &str, t: Tokens) -> gpui::Div {
    let body = skill_markdown_body(text);
    if body.is_empty() {
        return div()
            .py(zpx(8.))
            .t_small()
            .text_color(c(t.text_faint))
            .child("No instructions found in SKILL.md.");
    }

    let blocks = crate::markdown::parse_markdown_blocks("skill-md", &body);
    let mut out = div().flex().flex_col().gap(zpx(10.)).pb(zpx(16.));

    for block in blocks {
        match block {
            crate::markdown::TranscriptBlock::Heading {
                level,
                content,
                spans,
                ..
            } => {
                let styled = styled_inline(&content, &spans, t);
                let heading = match level {
                    1 => div()
                        .pt(zpx(8.))
                        .pb(zpx(4.))
                        .border_b_1()
                        .border_color(c(t.border))
                        .t_title()
                        .text_color(c(t.text))
                        .child(styled),
                    2 => div()
                        .pt(zpx(6.))
                        .pb(zpx(3.))
                        .border_b_1()
                        .border_color(c(t.border))
                        .t_body()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(c(t.text))
                        .child(styled),
                    _ => div()
                        .pt(zpx(4.))
                        .t_small()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(c(t.text))
                        .child(styled),
                };
                out = out.child(heading);
            }
            crate::markdown::TranscriptBlock::Paragraph { content, spans, .. } => {
                out = out.child(
                    div()
                        .t_small()
                        .text_color(c(t.text_muted))
                        .child(styled_inline(&content, &spans, t)),
                );
            }
            crate::markdown::TranscriptBlock::Code {
                language, content, ..
            } => {
                let lang = language.filter(|l| !l.trim().is_empty());
                out = out.child(
                    div()
                        .my(zpx(2.))
                        .rounded(r_md())
                        .bg(c(t.input))
                        .border_1()
                        .border_color(c(t.border))
                        .overflow_hidden()
                        .flex()
                        .flex_col()
                        .when_some(lang, |d, l| {
                            d.child(
                                div()
                                    .px(zpx(10.))
                                    .py(zpx(4.))
                                    .border_b_1()
                                    .border_color(c(t.border))
                                    .t_caption()
                                    .font_family(MONO)
                                    .text_color(c(t.text_faint))
                                    .child(l),
                            )
                        })
                        .child(
                            div()
                                .p(zpx(10.))
                                .t_code()
                                .text_color(c(t.text))
                                .child(content),
                        ),
                );
            }
            crate::markdown::TranscriptBlock::Quote { content, spans, .. } => {
                out = out.child(
                    div()
                        .my(zpx(2.))
                        .pl(zpx(12.))
                        .py(zpx(4.))
                        .border_l_2()
                        .border_color(c(t.accent))
                        .t_small()
                        .text_color(c(t.text_muted))
                        .child(styled_inline(&content, &spans, t)),
                );
            }
            crate::markdown::TranscriptBlock::Rule { .. } => {
                out = out.child(div().my(zpx(4.)).h(zpx(1.)).w_full().bg(c(t.border)));
            }
            crate::markdown::TranscriptBlock::List { items, .. } => {
                let mut list_div = div().flex().flex_col().gap(zpx(5.));
                for item in items {
                    let indent = zpx((item.depth as f32) * 14.0 + 4.0);
                    let marker = match item.task {
                        Some(true) => "☑".to_string(),
                        Some(false) => "☐".to_string(),
                        None => item.marker,
                    };
                    list_div = list_div.child(
                        div()
                            .flex()
                            .flex_row()
                            .items_start()
                            .gap(zpx(8.))
                            .pl(indent)
                            .child(
                                div()
                                    .flex_none()
                                    .t_small()
                                    .text_color(c(t.text_faint))
                                    .child(marker),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .t_small()
                                    .text_color(c(t.text_muted))
                                    .child(styled_inline(&item.text, &item.spans, t)),
                            ),
                    );
                }
                out = out.child(list_div);
            }
            crate::markdown::TranscriptBlock::Table { header, rows, .. } => {
                let has_header = header.iter().any(|c| !c.text.trim().is_empty());
                let mut table = div()
                    .my(zpx(2.))
                    .w_full()
                    .rounded(r_md())
                    .border_1()
                    .border_color(c(t.border))
                    .overflow_hidden()
                    .flex()
                    .flex_col();

                if has_header {
                    let mut hrow = div()
                        .w_full()
                        .bg(c(t.input))
                        .flex()
                        .flex_row()
                        .items_stretch();
                    for (ix, cell) in header.iter().enumerate() {
                        hrow = hrow.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .px(zpx(10.))
                                .py(zpx(6.))
                                .when(ix > 0, |d| d.border_l_1().border_color(c(t.border)))
                                .t_small()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(c(t.text))
                                .child(styled_inline(&cell.text, &cell.spans, t)),
                        );
                    }
                    table = table.child(hrow);
                }

                for (rix, row) in rows.iter().enumerate() {
                    let mut rdiv = div()
                        .w_full()
                        .when(has_header || rix > 0, |d| {
                            d.border_t_1().border_color(c(t.border))
                        })
                        .flex()
                        .flex_row()
                        .items_stretch();
                    for (cix, cell) in row.iter().enumerate() {
                        rdiv = rdiv.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .px(zpx(10.))
                                .py(zpx(6.))
                                .when(cix > 0, |d| d.border_l_1().border_color(c(t.border)))
                                .t_small()
                                .text_color(c(t.text_muted))
                                .child(styled_inline(&cell.text, &cell.spans, t)),
                        );
                    }
                    table = table.child(rdiv);
                }
                out = out.child(table);
            }
            _ => {}
        }
    }
    out
}

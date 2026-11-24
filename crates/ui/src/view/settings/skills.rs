//! Settings → Skills: inline first-class tab for browsing, filtering,
//! toggling, adding, and inspecting agent skills.

use gpui::{
    AnimationExt, AnyElement, ClickEvent, Context, ElementId, IntoElement, div, prelude::*,
    uniform_list,
};
use keel_theme::Tokens;

use super::super::KeelWindow;
use super::super::sidebar::{filled_button, text_button};
use super::controls::{
    creator_pill, open_browser_url, reveal_in_finder, row_text, toggle,
};
use super::{MemoryTab, SkillConfirm, SkillRow, SkillStatusFilter};
use crate::hover_fade::HoverFadeExt;
use crate::icons::Icon;
use crate::style::{KeelStyled, MONO, RADIUS_MD, RADIUS_SM, c, zpx};

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
                    Ok(v) => Ok(v["skills"]
                        .as_array()
                        .map(|rows| {
                            rows.iter()
                                .map(SkillRow::parse)
                                .filter(|r| r.scope != "project")
                                .collect()
                        })
                        .unwrap_or_default()),
                    Err(err) => Err(err),
                });
                cx.notify();
            });
        })
        .detach();
    }

    pub(in crate::view) fn open_skills(&mut self, cx: &mut Context<Self>) {
        self.skills.open = true;
        self.skills.notice = None;
        self.skills.confirm = None;
        self.skills.added = None;
        self.skills.author_filter = None;
        self.skills.status_filter = SkillStatusFilter::All;
        self.skills.filter_epoch = 0;
        self.skills.expanded_skill = None;
        self.skills.modal_skill = None;
        self.skills.modal_leaving = None;
        self.skills
            .scroll
            .set_offset(gpui::point(gpui::px(0.), gpui::px(0.)));
        self.skills
            .list_scroll
            .scroll_to_item(0, gpui::ScrollStrategy::Top);
        self.select_settings_tab(super::SettingsTab::Skills, cx);
        self.load_skills(cx);
    }

    pub(in crate::view) fn open_skill_modal(&mut self, skill: &SkillRow, cx: &mut Context<Self>) {
        self.skills.modal_gen = self.skills.modal_gen.wrapping_add(1);
        self.skills.modal_skill = Some(skill.clone());
        self.skills.modal_leaving = None;
        self.skills.expanded_skill = Some(skill.path.clone());
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
        if let Some(skill) = self.skills.modal_skill.take() {
            self.skills.expanded_skill = None;
            if self.reduced() {
                self.skills.modal_leaving = None;
                cx.notify();
                return;
            }
            let generation = self.skills.modal_gen;
            self.skills.modal_leaving = Some((generation, skill));
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(crate::motion::FAST).await;
                let _ = this.update(cx, |this, cx| {
                    if this.skills.modal_leaving.as_ref().map(|(n, _)| *n) == Some(generation) {
                        this.skills.modal_leaving = None;
                        cx.notify();
                    }
                });
            })
            .detach();
            cx.notify();
        }
    }

    pub(super) fn render_skills_tab(&mut self, t: Tokens, cx: &mut Context<Self>) -> gpui::Div {
        self.render_skills_body(t, cx)
    }

    fn render_skills_body(&mut self, t: Tokens, cx: &mut Context<Self>) -> gpui::Div {
        let missing = self.pi_missing();
        let query = self.skills_filter.read(cx).value().to_string();
        let needle = query.trim().to_lowercase();
        let rows = self.skills.rows.clone();
        let reduced = self.reduced();
        let mut body = div().size_full().flex().flex_col().gap_3();

        if missing {
            return body.child(
                div()
                    .py_3()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .t_body()
                    .text_color(c(t.danger))
                    .child(Icon::Alert.el(zpx(14.)))
                    .child("Keel's bundled Pi is missing"),
            );
        }

        let all_skills = match rows {
            None => return body,
            Some(Err(err)) => {
                return body.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .py_2()
                        .child(
                            div()
                                .t_body()
                                .text_color(c(t.text))
                                .child("Couldn't read skills."),
                        )
                        .child(div().t_small().text_color(c(t.danger)).child(err))
                        .child(text_button("skills-retry", "Retry", t).on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.load_skills(cx)),
                        )),
                );
            }
            Some(Ok(skills)) => skills,
        };

        let total = all_skills.len();
        let enabled_count = all_skills.iter().filter(|r| r.enabled).count();

        body = body.child(self.render_skills_header(t, missing, total, enabled_count, cx));

        if let Some(notice) = self.skills.notice.clone() {
            body = body.child(div().pb_2().t_small().text_color(c(t.danger)).child(notice));
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

        // Collect author stats
        let mut author_counts: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        for s in &all_skills {
            let auth = if s.author.is_empty() {
                if !s.collection.is_empty() {
                    s.collection.clone()
                } else {
                    "Other".to_string()
                }
            } else {
                s.author.clone()
            };
            *author_counts.entry(auth).or_insert(0) += 1;
        }
        let mut authors: Vec<(String, usize)> = author_counts.into_iter().collect();
        authors.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

        body = body.child(self.render_skill_controls(&authors, t, cx));

        // Filtering
        let status_filter = self.skills.status_filter;
        let author_filter = self.skills.author_filter.clone();

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
                // Author filter
                if let Some(ref selected_auth) = author_filter {
                    let auth = if row.author.is_empty() {
                        if !row.collection.is_empty() {
                            &row.collection
                        } else {
                            "Other"
                        }
                    } else {
                        &row.author
                    };
                    if auth != selected_auth {
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
                || status_filter != SkillStatusFilter::All;
            let empty_box = if is_filtering {
                div()
                    .py_12()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .child(Icon::Search.el(zpx(24.)).text_color(c(t.text_faint)))
                    .child(
                        div()
                            .t_body()
                            .text_color(c(t.text_muted))
                            .child("No skills match your search or filter."),
                    )
                    .child(
                        text_button("skills-reset-filters", "Reset filters", t).on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.skills.author_filter = None;
                                this.skills.status_filter = SkillStatusFilter::All;
                                this.skills
                                    .list_scroll
                                    .scroll_to_item(0, gpui::ScrollStrategy::Top);
                                cx.notify();
                            }),
                        ),
                    )
            } else {
                div()
                    .py_12()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_2()
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

        let count = matched.len();
        let weak = cx.entity().downgrade();
        let switch_flips = self.switch_flips.clone();
        let row_errors = self.skills.row_errors.clone();

        const SKILL_ROW_H: f32 = 62.0;

        let list_el = uniform_list("skills-list", count, move |range, _window, _cx| {
            range
                .map(|ix| {
                    let skill = &matched[ix];
                    let path = skill.path.clone();
                    let name = skill.name.clone();
                    let author = skill.author.clone();
                    let insert = skill.insert.clone();
                    let description = skill.description.clone();
                    let scope = skill.scope.clone();
                    let enabled = skill.enabled;
                    let packaged = scope == "package";
                    let error = row_errors.get(&path).cloned();
                    let flip = switch_flips
                        .get(&format!("skill-{path}"))
                        .filter(|(_, at)| !reduced && at.elapsed() < crate::motion::BASE * 2)
                        .map(|(n, _)| *n);

                    let weak_row = weak.clone();
                    let skill_for_click = skill.clone();
                    let path_for_toggle = path.clone();
                    let scope_for_toggle = scope.clone();

                    let card = div()
                        .id(ElementId::Name(format!("skill-card-{path}").into()))
                        .h(zpx(56.))
                        .w_full()
                        .px_3p5()
                        .py_2()
                        .rounded(RADIUS_MD)
                        .bg(c(t.input).opacity(0.35))
                        .border_1()
                        .border_color(c(t.border).opacity(0.5))
                        .hover(|s| s.border_color(c(t.border)))
                        .hover_fade(c(t.hover), None)
                        .cursor_pointer()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .on_click(move |_, _, cx| {
                            let skill = skill_for_click.clone();
                            let _ = weak_row.update(cx, |this, cx| {
                                this.open_skill_modal(&skill, cx);
                            });
                        })
                        // Left: Puzzle Icon + Name/Author/Slug + 1-line Description
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_3()
                                .child(
                                    div()
                                        .size(zpx(32.))
                                        .flex_none()
                                        .rounded(RADIUS_SM)
                                        .bg(if enabled {
                                            c(t.accent).opacity(0.12)
                                        } else {
                                            c(t.input)
                                        })
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(Icon::Puzzle.el(zpx(15.)).text_color(c(
                                            if enabled { t.accent } else { t.text_faint },
                                        ))),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .flex()
                                        .flex_col()
                                        .justify_center()
                                        .gap(zpx(2.))
                                        .child(
                                            div()
                                                .flex()
                                                .flex_row()
                                                .items_center()
                                                .gap_2()
                                                .min_w_0()
                                                .overflow_hidden()
                                                .child(
                                                    div()
                                                        .flex_none()
                                                        .text_size(zpx(13.5))
                                                        .line_height(zpx(18.))
                                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                                        .text_color(c(t.text))
                                                        .child(name.clone()),
                                                )
                                                .when(!author.is_empty(), |d| {
                                                    d.child(
                                                        div()
                                                            .id(ElementId::Name(
                                                                format!("skill-row-author-{path}")
                                                                    .into(),
                                                            ))
                                                            .flex()
                                                            .flex_row()
                                                            .items_center()
                                                            .px_1p5()
                                                            .h(zpx(17.))
                                                            .rounded(RADIUS_SM)
                                                            .bg(c(t.window))
                                                            .border_1()
                                                            .border_color(c(t.border).opacity(0.5))
                                                            .child(
                                                                div()
                                                                    .text_size(zpx(11.))
                                                                    .font_weight(
                                                                        gpui::FontWeight::MEDIUM,
                                                                    )
                                                                    .text_color(c(t.text_muted))
                                                                    .child(format!("by {author}")),
                                                            ),
                                                    )
                                                })
                                                .child(
                                                    div()
                                                        .px_1p5()
                                                        .h(zpx(17.))
                                                        .flex()
                                                        .items_center()
                                                        .rounded(RADIUS_SM)
                                                        .bg(c(t.window))
                                                        .border_1()
                                                        .border_color(c(t.border).opacity(0.4))
                                                        .font_family(MONO)
                                                        .text_size(zpx(10.5))
                                                        .text_color(c(t.accent).opacity(0.9))
                                                        .child(
                                                            insert
                                                                .strip_prefix("/skill:")
                                                                .map(|n| format!("/{n}"))
                                                                .unwrap_or_else(|| insert.clone()),
                                                        ),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .min_w_0()
                                                .truncate()
                                                .text_size(zpx(12.))
                                                .line_height(zpx(16.))
                                                .text_color(c(t.text_muted))
                                                .child(description.clone()),
                                        ),
                                ),
                        )
                        // Right: Error dot (if any) + "Instructions" button + Toggle Switch
                        .child(
                            div()
                                .flex_none()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_3()
                                .when(error.is_some(), |d| {
                                    d.child(
                                        div()
                                            .px_1p5()
                                            .py_0p5()
                                            .rounded(RADIUS_SM)
                                            .bg(c(t.danger).opacity(0.12))
                                            .text_size(zpx(11.))
                                            .text_color(c(t.danger))
                                            .child("Error"),
                                    )
                                })
                                .child(
                                    div()
                                        .id(ElementId::Name(format!("skill-row-btn-{path}").into()))
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap_1p5()
                                        .px_2()
                                        .py_1()
                                        .rounded(RADIUS_SM)
                                        .bg(c(t.hover).opacity(0.5))
                                        .border_1()
                                        .border_color(c(t.border).opacity(0.4))
                                        .hover(|s| s.border_color(c(t.border)))
                                        .hover_fade(c(t.hover), None)
                                        .t_small()
                                        .text_color(c(t.text_muted))
                                        .child(
                                            Icon::ScrollText
                                                .el(zpx(12.))
                                                .text_color(c(t.text_faint)),
                                        )
                                        .child("Instructions"),
                                )
                                .child(if packaged {
                                    div()
                                        .t_small()
                                        .text_color(c(t.text_faint))
                                        .child("In package")
                                        .into_any_element()
                                } else {
                                    let weak_toggle = weak.clone();
                                    let path_t = path_for_toggle.clone();
                                    let scope_t = scope_for_toggle.clone();
                                    toggle(format!("skill-{path_t}"), enabled, flip, t)
                                        .on_click(move |_, _, cx| {
                                            cx.stop_propagation();
                                            let path = path_t.clone();
                                            let scope = scope_t.clone();
                                            let _ = weak_toggle.update(cx, |this, cx| {
                                                this.toggle_skill(path, !enabled, scope, cx);
                                            });
                                        })
                                        .into_any_element()
                                }),
                        );

                    div()
                        .id(("skill-row", ix))
                        .h(zpx(SKILL_ROW_H))
                        .w_full()
                        .pb(zpx(6.))
                        .child(card)
                        .into_any_element()
                })
                .collect()
        })
        .track_scroll(&self.skills.list_scroll)
        .size_full();

        let filter_epoch = self.skills.filter_epoch as usize;
        let animated_list = if reduced {
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(list_el)
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .with_animation(
                    ("skills-filter-list", filter_epoch),
                    gpui::Animation::new(crate::motion::BASE).with_easing(crate::motion::ease_out),
                    |el, delta| {
                        let ease = crate::motion::ease_out(delta);
                        el.opacity(0.15 + 0.85 * ease)
                            .relative()
                            .top(zpx(8. * (1.0 - ease)))
                    },
                )
                .child(list_el)
                .into_any_element()
        };

        body.child(animated_list)
    }

    fn render_skills_header(
        &mut self,
        t: Tokens,
        missing: bool,
        total: usize,
        enabled_count: usize,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .pb_1()
            .child(
                div()
                    .t_small()
                    .text_color(c(t.text_muted))
                    .child(format!("{enabled_count} of {total} active")),
            )
            .when(!missing, |d| {
                d.child(
                    filled_button("skills-add-btn", "+ Add skill…", t.accent, t).on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.choose_skill_folder(cx)
                        }),
                    ),
                )
            })
    }

    fn render_skill_controls(
        &mut self,
        authors: &[(String, usize)],
        t: Tokens,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let author_filter = self.skills.author_filter.clone();
        let status_filter = self.skills.status_filter;

        div()
            .flex()
            .flex_col()
            .gap_2p5()
            .pb_2()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    // Search box
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h(zpx(34.))
                            .px_2p5()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .rounded(RADIUS_MD)
                            .bg(c(t.input))
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
                    // Status segmented control
                    .child(self.render_segmented_control(
                        "skill-status",
                        &[
                            ("All", SkillStatusFilter::All),
                            ("Enabled", SkillStatusFilter::Enabled),
                            ("Disabled", SkillStatusFilter::Disabled),
                        ],
                        status_filter,
                        66.0,
                        t,
                        cx,
                        |this, mode, _| {
                            this.skills.status_filter = mode;
                            this.skills.filter_epoch = this.skills.filter_epoch.wrapping_add(1);
                        },
                    )),
            )
            // Author / Provenance pills
            .when(!authors.is_empty(), |d| {
                d.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .flex_wrap()
                        .gap_1p5()
                        .pb_1()
                        .child(
                            creator_pill(
                                "skill-author-all".into(),
                                "All Creators".into(),
                                author_filter.is_none(),
                                t,
                            )
                            .on_click(cx.listener(
                                |this, _: &ClickEvent, _, cx| {
                                    this.skills.author_filter = None;
                                    this.skills.filter_epoch =
                                        this.skills.filter_epoch.wrapping_add(1);
                                    cx.notify();
                                },
                            )),
                        )
                        .children(authors.iter().map(|(author, count)| {
                            let is_sel = author_filter.as_deref() == Some(author);
                            let author_val = author.clone();
                            creator_pill(
                                format!("skill-author-{author}"),
                                format!("{author} ({count})"),
                                is_sel,
                                t,
                            )
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                if this.skills.author_filter.as_deref() == Some(&author_val) {
                                    this.skills.author_filter = None;
                                } else {
                                    this.skills.author_filter = Some(author_val.clone());
                                }
                                this.skills.filter_epoch = this.skills.filter_epoch.wrapping_add(1);
                                cx.notify();
                            }))
                            .into_any_element()
                        })),
                )
            })
    }

    pub(super) fn render_skill_modal(&mut self, t: Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (skill, leaving, modal_gen) =
            match (&self.skills.modal_skill, &self.skills.modal_leaving) {
                (Some(s), _) => (s.clone(), false, self.skills.modal_gen),
                (None, Some((n, s))) => (s.clone(), true, *n),
                _ => return None,
            };

        let reduced = self.reduced();
        let name = skill.name.clone();
        let path = skill.path.clone();
        let author = skill.author.clone();
        let source_url = skill.source_url.clone();
        let insert = skill.insert.clone();
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

        let mut card = div()
            .id(("skill-instruction-modal-card", modal_gen as usize))
            .w(zpx(720.))
            .max_w(gpui::relative(0.92))
            .max_h(gpui::relative(0.85))
            .rounded(crate::style::RADIUS_LG)
            .bg(c(t.surface))
            .border_1()
            .border_color(c(t.border))
            .floating_surface(t)
            .flex()
            .flex_col()
            .overflow_hidden()
            .on_click(|_: &ClickEvent, _, cx| cx.stop_propagation());

        // Header
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap_3()
            .px_5()
            .py_3p5()
            .border_b_1()
            .border_color(c(t.border).opacity(0.5))
            .bg(c(t.window).opacity(0.5))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2p5()
                    .flex_wrap()
                    .child(
                        div()
                            .size(zpx(28.))
                            .rounded(RADIUS_SM)
                            .bg(c(t.accent).opacity(0.12))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(Icon::Puzzle.el(zpx(15.)).text_color(c(t.accent))),
                    )
                    .child(
                        div()
                            .text_size(zpx(16.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(c(t.text))
                            .child(name.clone()),
                    )
                    .when(!author.is_empty(), |d| {
                        d.child(
                            div()
                                .id(ElementId::Name(format!("skill-modal-author-{name}").into()))
                                .flex()
                                .flex_row()
                                .items_center()
                                .px_2()
                                .h(zpx(20.))
                                .rounded(RADIUS_SM)
                                .bg(c(t.window))
                                .border_1()
                                .border_color(c(t.border).opacity(0.5))
                                .child(
                                    div()
                                        .text_size(zpx(11.5))
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(c(t.text_muted))
                                        .child(format!("by {author}")),
                                ),
                        )
                    })
                    .child(
                        div()
                            .px_2()
                            .h(zpx(20.))
                            .flex()
                            .items_center()
                            .rounded(RADIUS_SM)
                            .bg(c(t.window))
                            .border_1()
                            .border_color(c(t.border).opacity(0.5))
                            .font_family(MONO)
                            .text_size(zpx(11.5))
                            .text_color(c(t.accent))
                            .child(
                                insert
                                    .strip_prefix("/skill:")
                                    .map(|n| format!("/{n}"))
                                    .unwrap_or_else(|| insert.clone()),
                            ),
                    ),
            )
            .child(
                div()
                    .id("skill-modal-close-x")
                    .size(zpx(26.))
                    .rounded(RADIUS_SM)
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .text_color(c(t.text_muted))
                    .hover(|s| s.text_color(c(t.text)))
                    .hover_fade(c(t.hover), None)
                    .active(|s| s.opacity(0.7))
                    .child(Icon::Close.el(zpx(14.)))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.dismiss_skill_modal(cx);
                    })),
            );
        card = card.child(header);

        // Body: Scrollable markdown content with description callout
        let body_content = div()
            .id("skill-modal-scroll")
            .track_scroll(&self.skills.modal_scroll)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_5()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .p_3()
                    .rounded(RADIUS_MD)
                    .bg(c(t.input).opacity(0.4))
                    .border_1()
                    .border_color(c(t.border).opacity(0.5))
                    .t_body()
                    .text_color(c(t.text))
                    .child(description),
            )
            .child(render_skill_instructions(&instructions, t));
        card = card.child(body_content);

        // Footer
        let path_finder = path.clone();
        let footer = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px_5()
            .py_3()
            .border_t_1()
            .border_color(c(t.border).opacity(0.5))
            .bg(c(t.window).opacity(0.4))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .child({
                        let path_finder = path_finder.clone();
                        row_text("skill-modal-finder", "Reveal in Finder", t.text_faint, t)
                            .on_click(cx.listener(move |_, _: &ClickEvent, _, _| {
                                reveal_in_finder(&path_finder);
                            }))
                    })
                    .when(!source_url.is_empty(), |d| {
                        let url = source_url.clone();
                        d.child(
                            row_text("skill-modal-source", "View source ↗", t.text_faint, t)
                                .on_click(cx.listener(move |_, _: &ClickEvent, _, _| {
                                    open_browser_url(&url);
                                })),
                        )
                    })
                    .when(removable, |d| {
                        let path_remove = path_finder.clone();
                        let name_remove = name.clone();
                        d.child(
                            row_text("skill-modal-remove", "Remove skill", t.danger, t).on_click(
                                cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.dismiss_skill_modal(cx);
                                    this.skills.confirm_gen += 1;
                                    this.skills.confirm_leaving = None;
                                    this.skills.confirm = Some(SkillConfirm::Remove {
                                        path: path_remove.clone(),
                                        name: name_remove.clone(),
                                    });
                                    cx.notify();
                                }),
                            ),
                        )
                    }),
            )
            .child(
                filled_button("skill-modal-done-btn", "Done", t.accent, t).on_click(cx.listener(
                    |this, _: &ClickEvent, _, cx| {
                        this.dismiss_skill_modal(cx);
                    },
                )),
            );
        card = card.child(footer);

        // Motion for the modal card
        let card_el = if leaving {
            crate::motion::dismiss(card, ("skill-modal-card-out", modal_gen as usize), reduced)
        } else {
            crate::motion::appear(
                card,
                ("skill-modal-card-in", modal_gen as usize),
                crate::motion::BASE,
                14.,
                reduced,
            )
            .into_any_element()
        };

        // Scrim
        let scrim = div()
            .id(("skill-modal-scrim", modal_gen as usize))
            .absolute()
            .inset_0()
            .occlude()
            .bg(gpui::rgba(0x00000080))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.dismiss_skill_modal(cx);
            }))
            .child(card_el);

        let scrim = if reduced {
            scrim.into_any_element()
        } else if leaving {
            scrim
                .with_animation(
                    ("skill-scrim-out", modal_gen as usize),
                    gpui::Animation::new(crate::motion::FAST).with_easing(crate::motion::ease_out),
                    |el, delta| el.opacity(1.0 - delta),
                )
                .into_any_element()
        } else {
            scrim
                .with_animation(
                    ("skill-scrim-in", modal_gen as usize),
                    gpui::Animation::new(crate::motion::BASE).with_easing(crate::motion::ease_out),
                    |el, delta| el.opacity(delta),
                )
                .into_any_element()
        };

        Some(gpui::deferred(scrim).with_priority(3).into_any_element())
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
            .gap_3()
            .p_3()
            .mb_3()
            .rounded(RADIUS_MD)
            .bg(c(if danger { t.danger } else { t.accent }).opacity(0.1))
            .border_1()
            .border_color(c(if danger { t.danger } else { t.accent }).opacity(0.3))
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
                    .gap_2()
                    .child(
                        row_text(
                            "skills-confirm-go",
                            action,
                            if danger { t.danger } else { t.accent },
                            t,
                        )
                        .on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.confirm_skill(cx)),
                        ),
                    )
                    .child(text_button("skills-confirm-cancel", "Cancel", t).on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| this.dismiss_skill_confirm(cx)),
                    )),
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
        self.note_switch_flip(&format!("skill-{path}"));
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
                        this.note_switch_flip(&format!("skill-{path}"));
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
                    }
                    Err(err) => this.skills.notice = Some(err),
                }
                cx.notify();
            });
        })
        .detach();
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

fn render_skill_instructions(text: &str, t: Tokens) -> gpui::Div {
    let body = skill_markdown_body(text);
    if body.is_empty() {
        return div()
            .py_2()
            .t_small()
            .text_color(c(t.text_faint))
            .child("No instructions found in SKILL.md.");
    }
    let mut out = div().flex().flex_col().gap_2();
    let lines: Vec<&str> = body.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim_end();
        let trimmed = line.trim();
        if trimmed.is_empty() {
            i += 1;
            continue;
        }
        if let Some(heading) = trimmed.strip_prefix("### ") {
            out = out.child(
                div()
                    .pt_1()
                    .text_size(zpx(13.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(c(t.text))
                    .child(heading.to_string()),
            );
            i += 1;
        } else if let Some(heading) = trimmed.strip_prefix("## ") {
            out = out.child(
                div()
                    .pt_2()
                    .pb_0p5()
                    .border_b_1()
                    .border_color(c(t.border).opacity(0.3))
                    .text_size(zpx(14.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(c(t.text))
                    .child(heading.to_string()),
            );
            i += 1;
        } else if let Some(heading) = trimmed.strip_prefix("# ") {
            out = out.child(
                div()
                    .pt_2p5()
                    .pb_1()
                    .border_b_1()
                    .border_color(c(t.border).opacity(0.4))
                    .text_size(zpx(15.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(c(t.text))
                    .child(heading.to_string()),
            );
            i += 1;
        } else if trimmed.starts_with("```") {
            let mut code_lines = Vec::new();
            i += 1;
            while i < lines.len() && !lines[i].trim().starts_with("```") {
                code_lines.push(lines[i]);
                i += 1;
            }
            if i < lines.len() {
                i += 1;
            }
            out = out.child(
                div()
                    .my_1()
                    .p_2p5()
                    .rounded(RADIUS_SM)
                    .bg(c(t.input))
                    .border_1()
                    .border_color(c(t.border).opacity(0.5))
                    .font_family(MONO)
                    .text_size(zpx(11.5))
                    .line_height(zpx(16.))
                    .text_color(c(t.text))
                    .overflow_hidden()
                    .child(code_lines.join("\n")),
            );
        } else if let Some(quote) = trimmed.strip_prefix("> ") {
            out = out.child(
                div()
                    .my_1()
                    .pl_3()
                    .py_0p5()
                    .border_l_2()
                    .border_color(c(t.accent))
                    .text_size(zpx(12.5))
                    .line_height(zpx(18.))
                    .text_color(c(t.text_muted))
                    .child(quote.to_string()),
            );
            i += 1;
        } else if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
            let item = &trimmed[2..];
            out = out.child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap_2()
                    .pl_1()
                    .child(
                        div()
                            .text_size(zpx(12.5))
                            .line_height(zpx(18.))
                            .text_color(c(t.text_faint))
                            .child("•"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(zpx(12.5))
                            .line_height(zpx(18.))
                            .text_color(c(t.text_muted))
                            .child(item.to_string()),
                    ),
            );
            i += 1;
        } else {
            out = out.child(
                div()
                    .text_size(zpx(12.5))
                    .line_height(zpx(18.))
                    .text_color(c(t.text_muted))
                    .child(trimmed.to_string()),
            );
            i += 1;
        }
    }
    out
}


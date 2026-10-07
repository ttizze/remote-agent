//! The Project page: choosing a project, then its overrides of the Host's
//! settings and its actions.
use super::{Row, SettingsPage, notice, page_container, reset_button, scripts, section};
use crate::app::{
    Desktop,
    ui::{color, icon, tint},
};
use agent_core::{
    state::Intent,
    view::settings::{SettingSource, SettingsScope},
};
use agent_protocol::models::ProjectScriptIcon;
use gpui_kit::{
    component::{
        Disableable, Sizable,
        button::{Button, ButtonVariants},
        h_flex, v_flex,
    },
    prelude::FluentBuilder,
    *,
};

/// The lucide icon of a project action.
pub(super) fn script_icon(icon: ProjectScriptIcon) -> &'static str {
    match icon {
        ProjectScriptIcon::Play => "play",
        ProjectScriptIcon::Test => "flask-conical",
        ProjectScriptIcon::Lint => "list-checks",
        ProjectScriptIcon::Configure => "wrench",
        ProjectScriptIcon::Build => "hammer",
        ProjectScriptIcon::Debug => "bug",
    }
}

fn badge(text: &'static str) -> Div {
    div()
        .flex_shrink_0()
        .px(px(6.))
        .rounded(px(4.))
        .border_1()
        .border_color(tint("border", 0.6))
        .text_size(px(11.))
        .line_height(px(16.))
        .font_weight(FontWeight::NORMAL)
        .text_color(color("textMuted"))
        .child(text)
}

impl Desktop {
    /// No project chosen yet: offer each project.
    pub(super) fn render_projects(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let projects = self.snapshot.shell_projects().to_vec();
        if projects.is_empty() {
            return page_container(
                896.,
                vec![
                    v_flex()
                        .items_start()
                        .gap_3()
                        .child(notice("No projects yet"))
                        .child(
                            Button::new("settings-add-project")
                                .outline()
                                .small()
                                .icon(icon("folder-plus"))
                                .label("Add project")
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.open_add_project(window, cx)
                                })),
                        )
                        .into_any_element(),
                ],
            );
        }
        let mut choices = h_flex().flex_wrap().gap_2();
        for (index, project) in projects.into_iter().enumerate() {
            let id = project.id.clone();
            let mark = self.project_icon(&project.id, &project.name, 14.);
            choices = choices.child(
                Button::new(("settings-project", index))
                    .outline()
                    .small()
                    .child(mark)
                    .label(project.name)
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.open_settings(
                            SettingsPage::Projects {
                                project_id: Some(id.clone()),
                            },
                            window,
                            cx,
                        )
                    })),
            );
        }
        page_container(
            896.,
            vec![
                v_flex()
                    .gap_3()
                    .p_4()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(color("border"))
                    .bg(color("surface"))
                    .text_sm()
                    .child("Choose a project to manage its name, icon, checkouts and actions.")
                    .child(choices)
                    .into_any_element(),
            ],
        )
    }

    pub(super) fn render_project(
        &mut self,
        project_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(project) = self
            .snapshot
            .shell_projects()
            .iter()
            .find(|project| project.id == project_id)
            .cloned()
        else {
            return page_container(896., vec![notice("This project is no longer available.")]);
        };
        let scope = SettingsScope::Project {
            project_id: project_id.to_owned(),
        };
        let view = self.snapshot.settings(scope.clone());
        let mut sections = Vec::new();
        let root = project
            .roots
            .first()
            .map(|root| root.path.clone())
            .unwrap_or_default();
        let has_overrides = view
            .project
            .as_ref()
            .is_some_and(|header| header.has_overrides);
        let reset_id = project_id.to_owned();
        let icon_row = self.project_icon_row(&project, cx);
        sections.push(
            section(
                Some("Project".into()),
                None,
                None,
                vec![
                    Row::new(
                        view.project
                            .as_ref()
                            .map_or_else(|| project.name.clone(), |header| header.label.clone()),
                    )
                    .description(div().truncate().child(root))
                    .when(has_overrides, |row| {
                        row.control(
                            Button::new("project-use-defaults")
                                .ghost()
                                .small()
                                .label("Use defaults")
                                .text_color(color("accent"))
                                .accessibility_label("Use environment defaults")
                                .on_click(cx.listener(move |view, _, _, _| {
                                    view.perform(Intent::ResetProjectSettings {
                                        project_id: reset_id.clone(),
                                    })
                                })),
                        )
                    })
                    .render(),
                    icon_row,
                ],
            )
            .into_any_element(),
        );
        if self.snapshot.conversation_settings_loaded() {
            sections.extend(self.render_setting_sections(&scope, &view.sections, window, cx));
        } else {
            sections.push(notice(if self.snapshot.connected {
                "Loading settings…"
            } else {
                "Connect an environment to change its settings."
            }));
        }
        sections.push(self.render_project_actions(project_id, cx));
        page_container(896., sections)
    }

    /// The project's icon: the file it reads, choosing another, and going
    /// back to finding one automatically.
    fn project_icon_row(
        &self,
        project: &agent_protocol::models::Project,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (choose, reset) = (project.id.clone(), project.id.clone());
        let local = self.remote.is_none();
        let reset = project.favicon_path.is_some().then(|| {
            reset_button(
                "reset-project-icon",
                "project icon",
                "Reset to default",
                move |view, _, _| {
                    view.perform(Intent::SetProjectIcon {
                        project_id: reset.clone(),
                        path: None,
                    })
                },
                cx,
            )
        });
        Row::new("Project icon")
            .description(
                project
                    .favicon_path
                    .clone()
                    .unwrap_or_else(|| "Automatic".into()),
            )
            .reset(reset)
            .control(
                h_flex()
                    .gap_2()
                    .child(self.project_icon(&project.id, &project.name, 24.))
                    .child(
                        Button::new("choose-project-icon")
                            .outline()
                            .small()
                            .label("Choose file")
                            .accessibility_label("Choose a project icon file")
                            .disabled(!local)
                            .on_click(cx.listener(move |view, _, _, _| {
                                if let Some(path) = rfd::FileDialog::new()
                                    .add_filter(
                                        "Image",
                                        &["png", "jpg", "jpeg", "svg", "ico", "webp", "gif"],
                                    )
                                    .pick_file()
                                {
                                    view.perform(Intent::SetProjectIcon {
                                        project_id: choose.clone(),
                                        path: Some(path.to_string_lossy().into_owned()),
                                    });
                                }
                            })),
                    ),
            )
            .render()
    }

    /// Where a project page's value comes from, beside its description.
    pub(super) fn setting_source_label(source: SettingSource) -> &'static str {
        match source {
            SettingSource::Project => "Overridden for this project",
            SettingSource::Host => "Inherited from environment",
        }
    }

    fn render_project_actions(&mut self, project_id: &str, cx: &mut Context<Self>) -> AnyElement {
        let scripts_view = self.snapshot.project_scripts(project_id.to_owned());
        let add_project = project_id.to_owned();
        let mut rows = vec![
            Row::new("Actions")
                .description(
                    "Commands that run in this project's checkout or its worktree, with optional shortcuts.",
                )
                .control(
                    Button::new("add-action")
                        .outline()
                        .xsmall()
                        .icon(icon("plus"))
                        .label("Add action")
                        .disabled(scripts_view.is_none())
                        .on_click(cx.listener(move |view, _, window, cx| {
                            scripts::open(view, add_project.clone(), None, window, cx)
                        })),
                )
                .render(),
        ];
        let script_rows = scripts_view.map(|view| view.rows).unwrap_or_default();
        if script_rows.is_empty() {
            rows.push(
                div()
                    .px_4()
                    .py_2()
                    .text_sm()
                    .text_color(color("textMuted"))
                    .child("No actions configured.")
                    .into_any_element(),
            );
        }
        for (index, row) in script_rows.into_iter().enumerate() {
            let script = row.script;
            let edit_project = project_id.to_owned();
            let script_id = script.id.clone();
            let group = SharedString::from(format!("project-action-{index}"));
            rows.push(
                div()
                    .id(("project-action", index))
                    .group(group.clone())
                    .child(
                        Row::new(
                            h_flex()
                                .min_w_0()
                                .gap_2()
                                .child(
                                    icon(script_icon(script.icon))
                                        .size_4()
                                        .text_color(color("textMuted")),
                                )
                                .child(div().min_w_0().truncate().child(script.name.clone()))
                                .when(script.run_on_worktree_create, |title| {
                                    title.child(badge("setup"))
                                })
                                .when(script.preview_url.is_some(), |title| {
                                    title.child(badge("preview · desktop only"))
                                }),
                        )
                        .description(
                            div()
                                .truncate()
                                .font_family("monospace")
                                .child(script.command.clone()),
                        )
                        .control(
                            div()
                                .opacity(0.)
                                .group_hover(group, |style| style.opacity(1.))
                                .child(
                                    Button::new(("edit-action", index))
                                        .icon(icon("settings"))
                                        .ghost()
                                        .xsmall()
                                        .text_color(color("textMuted"))
                                        .accessibility_label(row.edit_label.clone())
                                        .tooltip(row.edit_label)
                                        .on_click(cx.listener(move |view, _, window, cx| {
                                            scripts::open(
                                                view,
                                                edit_project.clone(),
                                                Some(script_id.clone()),
                                                window,
                                                cx,
                                            )
                                        })),
                                ),
                        )
                        .render(),
                    )
                    .into_any_element(),
            );
        }
        section(Some("Actions".into()), None, None, rows).into_any_element()
    }
}

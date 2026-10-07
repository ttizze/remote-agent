//! The General page: the Host's conversation settings, this device's
//! preferences and new-thread defaults, and the Host's worktrees.
use super::{
    Choice, Row, SettingsPage, group, notice, page_container, reset_button, section, select,
};
use crate::app::{
    Desktop,
    ui::{color, driver_icon, tint},
};
use agent_core::{
    state::Intent,
    view::{
        models::picker::PickerRail,
        settings::{
            SettingControl, SettingId, SettingValue, SettingsRow, SettingsScope, SettingsSection,
            parse_auto_settle_days, setting_intent, setting_reset_intent,
        },
    },
};
use agent_protocol::models::{Worktree, WorktreeSettings};
use gpui_kit::{
    component::{
        Disableable, Sizable,
        button::Button,
        h_flex,
        input::{Input, InputEvent, InputState},
        menu::{DropdownMenu, PopupMenuItem},
        switch::Switch,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};

pub(super) struct GeneralState {
    days: Entity<InputState>,
    /// The scope and value the days field last showed.
    days_value: Option<(SettingsScope, u32)>,
    folder: Entity<InputState>,
    folder_value: Option<String>,
    copy_paths: Entity<InputState>,
    copy_value: Option<Vec<String>>,
    _subscriptions: Vec<Subscription>,
}

impl GeneralState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Desktop>) -> Self {
        let days = cx.new(|cx| InputState::new(window, cx));
        let folder = cx.new(|cx| InputState::new(window, cx));
        let copy_paths = cx.new(|cx| InputState::new(window, cx).placeholder(".env, .env.local"));
        let subscriptions = vec![
            cx.subscribe_in(
                &days,
                window,
                |view, input, event: &InputEvent, window, cx| {
                    let Some(scope) = view.settings_scope() else {
                        return;
                    };
                    match event {
                        InputEvent::Change => {
                            let text = input.read(cx).value();
                            if let Some(days) = parse_auto_settle_days(&text)
                                && view
                                    .settings
                                    .general
                                    .days_value
                                    .as_ref()
                                    .map(|(_, value)| *value)
                                    != Some(days)
                            {
                                view.apply_setting(
                                    &scope,
                                    SettingId::AutoSettleDays,
                                    SettingValue::Number { value: days },
                                );
                            }
                        }
                        InputEvent::Blur => {
                            if let Some((_, days)) = &view.settings.general.days_value {
                                let days = days.to_string();
                                input.update(cx, |input, cx| input.set_value(days, window, cx));
                            }
                        }
                        _ => {}
                    }
                },
            ),
            cx.subscribe_in(&folder, window, |view, input, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                    let directory = input.read(cx).value().trim().to_owned();
                    view.save_worktree_settings(|settings| settings.worktree_directory = directory);
                }
            }),
            cx.subscribe_in(
                &copy_paths,
                window,
                |view, input, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                        let paths: Vec<String> = input
                            .read(cx)
                            .value()
                            .split([',', '\n'])
                            .map(str::trim)
                            .filter(|path| !path.is_empty())
                            .map(str::to_owned)
                            .collect();
                        view.save_worktree_settings(|settings| settings.copy_paths = paths);
                    }
                },
            ),
        ];
        Self {
            days,
            days_value: None,
            folder,
            folder_value: None,
            copy_paths,
            copy_value: None,
            _subscriptions: subscriptions,
        }
    }
}

impl Desktop {
    /// The scope the open settings page edits.
    pub(super) fn settings_scope(&self) -> Option<SettingsScope> {
        match &self.settings.page {
            SettingsPage::General => Some(SettingsScope::Host),
            SettingsPage::Projects {
                project_id: Some(project_id),
            } => Some(SettingsScope::Project {
                project_id: project_id.clone(),
            }),
            _ => None,
        }
    }

    fn apply_setting(&mut self, scope: &SettingsScope, id: SettingId, value: SettingValue) {
        if let Some(intent) = setting_intent(&self.snapshot, scope, id, &value) {
            self.perform(intent);
        }
    }

    fn save_worktree_settings(&mut self, change: impl FnOnce(&mut WorktreeSettings)) {
        let Some(mut settings) = self.snapshot.workspace.worktree_settings.clone() else {
            return;
        };
        let before = settings.clone();
        change(&mut settings);
        if settings != before {
            self.perform(Intent::SaveWorktreeSettings { settings });
        }
    }

    pub(super) fn render_general(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if !self.snapshot.conversation_settings_loaded() {
            return page_container(
                896.,
                vec![notice(if self.snapshot.connected {
                    "Loading settings…"
                } else {
                    "Connect an environment to change its settings."
                })],
            );
        }
        let scope = SettingsScope::Host;
        let view = self.snapshot.settings(scope.clone());
        let mut sections = self.render_setting_sections(&scope, &view.sections, window, cx);
        if let Some(worktrees) = self.render_worktrees(window, cx) {
            sections.push(worktrees);
        }
        page_container(896., sections)
    }

    /// The sections of a settings view, for the Host or one project.
    pub(super) fn render_setting_sections(
        &mut self,
        scope: &SettingsScope,
        sections: &[SettingsSection],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        self.sync_days_field(scope, sections, window, cx);
        sections
            .iter()
            .map(|settings_section| {
                let rows = settings_section
                    .rows
                    .iter()
                    .map(|row| self.render_setting_row(scope, row, cx))
                    .collect();
                section(
                    Some(settings_section.title.clone().into()),
                    None,
                    None,
                    rows,
                )
                .when_some(settings_section.footer.clone(), |section, footer| {
                    section.child(
                        div()
                            .px_4()
                            .text_xs()
                            .text_color(tint("textMuted", 0.8))
                            .child(footer),
                    )
                })
                .into_any_element()
            })
            .collect()
    }

    fn sync_days_field(
        &mut self,
        scope: &SettingsScope,
        sections: &[SettingsSection],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(days) =
            sections
                .iter()
                .flat_map(|section| &section.rows)
                .find_map(|row| match row.control {
                    SettingControl::Number { value, .. } if row.id == SettingId::AutoSettleDays => {
                        Some(value)
                    }
                    _ => None,
                })
        else {
            return;
        };
        let shown = Some((scope.clone(), days));
        if self.settings.general.days_value != shown {
            self.settings.general.days_value = shown;
            self.settings.general.days.update(cx, |input, cx| {
                input.set_value(days.to_string(), window, cx)
            });
        }
    }

    fn render_setting_row(
        &mut self,
        scope: &SettingsScope,
        row: &SettingsRow,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = row.id;
        let element_id = SharedString::from(format!("setting-{id:?}"));
        let reset = setting_reset_intent(scope, row).map(|intent| {
            let tooltip = match scope {
                SettingsScope::Host => "Reset to default",
                SettingsScope::Project { .. } => "Reset to inherited value",
            };
            reset_button(
                SharedString::from(format!("reset-{id:?}")),
                &row.title.to_lowercase(),
                tooltip,
                move |view, _, _| view.perform(intent.clone()),
                cx,
            )
        });
        let control = match &row.control {
            SettingControl::Switch { on } => {
                let scope = scope.clone();
                Switch::new(element_id)
                    .checked(*on)
                    .accessibility_label(row.title.clone())
                    .on_click(cx.listener(move |view, checked: &bool, _, _| {
                        view.apply_setting(&scope, id, SettingValue::Switch { on: *checked });
                    }))
                    .into_any_element()
            }
            SettingControl::Choice { choices, selected } => {
                let label = choices
                    .iter()
                    .find(|choice| Some(&choice.id) == selected.as_ref())
                    .map_or_else(String::new, |choice| choice.label.clone());
                let scope = scope.clone();
                select(
                    element_id,
                    label,
                    choices
                        .iter()
                        .map(|choice| Choice {
                            id: choice.id.clone(),
                            label: choice.label.clone(),
                            description: choice.description.clone(),
                            selected: Some(&choice.id) == selected.as_ref(),
                        })
                        .collect(),
                    move |view, choice, _, _| {
                        view.apply_setting(&scope, id, SettingValue::Choice { id: choice });
                    },
                    cx,
                )
                .into_any_element()
            }
            SettingControl::Number { .. } => Input::new(&self.settings.general.days)
                .small()
                .w(px(96.))
                .aria_label(row.title.clone())
                .into_any_element(),
            SettingControl::Model {
                model_label,
                traits_label,
            } => self.render_default_model(model_label, traits_label.as_deref(), cx),
        };
        let mut setting = Row::new(row.title.clone()).reset(reset).control(control);
        if let Some(description) = &row.description {
            setting = setting.description(description.clone());
        }
        if let Some(source) = row.source {
            setting = setting.status(Self::setting_source_label(source));
        }
        setting.render()
    }

    /// The Model row's control: the default model's picker and its traits.
    fn render_default_model(
        &self,
        model_label: &str,
        traits_label: Option<&str>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let picker = self.snapshot.default_model_picker(String::new(), None);
        let driver = picker
            .trigger
            .instance
            .as_ref()
            .map(|instance| instance.driver);
        let snapshot = self.snapshot.clone();
        let owner = cx.entity().downgrade();
        let trigger = Button::new("default-model")
            .outline()
            .small()
            .label(model_label.to_owned())
            .when_some(driver, |button, driver| button.icon(driver_icon(driver)))
            .dropdown_caret(true)
            .dropdown_menu_with_anchor(Anchor::TopRight, move |mut menu, _, _| {
                let picker = snapshot.default_model_picker(String::new(), None);
                let mut any = false;
                for rail in &picker.rail {
                    let PickerRail::Instance { .. } = &rail.rail else {
                        continue;
                    };
                    let rows = snapshot
                        .default_model_picker(String::new(), Some(rail.rail.clone()))
                        .rows;
                    if rows.is_empty() {
                        continue;
                    }
                    any = true;
                    menu = menu.label(rail.label.clone());
                    for row in rows {
                        let owner = owner.clone();
                        let intent = Intent::SetDefaultModel {
                            instance_id: row.instance_id.clone(),
                            driver: row.driver,
                            model: row.slug.clone(),
                            options: vec![],
                        };
                        menu = menu.item(
                            PopupMenuItem::new(row.name.clone())
                                .checked(row.selected)
                                .disabled(row.disabled_reason.is_some())
                                .on_click(move |_, _, cx| {
                                    let _ =
                                        owner.update(cx, |view, _| view.perform(intent.clone()));
                                }),
                        );
                    }
                }
                if !any {
                    menu = menu.item(
                        PopupMenuItem::new(
                            picker
                                .empty_label
                                .clone()
                                .unwrap_or_else(|| "No models found".into()),
                        )
                        .disabled(true),
                    );
                }
                menu
            });
        h_flex()
            .gap(px(6.))
            .child(trigger)
            .when_some(traits_label, |row, traits| {
                row.child(
                    div()
                        .text_xs()
                        .text_color(color("textMuted"))
                        .child(traits.to_owned()),
                )
            })
            .into_any_element()
    }

    /// The Host's worktree settings and the worktrees it keeps.
    fn render_worktrees(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let settings = self.snapshot.workspace.worktree_settings.clone()?;
        if self.settings.general.folder_value.as_ref() != Some(&settings.worktree_directory) {
            self.settings.general.folder_value = Some(settings.worktree_directory.clone());
            let value = settings.worktree_directory.clone();
            self.settings
                .general
                .folder
                .update(cx, |input, cx| input.set_value(value, window, cx));
        }
        if self.settings.general.copy_value.as_ref() != Some(&settings.copy_paths) {
            self.settings.general.copy_value = Some(settings.copy_paths.clone());
            let value = settings.copy_paths.join(", ");
            self.settings
                .general
                .copy_paths
                .update(cx, |input, cx| input.set_value(value, window, cx));
        }
        let mut rows = vec![
            Row::new("Worktree folder")
                .description("New worktrees are created in this folder on the Host.")
                .control(
                    Input::new(&self.settings.general.folder)
                        .small()
                        .w(px(256.))
                        .aria_label("Worktree folder"),
                )
                .render(),
            Row::new("Copy files into new worktrees")
                .description("Copy these ignored files from the checkout when a worktree is created.")
                .control(
                    Switch::new("worktree-copy")
                        .checked(settings.copy_on_create)
                        .accessibility_label("Copy files into new worktrees")
                        .on_click(cx.listener(|view, checked: &bool, _, _| {
                            let checked = *checked;
                            view.save_worktree_settings(|settings| {
                                settings.copy_on_create = checked
                            });
                        })),
                )
                .when(settings.copy_on_create, |row| {
                    row.below(
                        Input::new(&self.settings.general.copy_paths)
                            .small()
                            .aria_label("Files to copy"),
                    )
                })
                .render(),
            Row::new("Delete merged worktrees")
                .description("Remove worktrees whose pull request is merged and whose commits are included in the default branch.")
                .control(
                    Switch::new("worktree-delete-merged")
                        .checked(settings.delete_merged)
                        .accessibility_label("Delete merged worktrees")
                        .on_click(cx.listener(|view, checked: &bool, _, _| {
                            let checked = *checked;
                            view.save_worktree_settings(|settings| {
                                settings.delete_merged = checked
                            });
                        })),
                )
                .render(),
        ];
        rows.extend(
            self.snapshot
                .workspace
                .worktrees
                .clone()
                .into_iter()
                .enumerate()
                .map(|(index, worktree)| self.render_worktree(index, worktree, cx)),
        );
        Some(
            v_flex()
                .gap(px(10.))
                .child(
                    h_flex()
                        .min_h_7()
                        .px_4()
                        .text_sm()
                        .text_color(tint("text", 0.7))
                        .child("Worktrees"),
                )
                .child(group(rows))
                .into_any_element(),
        )
    }

    fn render_worktree(
        &self,
        index: usize,
        worktree: Worktree,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let description = match &worktree.blocked_reason {
            Some(reason) => format!("{} · {reason}", worktree.path),
            None => worktree.path.clone(),
        };
        let blocked = worktree.blocked_reason.is_some();
        let path = worktree.path.clone();
        Row::new(if worktree.branch.is_empty() {
            worktree.path.clone()
        } else {
            worktree.branch.clone()
        })
        .description(div().truncate().child(description))
        .control(
            Button::new(("remove-worktree", index))
                .outline()
                .small()
                .label("Remove")
                .disabled(blocked)
                .accessibility_label(format!("Remove worktree {}", worktree.path))
                .on_click(cx.listener(move |view, _, window, cx| {
                    let path = path.clone();
                    view.confirm(
                        crate::app::dialogs::Confirm {
                            title: Some("Remove worktree?".into()),
                            message: format!("{path} and its local changes are deleted from disk. Thread history is kept."),
                            action: "Remove".into(),
                            destructive: true,
                        },
                        window,
                        cx,
                        move |view, _, _| {
                            view.perform_then(
                                Intent::RemoveWorktree { path: path.clone() },
                                |view, result, window, cx| match result {
                                    Ok(_) => view.perform(Intent::ListWorktrees),
                                    Err(error) => {
                                        let error = error.clone();
                                        view.show_error(&error, window, cx)
                                    }
                                },
                            )
                        },
                    );
                })),
        )
        .render()
    }
}

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
        browser::BrowserProfile,
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
    storage_days: Entity<InputState>,
    storage_days_value: Option<(SettingsScope, u32)>,
    browser_days: Entity<InputState>,
    browser_days_value: Option<(SettingsScope, u32)>,
    browser_width: Entity<InputState>,
    browser_width_value: Option<(SettingsScope, u32)>,
    browser_height: Entity<InputState>,
    browser_height_value: Option<(SettingsScope, u32)>,
    browser_profile_name: Entity<InputState>,
    browser_profile_edit_id: Option<String>,
    logs_days: Entity<InputState>,
    logs_days_value: Option<(SettingsScope, u32)>,
    project_base: Entity<InputState>,
    project_base_value: Option<(SettingsScope, String)>,
    folder: Entity<InputState>,
    folder_value: Option<String>,
    copy_paths: Entity<InputState>,
    copy_value: Option<Vec<String>>,
    _subscriptions: Vec<Subscription>,
}

impl GeneralState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Desktop>) -> Self {
        let days = cx.new(|cx| InputState::new(window, cx));
        let storage_days = cx.new(|cx| InputState::new(window, cx));
        let browser_days = cx.new(|cx| InputState::new(window, cx));
        let browser_width = cx.new(|cx| InputState::new(window, cx));
        let browser_height = cx.new(|cx| InputState::new(window, cx));
        let browser_profile_name = cx.new(|cx| InputState::new(window, cx));
        let logs_days = cx.new(|cx| InputState::new(window, cx));
        let project_base = cx.new(|cx| InputState::new(window, cx));
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
            cx.subscribe_in(
                &storage_days,
                window,
                |view, input, event: &InputEvent, window, cx| {
                    let Some(scope) = view.settings_scope() else {
                        return;
                    };
                    match event {
                        InputEvent::Change => {
                            let text = input.read(cx).value();
                            if let Ok(days) = text.trim().parse::<u32>()
                                && (agent_protocol::models::MIN_RETENTION_DAYS
                                    ..=agent_protocol::models::MAX_RETENTION_DAYS)
                                    .contains(&days)
                            {
                                view.apply_setting(
                                    &scope,
                                    SettingId::StorageWorktreeAfterDays,
                                    SettingValue::Number { value: days },
                                );
                            }
                        }
                        InputEvent::Blur => {
                            if let Some((_, days)) = &view.settings.general.storage_days_value {
                                input.update(cx, |input, cx| {
                                    input.set_value(days.to_string(), window, cx)
                                });
                            }
                        }
                        _ => {}
                    }
                },
            ),
            cx.subscribe_in(
                &browser_days,
                window,
                |view, input, event: &InputEvent, window, cx| {
                    let Some(scope) = view.settings_scope() else {
                        return;
                    };
                    match event {
                        InputEvent::Change => {
                            let text = input.read(cx).value();
                            if let Ok(days) = text.trim().parse::<u32>()
                                && (agent_protocol::models::MIN_RETENTION_DAYS
                                    ..=agent_protocol::models::MAX_RETENTION_DAYS)
                                    .contains(&days)
                            {
                                view.apply_setting(
                                    &scope,
                                    SettingId::StorageBrowserArtifactsAfterDays,
                                    SettingValue::Number { value: days },
                                );
                            }
                        }
                        InputEvent::Blur => {
                            if let Some((_, days)) = &view.settings.general.browser_days_value {
                                input.update(cx, |input, cx| {
                                    input.set_value(days.to_string(), window, cx)
                                });
                            }
                        }
                        _ => {}
                    }
                },
            ),
            cx.subscribe_in(
                &browser_profile_name,
                window,
                |view, input, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. })
                        && let Some(profile_id) =
                            view.settings.general.browser_profile_edit_id.clone()
                    {
                        let name = input.read(cx).value().trim().to_owned();
                        if !name.is_empty() {
                            view.perform(Intent::RenameBrowserProfile { profile_id, name });
                        }
                        view.settings.general.browser_profile_edit_id = None;
                    }
                },
            ),
            cx.subscribe_in(
                &browser_width,
                window,
                |view, input, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change)
                        && let Ok(value) = input.read(cx).value().trim().parse::<u32>()
                        && (agent_protocol::preview::PREVIEW_VIEWPORT_MIN_DIMENSION
                            ..=agent_protocol::preview::PREVIEW_VIEWPORT_MAX_DIMENSION)
                            .contains(&value)
                    {
                        view.apply_setting(
                            &SettingsScope::Host,
                            SettingId::BrowserDefaultViewportWidth,
                            SettingValue::Number { value },
                        );
                    }
                },
            ),
            cx.subscribe_in(
                &browser_height,
                window,
                |view, input, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change)
                        && let Ok(value) = input.read(cx).value().trim().parse::<u32>()
                        && (agent_protocol::preview::PREVIEW_VIEWPORT_MIN_DIMENSION
                            ..=agent_protocol::preview::PREVIEW_VIEWPORT_MAX_DIMENSION)
                            .contains(&value)
                    {
                        view.apply_setting(
                            &SettingsScope::Host,
                            SettingId::BrowserDefaultViewportHeight,
                            SettingValue::Number { value },
                        );
                    }
                },
            ),
            cx.subscribe_in(
                &logs_days,
                window,
                |view, input, event: &InputEvent, window, cx| {
                    let Some(scope) = view.settings_scope() else {
                        return;
                    };
                    match event {
                        InputEvent::Change => {
                            let text = input.read(cx).value();
                            if let Ok(days) = text.trim().parse::<u32>()
                                && (agent_protocol::models::MIN_RETENTION_DAYS
                                    ..=agent_protocol::models::MAX_RETENTION_DAYS)
                                    .contains(&days)
                            {
                                view.apply_setting(
                                    &scope,
                                    SettingId::StorageLogsAfterDays,
                                    SettingValue::Number { value: days },
                                );
                            }
                        }
                        InputEvent::Blur => {
                            if let Some((_, days)) = &view.settings.general.logs_days_value {
                                input.update(cx, |input, cx| {
                                    input.set_value(days.to_string(), window, cx)
                                });
                            }
                        }
                        _ => {}
                    }
                },
            ),
            cx.subscribe_in(
                &project_base,
                window,
                |view, input, event: &InputEvent, _, cx| {
                    let Some(scope) = view.settings_scope() else {
                        return;
                    };
                    if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                        view.apply_setting(
                            &scope,
                            SettingId::AddProjectBaseDirectory,
                            SettingValue::Text {
                                value: input.read(cx).value().trim().to_owned(),
                            },
                        );
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
            storage_days,
            storage_days_value: None,
            browser_days,
            browser_days_value: None,
            browser_width,
            browser_width_value: None,
            browser_height,
            browser_height_value: None,
            browser_profile_name,
            browser_profile_edit_id: None,
            logs_days,
            logs_days_value: None,
            project_base,
            project_base_value: None,
            folder,
            folder_value: None,
            copy_paths,
            copy_value: None,
            _subscriptions: subscriptions,
        }
    }
}

/// The icon of a permissions choice, as the composer's runtime mode shows it.
fn runtime_mode_icon(id: &str) -> Option<&'static str> {
    match id {
        "approval-required" => Some("lock"),
        "auto-accept-edits" => Some("pen-line"),
        "auto" => Some("sparkles"),
        "full-access" => Some("lock-open"),
        _ => None,
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
        if !self.snapshot.host_settings_loaded() {
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
        sections.push(self.render_background_diagnostics(cx));
        page_container(896., sections)
    }

    fn render_background_diagnostics(&mut self, cx: &mut Context<Desktop>) -> AnyElement {
        let background_rows = self.snapshot.background_rows();
        let selected_profile = background_rows
            .iter()
            .find(|row| row.key == "profile")
            .map(|row| row.value.to_ascii_lowercase());
        let git_fetch_seconds =
            background_interval_seconds(&background_rows, "automaticGitFetchIntervalMs");
        let provider_health_seconds =
            background_interval_seconds(&background_rows, "providerHealthRefreshIntervalMs");
        let choices = ["balanced", "performance", "battery-saver"]
            .into_iter()
            .map(|id| Choice {
                id: id.into(),
                label: id.replace('-', " "),
                description: None,
                icon: None,
                selected: selected_profile.as_deref() == Some(id),
            })
            .collect();
        let profile = select(
            "background-profile",
            selected_profile
                .clone()
                .unwrap_or_else(|| "balanced".into()),
            choices,
            |view, profile, _, _| view.perform(Intent::SetBackgroundProfile { profile }),
            cx,
        );
        let mut rows = vec![
            Row::new("Profile")
                .description("Controls whether Host background work follows foreground demand or keeps running.")
                .control(profile)
                .render(),
            Row::new("Git fetch interval")
                .description("Refreshes remote branch status for active VCS leases.")
                .control(background_interval_select(
                    "background-git-fetch-interval",
                    git_fetch_seconds,
                    &[0, 15, 30, 60, 300, 900],
                    |view, seconds, _, _| {
                        view.perform(Intent::SetAutomaticGitFetchInterval { seconds })
                    },
                    cx,
                ))
                .render(),
            Row::new("Provider health interval")
                .description("Refreshes provider availability and model metadata for active provider leases.")
                .control(background_interval_select(
                    "background-provider-health-interval",
                    provider_health_seconds,
                    &[0, 60, 300, 900, 1800],
                    |view, seconds, _, _| {
                        view.perform(Intent::SetProviderHealthRefreshInterval { seconds })
                    },
                    cx,
                ))
                .render(),
        ];
        rows.extend(
            background_rows
                .into_iter()
                .filter(|row| {
                    !matches!(
                        row.key.as_str(),
                        "profile"
                            | "automaticGitFetchIntervalMs"
                            | "providerHealthRefreshIntervalMs"
                    )
                })
                .map(|row| Row::new(row.key).description(row.value).render()),
        );
        rows.extend(
            self.snapshot
                .host_resource_rows()
                .into_iter()
                .chain(self.snapshot.process_rows())
                .chain(self.snapshot.process_history_rows())
                .chain(self.snapshot.trace_rows())
                .map(|row| Row::new(row.key).description(row.value).render()),
        );
        let refresh = Button::new("refresh-background-diagnostics")
            .outline()
            .small()
            .label("Refresh")
            .on_click(cx.listener(|view, _, _, _| {
                view.perform(Intent::LoadDiagnostics {
                    trace_file_path: String::new(),
                })
            }))
            .into_any_element();
        section(
            Some("Background activity & diagnostics".into()),
            None,
            Some(refresh),
            rows,
        )
        .into_any_element()
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
        self.sync_storage_days_field(scope, sections, window, cx);
        self.sync_browser_days_field(scope, sections, window, cx);
        self.sync_browser_dimension_field(
            scope,
            sections,
            SettingId::BrowserDefaultViewportWidth,
            window,
            cx,
        );
        self.sync_browser_dimension_field(
            scope,
            sections,
            SettingId::BrowserDefaultViewportHeight,
            window,
            cx,
        );
        self.sync_logs_days_field(scope, sections, window, cx);
        self.sync_project_base_field(scope, sections, window, cx);
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

    fn sync_storage_days_field(
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
                    SettingControl::Number { value, .. }
                        if row.id == SettingId::StorageWorktreeAfterDays =>
                    {
                        Some(value)
                    }
                    _ => None,
                })
        else {
            return;
        };
        let shown = Some((scope.clone(), days));
        if self.settings.general.storage_days_value != shown {
            self.settings.general.storage_days_value = shown;
            self.settings.general.storage_days.update(cx, |input, cx| {
                input.set_value(days.to_string(), window, cx)
            });
        }
    }

    fn sync_browser_days_field(
        &mut self,
        scope: &SettingsScope,
        sections: &[SettingsSection],
        window: &mut Window,
        cx: &mut Context<Desktop>,
    ) {
        let Some(days) =
            sections
                .iter()
                .flat_map(|section| &section.rows)
                .find_map(|row| match row.control {
                    SettingControl::Number { value, .. }
                        if row.id == SettingId::StorageBrowserArtifactsAfterDays =>
                    {
                        Some(value)
                    }
                    _ => None,
                })
        else {
            return;
        };
        let shown = Some((scope.clone(), days));
        if self.settings.general.browser_days_value != shown {
            self.settings.general.browser_days_value = shown;
            self.settings.general.browser_days.update(cx, |input, cx| {
                input.set_value(days.to_string(), window, cx)
            });
        }
    }

    fn sync_logs_days_field(
        &mut self,
        scope: &SettingsScope,
        sections: &[SettingsSection],
        window: &mut Window,
        cx: &mut Context<Desktop>,
    ) {
        let Some(days) =
            sections
                .iter()
                .flat_map(|section| &section.rows)
                .find_map(|row| match row.control {
                    SettingControl::Number { value, .. }
                        if row.id == SettingId::StorageLogsAfterDays =>
                    {
                        Some(value)
                    }
                    _ => None,
                })
        else {
            return;
        };
        let shown = Some((scope.clone(), days));
        if self.settings.general.logs_days_value != shown {
            self.settings.general.logs_days_value = shown;
            self.settings.general.logs_days.update(cx, |input, cx| {
                input.set_value(days.to_string(), window, cx)
            });
        }
    }

    fn sync_browser_dimension_field(
        &mut self,
        scope: &SettingsScope,
        sections: &[SettingsSection],
        id: SettingId,
        window: &mut Window,
        cx: &mut Context<Desktop>,
    ) {
        let Some(value) =
            sections
                .iter()
                .flat_map(|section| &section.rows)
                .find_map(|row| match row.control {
                    SettingControl::Number { value, .. } if row.id == id => Some(value),
                    _ => None,
                })
        else {
            return;
        };
        let shown = Some((scope.clone(), value));
        match id {
            SettingId::BrowserDefaultViewportWidth
                if self.settings.general.browser_width_value != shown =>
            {
                self.settings.general.browser_width_value = shown;
                self.settings.general.browser_width.update(cx, |input, cx| {
                    input.set_value(value.to_string(), window, cx)
                });
            }
            SettingId::BrowserDefaultViewportHeight
                if self.settings.general.browser_height_value != shown =>
            {
                self.settings.general.browser_height_value = shown;
                self.settings
                    .general
                    .browser_height
                    .update(cx, |input, cx| {
                        input.set_value(value.to_string(), window, cx)
                    });
            }
            _ => {}
        }
    }

    fn sync_project_base_field(
        &mut self,
        scope: &SettingsScope,
        sections: &[SettingsSection],
        window: &mut Window,
        cx: &mut Context<Desktop>,
    ) {
        let Some(value) = sections
            .iter()
            .flat_map(|section| &section.rows)
            .find_map(|row| match &row.control {
                SettingControl::Text { value, .. }
                    if row.id == SettingId::AddProjectBaseDirectory =>
                {
                    Some(value.clone())
                }
                _ => None,
            })
        else {
            return;
        };
        let shown = Some((scope.clone(), value.clone()));
        if self.settings.general.project_base_value != shown {
            self.settings.general.project_base_value = shown;
            self.settings
                .general
                .project_base
                .update(cx, |input, cx| input.set_value(value, window, cx));
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
                            icon: (id == SettingId::DefaultPermissions)
                                .then(|| runtime_mode_icon(&choice.id))
                                .flatten(),
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
            SettingControl::Number { .. } => {
                let input = match id {
                    SettingId::AutoSettleDays => &self.settings.general.days,
                    SettingId::StorageWorktreeAfterDays => &self.settings.general.storage_days,
                    SettingId::StorageBrowserArtifactsAfterDays => {
                        &self.settings.general.browser_days
                    }
                    SettingId::StorageLogsAfterDays => &self.settings.general.logs_days,
                    SettingId::BrowserDefaultViewportWidth => &self.settings.general.browser_width,
                    SettingId::BrowserDefaultViewportHeight => {
                        &self.settings.general.browser_height
                    }
                    _ => &self.settings.general.days,
                };
                Input::new(input)
                    .small()
                    .w(px(96.))
                    .aria_label(row.title.clone())
                    .into_any_element()
            }
            SettingControl::Text { .. } => Input::new(&self.settings.general.project_base)
                .small()
                .w(px(280.))
                .aria_label(row.title.clone())
                .into_any_element(),
            SettingControl::BrowserProfiles {
                profiles,
                default_profile_id,
            } => self.render_browser_profiles(profiles, default_profile_id, cx),
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

    fn render_browser_profiles(
        &self,
        profiles: &[BrowserProfile],
        default_profile_id: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut rows = v_flex()
            .gap(px(4.))
            .children(profiles.iter().map(|profile| {
                let profile_id = profile.id.clone();
                let is_default = profile.id == default_profile_id;
                let built_in = matches!(profile.id.as_str(), "default" | "incognito");
                let mut row = h_flex()
                    .gap(px(8.))
                    .child(div().flex_1().text_sm().child(profile.name.clone()));
                if profile.id != "incognito" {
                    row = row.child(
                        Button::new(("browser-profile-select", profile.id.clone()))
                            .outline()
                            .small()
                            .label(if is_default { "Default" } else { "Use" })
                            .on_click(cx.listener(move |view, _, _, _| {
                                view.perform(Intent::SetBrowserDefaultProfile {
                                    profile_id: profile_id.clone(),
                                });
                            })),
                    );
                }
                if !built_in {
                    let edit_id = profile.id.clone();
                    let edit_name = profile.name.clone();
                    let remove_id = profile.id.clone();
                    row = row
                        .child(
                            Button::new(("browser-profile-rename", profile.id.clone()))
                                .outline()
                                .small()
                                .label("Rename")
                                .on_click(cx.listener(move |view, _, window, cx| {
                                    view.settings.general.browser_profile_edit_id =
                                        Some(edit_id.clone());
                                    view.settings
                                        .general
                                        .browser_profile_name
                                        .update(cx, |input, cx| {
                                            input.set_value(edit_name.clone(), window, cx)
                                        });
                                })),
                        )
                        .child(
                            Button::new(("browser-profile-remove", profile.id.clone()))
                                .outline()
                                .small()
                                .label("Remove")
                                .on_click(cx.listener(move |view, _, window, cx| {
                                    view.remove_browser_profile(remove_id.clone(), window, cx);
                                })),
                        );
                }
                row
            }));
        let create = Button::new("browser-profile-create")
            .outline()
            .small()
            .disabled(
                profiles
                    .iter()
                    .filter(|profile| !matches!(profile.id.as_str(), "default" | "incognito"))
                    .count()
                    >= 24,
            )
            .label("New profile")
            .on_click(cx.listener(|view, _, _, _| {
                view.perform(Intent::CreateBrowserProfile {
                    profile_id: uuid::Uuid::new_v4().to_string(),
                    requested_name: None,
                });
            }));
        rows = rows.child(create);
        if self.settings.general.browser_profile_edit_id.is_some() {
            rows = rows.child(
                Input::new(&self.settings.general.browser_profile_name)
                    .small()
                    .w(px(240.))
                    .aria_label("Browser profile name"),
            );
        }
        rows.into_any_element()
    }

    /// The Model row's control: the default model's picker and its traits.
    fn render_default_model(
        &self,
        model_label: &str,
        traits_label: Option<&str>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let picker = self
            .snapshot
            .default_model_picker(String::new(), None, vec![]);
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
                let picker = snapshot.default_model_picker(String::new(), None, vec![]);
                let mut any = false;
                for rail in &picker.rail {
                    let PickerRail::Instance { .. } = &rail.rail else {
                        continue;
                    };
                    let rows = snapshot
                        .default_model_picker(String::new(), Some(rail.rail.clone()), vec![])
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

fn background_interval_seconds(
    rows: &[agent_core::view::diagnostics::DiagnosticRow],
    key: &str,
) -> u32 {
    rows.iter()
        .find(|row| row.key == key)
        .and_then(|row| row.value.parse::<u64>().ok())
        .map(|milliseconds| (milliseconds / 1_000).min(u32::MAX as u64) as u32)
        .unwrap_or_default()
}

fn background_interval_select(
    id: &'static str,
    selected_seconds: u32,
    options: &'static [u32],
    pick: impl Fn(&mut Desktop, u32, &mut Window, &mut Context<Desktop>) + 'static,
    cx: &mut Context<Desktop>,
) -> AnyElement {
    let choices = options
        .iter()
        .copied()
        .map(|seconds| Choice {
            id: seconds.to_string(),
            label: if seconds == 0 {
                "Disabled".into()
            } else {
                format!("{seconds} seconds")
            },
            description: None,
            icon: None,
            selected: seconds == selected_seconds,
        })
        .collect();
    select(
        id,
        if selected_seconds == 0 {
            "Disabled".into()
        } else {
            format!("{selected_seconds} seconds")
        },
        choices,
        move |view, value, window, cx| {
            if let Ok(seconds) = value.parse::<u32>() {
                pick(view, seconds, window, cx);
            }
        },
        cx,
    )
    .into_any_element()
}

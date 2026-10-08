//! New threads: the model, permissions and workspace a new thread starts
//! with, and whether a new worktree starts from origin.
use super::{
    ProjectSettingKey, SettingChange, SettingChoice, SettingControl, SettingId, SettingSource,
    SettingValue, SettingsRow, SettingsScope, choice,
    registry::{Context, Section},
    row, section, update,
};
use crate::{
    models::HostSettings,
    state::{Draft, Intent, Snapshot},
    view::models::{
        catalog,
        picker::{ModelPickerOptions, ModelPickerView, build_model_picker, trigger},
        traits::build_traits,
    },
};
use agent_domain::RuntimeMode;

pub fn runtime_mode_id(mode: RuntimeMode) -> &'static str {
    match mode {
        RuntimeMode::ApprovalRequired => "approval-required",
        RuntimeMode::AutoAcceptEdits => "auto-accept-edits",
        RuntimeMode::Auto => "auto",
        RuntimeMode::FullAccess => "full-access",
    }
}

const RUNTIME_MODES: [RuntimeMode; 4] = [
    RuntimeMode::ApprovalRequired,
    RuntimeMode::AutoAcceptEdits,
    RuntimeMode::Auto,
    RuntimeMode::FullAccess,
];

fn runtime_mode_choices() -> Vec<SettingChoice> {
    [
        (
            RuntimeMode::ApprovalRequired,
            "Supervised",
            "Ask before commands and file changes.",
        ),
        (
            RuntimeMode::AutoAcceptEdits,
            "Auto-accept edits",
            "Auto-approve edits, ask before other actions.",
        ),
        (
            RuntimeMode::Auto,
            "Auto",
            "Supported providers approve routine actions; others still ask.",
        ),
        (
            RuntimeMode::FullAccess,
            "Full access",
            "Allow commands and edits without prompts.",
        ),
    ]
    .into_iter()
    .map(|(mode, label, description)| choice(runtime_mode_id(mode), label, Some(description)))
    .collect()
}

fn start_from_origin_row(on: bool, source: Option<SettingSource>) -> SettingsRow {
    SettingsRow {
        resettable: on != HostSettings::default().new_worktrees_start_from_origin,
        source,
        ..row(
            SettingId::StartFromOrigin,
            "Start from origin",
            Some(
                "Creates the worktree from the latest matching branch on origin instead of your local branch.",
            ),
            SettingControl::Switch { on },
        )
    }
}

/// The picker the Model row opens: the new-thread default, which no open
/// thread locks or limits.
pub fn default_model_picker(snapshot: &Snapshot, options: &ModelPickerOptions) -> ModelPickerView {
    let draft = &snapshot.default_draft;
    build_model_picker(
        &catalog(snapshot),
        &draft.instance_id,
        &draft.model,
        None,
        &|_, _| None,
        options,
    )
}

pub(super) const SECTION: Section = Section {
    ids: &[
        SettingId::DefaultModel,
        SettingId::DefaultPermissions,
        SettingId::DefaultWorkspace,
        SettingId::StartFromOrigin,
    ],
    host: |context: &Context| {
        let snapshot = context.snapshot;
        let draft = &snapshot.default_draft;
        let models = catalog(snapshot);
        let traits = build_traits(&models, draft, false);
        let mut rows = vec![
            row(
                SettingId::DefaultModel,
                "Model",
                Some("Default model for new threads."),
                SettingControl::Model {
                    model_label: trigger(&models, &draft.instance_id, &draft.model).label,
                    traits_label: traits.visible.then_some(traits.trigger.label),
                },
            ),
            SettingsRow {
                resettable: draft.runtime_mode != RuntimeMode::FullAccess,
                ..row(
                    SettingId::DefaultPermissions,
                    "Permissions",
                    Some("Default permissions for new threads."),
                    SettingControl::Choice {
                        choices: runtime_mode_choices(),
                        selected: Some(runtime_mode_id(draft.runtime_mode).into()),
                    },
                )
            },
        ];
        if let Some(worktrees) = &snapshot.workspace.worktree_settings {
            rows.push(row(
                SettingId::DefaultWorkspace,
                "Workspace",
                Some("Where new threads start."),
                SettingControl::Choice {
                    choices: vec![
                        choice("local", "Current checkout", None),
                        choice("worktree", "New worktree", None),
                    ],
                    selected: Some(
                        if worktrees.create_on_new_session {
                            "worktree"
                        } else {
                            "local"
                        }
                        .into(),
                    ),
                },
            ));
        }
        if let Some(host) = context.host {
            rows.push(start_from_origin_row(
                host.new_worktrees_start_from_origin,
                None,
            ));
        }
        Some(section("new-threads", "New threads", rows, None))
    },
    project: |resolved| {
        Some(section(
            "new-threads",
            "New threads",
            vec![start_from_origin_row(
                resolved.new_worktrees_start_from_origin.0,
                Some(resolved.new_worktrees_start_from_origin.1),
            )],
            None,
        ))
    },
    intent: |snapshot, scope, id, value| match (id, value) {
        (SettingId::StartFromOrigin, SettingValue::Switch { on }) => Some(update(
            scope,
            SettingChange::NewWorktreesStartFromOrigin { on: *on },
        )),
        (SettingId::DefaultPermissions, SettingValue::Choice { id }) => RUNTIME_MODES
            .into_iter()
            .find(|mode| runtime_mode_id(*mode) == id)
            .map(|mode| Intent::SetDefaultRuntimeMode { mode }),
        (SettingId::DefaultWorkspace, SettingValue::Choice { id }) => {
            let create_on_new_session = match id.as_str() {
                "local" => false,
                "worktree" => true,
                _ => return None,
            };
            let settings = snapshot.workspace.worktree_settings.clone()?;
            Some(Intent::SaveWorktreeSettings {
                settings: crate::models::WorktreeSettings {
                    create_on_new_session,
                    ..settings
                },
            })
        }
        _ => None,
    },
    reset: |id| match id {
        SettingId::StartFromOrigin => Some(update(
            &SettingsScope::Host,
            SettingChange::NewWorktreesStartFromOrigin {
                on: HostSettings::default().new_worktrees_start_from_origin,
            },
        )),
        SettingId::DefaultPermissions => Some(Intent::SetDefaultRuntimeMode {
            mode: Draft::default().runtime_mode,
        }),
        _ => None,
    },
    inherit: |id| match id {
        SettingId::StartFromOrigin => Some(ProjectSettingKey::NewWorktreesStartFromOrigin),
        _ => None,
    },
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::{
        settings::{
            clear_project_overrides,
            fixtures::{find, project, same},
            new_worktrees_start_from_origin, plan_settings_update, setting_reset_intent,
            settings_view,
        },
        time::TimestampFormat,
    };

    // contracts settings.test.ts "defaults start-from-origin on" and "accepts
    // start-from-origin updates"; a project can override it like the reference's
    // project-scoped server settings.
    #[test]
    fn start_from_origin_defaults_on_and_a_project_can_override_it() {
        let decoded: HostSettings = serde_json::from_str("{}").unwrap();
        assert!(decoded.new_worktrees_start_from_origin);
        assert!(new_worktrees_start_from_origin(None, Some("p")));
        let change = plan_settings_update(
            &SettingsScope::Host,
            &SettingChange::NewWorktreesStartFromOrigin { on: false },
        )
        .unwrap();
        let host = decoded.patched(&change);
        assert!(!new_worktrees_start_from_origin(Some(&host), Some("p")));
        let change = plan_settings_update(
            &project("p"),
            &SettingChange::NewWorktreesStartFromOrigin { on: true },
        )
        .unwrap();
        let host = host.patched(&change);
        assert!(new_worktrees_start_from_origin(Some(&host), Some("p")));
        assert!(!new_worktrees_start_from_origin(Some(&host), Some("q")));
        let view = settings_view(
            &Snapshot::default(),
            Some(&host),
            &project("p"),
            TimestampFormat::Locale,
        );
        let row = find(&view, SettingId::StartFromOrigin);
        assert_eq!(
            (&row.control, row.source),
            (
                &SettingControl::Switch { on: true },
                Some(SettingSource::Project)
            )
        );
        same(
            setting_reset_intent(&project("p"), &row),
            Some(Intent::UpdateSettings {
                scope: project("p"),
                change: SettingChange::Inherit {
                    key: ProjectSettingKey::NewWorktreesStartFromOrigin,
                },
            }),
        );
        let cleared =
            clear_project_overrides("p", &[ProjectSettingKey::NewWorktreesStartFromOrigin]);
        assert!(!host.patched(&cleared).project_overrides.contains_key("p"));
    }

    #[test]
    fn the_default_model_picker_reads_the_new_thread_default_not_the_open_thread() {
        use crate::view::models::fixtures::{host_instance, host_model};
        use agent_domain::Driver;
        let mut snapshot = Snapshot {
            providers: Some(vec![
                host_instance(
                    "codex",
                    Driver::Codex,
                    vec![host_model("gpt-5.4", "gpt-5.4")],
                ),
                host_instance(
                    "claude",
                    Driver::Claude,
                    vec![host_model("sonnet", "Sonnet")],
                ),
            ]),
            ..Snapshot::default()
        };
        snapshot.default_draft.instance_id = "claude".into();
        snapshot.default_draft.driver = agent_domain::Driver::Claude;
        snapshot.default_draft.model = "sonnet".into();
        snapshot.drafts.insert(
            "new:chats".into(),
            crate::state::Draft {
                instance_id: "codex".into(),
                model: "gpt-5.4".into(),
                ..Default::default()
            },
        );
        let picker = default_model_picker(&snapshot, &ModelPickerOptions::default());
        assert_eq!(picker.trigger.instance_id, "claude");
        assert_eq!(picker.locked_driver, None);
        let selected: Vec<_> = picker.rows.iter().filter(|row| row.selected).collect();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].slug, "sonnet");
        assert!(picker.rows.iter().all(|row| row.disabled_reason.is_none()));
    }
}

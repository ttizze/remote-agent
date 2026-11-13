//! Worktree form values and removal availability shared by native clients.
use agent_protocol::models::WorktreeSettings;

/// Clients supply the lines from their native text editor; the Host validates paths.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn normalize_worktree_settings(settings: WorktreeSettings) -> WorktreeSettings {
    WorktreeSettings {
        worktree_directory: settings.worktree_directory.trim().to_owned(),
        copy_paths: settings
            .copy_paths
            .iter()
            .map(|path| path.trim())
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect(),
        ..settings
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn worktree_removal_reason(
    blocked_reason: Option<String>,
    current_directory: String,
    worktree_path: String,
) -> Option<String> {
    blocked_reason.or_else(|| {
        (!current_directory.is_empty()
            && super::markdown::file_workspace_relative_path(&current_directory, &worktree_path)
                .is_some())
        .then(|| {
            "現在開いている会話の作業場所です。別の会話に移動してから削除してください。".into()
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_normalization_preserves_settings_and_leaves_invalid_paths_for_host_validation() {
        let settings = WorktreeSettings {
            create_on_new_session: true,
            copy_on_create: true,
            delete_unchanged: true,
            worktree_directory: "  /fixture/work trees\n".into(),
            copy_paths: [
                "  .env.local ",
                " \t",
                "config/日本語　",
                "../escape",
                "/absolute",
                "my file",
                "my file",
            ]
            .map(str::to_owned)
            .into(),
            project_delete_unchanged: [("project".into(), false)].into(),
            setup_script: Some(agent_protocol::workspace::SetupScript {
                name: "Install".into(),
                command: "npm ci".into(),
                asynchronous: false,
            }),
            project_setup_scripts: Default::default(),
            init_submodules: true,
        };
        let normalized = normalize_worktree_settings(settings.clone());
        assert_eq!(normalized.worktree_directory, "/fixture/work trees");
        assert_eq!(
            normalized.copy_paths,
            [
                ".env.local",
                "config/日本語",
                "../escape",
                "/absolute",
                "my file",
                "my file"
            ]
        );
        assert_eq!(
            normalized,
            WorktreeSettings {
                worktree_directory: "/fixture/work trees".into(),
                copy_paths: normalized.copy_paths.clone(),
                ..settings
            }
        );
        assert_eq!(normalize_worktree_settings(normalized.clone()), normalized);
        assert_eq!(
            normalize_worktree_settings(WorktreeSettings::default()),
            WorktreeSettings::default()
        );
    }

    #[test]
    fn removal_preserves_host_restrictions_and_blocks_the_open_workspace_across_host_path_styles() {
        let message = "現在開いている会話の作業場所です。別の会話に移動してから削除してください。";
        for (current, root, blocked) in [
            ("/repo/worktree", "/repo/worktree", true),
            ("/repo/worktree/src", "/repo/worktree/", true),
            ("/repo/worktree-other", "/repo/worktree", false),
            ("/repo/worktree/../other", "/repo/worktree", false),
            (r"c:\repo\Worktree\src", "C:/repo/worktree", true),
            (r"C:\repo\worktree-other", r"C:\repo\worktree", false),
            ("", "/", false),
            ("/repo", "", false),
        ] {
            assert_eq!(
                worktree_removal_reason(None, current.into(), root.into()).as_deref(),
                blocked.then_some(message)
            );
        }
        assert_eq!(
            worktree_removal_reason(
                Some("ターミナルを使用中です".into()),
                "/repo".into(),
                "/repo".into()
            )
            .as_deref(),
            Some("ターミナルを使用中です")
        );
    }
}

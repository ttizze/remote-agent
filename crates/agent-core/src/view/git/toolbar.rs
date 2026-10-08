use agent_protocol::workspace::VcsStatus;

/// The branch control's label, including its loading and detached states.
pub fn branch_label(status: Option<&VcsStatus>) -> String {
    status
        .and_then(|status| status.ref_name.clone())
        .filter(|branch| !branch.is_empty())
        .unwrap_or_else(|| "Select ref".into())
}

/// A compact action label that explains why a pull control is available.
pub fn pull_label(status: Option<&VcsStatus>) -> String {
    match status {
        Some(status) if status.behind_count > 0 => "Pull".into(),
        Some(status) if status.has_upstream => "Up to date".into(),
        Some(_) => "No upstream".into(),
        None => "Git unavailable".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::workspace::{VcsStatus, WorkingTreeChanges};

    fn status() -> VcsStatus {
        VcsStatus {
            is_repo: true,
            source_control_provider: None,
            has_primary_remote: true,
            is_default_ref: false,
            ref_name: Some("feature/demo".into()),
            has_working_tree_changes: false,
            working_tree: WorkingTreeChanges {
                files: vec![],
                insertions: 0,
                deletions: 0,
            },
            branch_changes: None,
            has_upstream: true,
            ahead_count: 0,
            behind_count: 1,
            ahead_of_default_count: Some(0),
            pr: None,
        }
    }

    #[test]
    fn branch_and_pull_labels_follow_status() {
        let status = status();
        assert_eq!(branch_label(Some(&status)), "feature/demo");
        assert_eq!(pull_label(Some(&status)), "Pull");
        assert_eq!(branch_label(None), "Select ref");
    }
}

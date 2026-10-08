use agent_protocol::workspace::VcsStatus;

/// The compact status record rendered by branch controls on every client.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct GitStatusView {
    pub is_repo: bool,
    pub branch: Option<String>,
    pub has_working_tree_changes: bool,
    pub has_upstream: bool,
    pub ahead_count: u64,
    pub behind_count: u64,
    pub ahead_of_default_count: u64,
    pub pull_request_url: Option<String>,
    pub provider_name: Option<String>,
}

pub fn status_view(status: Option<&VcsStatus>) -> Option<GitStatusView> {
    status.map(|status| GitStatusView {
        is_repo: status.is_repo,
        branch: status.ref_name.clone(),
        has_working_tree_changes: status.has_working_tree_changes,
        has_upstream: status.has_upstream,
        ahead_count: status.ahead_count,
        behind_count: status.behind_count,
        ahead_of_default_count: status.ahead_of_default_count.unwrap_or(status.ahead_count),
        pull_request_url: status.pr.as_ref().map(|pr| pr.url.clone()),
        provider_name: status
            .source_control_provider
            .as_ref()
            .map(|provider| provider.name.clone()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::vcs::{SourceControlProviderInfo, SourceControlProviderKind};
    use agent_protocol::workspace::WorkingTreeChanges;

    #[test]
    fn missing_remote_ahead_count_uses_local_ahead_count() {
        let status = VcsStatus {
            is_repo: true,
            source_control_provider: Some(SourceControlProviderInfo {
                kind: SourceControlProviderKind::Github,
                name: "GitHub".into(),
                base_url: "https://github.com".into(),
            }),
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
            has_upstream: false,
            ahead_count: 2,
            behind_count: 0,
            ahead_of_default_count: None,
            pr: None,
        };
        assert_eq!(
            status_view(Some(&status)).unwrap().ahead_of_default_count,
            2
        );
    }
}

//! A list scope never replaces the last concrete destination used for a task.
use super::list::ThreadList;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ConnectionSelection {
    pub host_id: Option<String>,
    pub all: bool,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn resolve_connection_selection(
    hosts: Vec<String>,
    last_host: Option<String>,
    all: bool,
) -> ConnectionSelection {
    let host_id = last_host
        .filter(|id| hosts.contains(id))
        .or_else(|| hosts.into_iter().next());
    ConnectionSelection {
        all: all && host_id.is_some(),
        host_id,
    }
}

/// Keep each checkout and session on its owning Host, including colliding IDs.
/// Grouping and ordering within each Host remain those of its existing list.
pub fn combine_connection_lists(
    lists: impl IntoIterator<Item = (String, ThreadList)>,
) -> Option<ThreadList> {
    lists
        .into_iter()
        .map(|(host, mut list)| {
            for thread in &mut list.threads {
                thread.host_id.clone_from(&host);
            }
            for project in list.projects.iter_mut().chain(&mut list.hidden_projects) {
                project.host_id.clone_from(&host);
                for thread in &mut project.threads {
                    thread.host_id.clone_from(&host);
                }
            }
            list
        })
        .reduce(|mut list, other| {
            list.loading_projects |= other.loading_projects;
            list.loading_chats |= other.loading_chats;
            list.has_more |= other.has_more;
            list.has_more_projects |= other.has_more_projects;
            list.notice = match (list.notice, other.notice) {
                (Some(first), Some(second)) if first != second => {
                    Some(format!("{first}\n{second}"))
                }
                (first, second) => first.or(second),
            };
            list.projects.extend(other.projects);
            list.hidden_projects.extend(other.hidden_projects);
            list.threads.extend(other.threads);
            list
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presentation::list::{ProjectSummary, ThreadSummary};

    fn thread() -> ThreadSummary {
        ThreadSummary {
            attention: None,
            host_id: String::new(),
            id: crate::session::SessionRef {
                provider: Some(crate::session::ProviderKind::Codex),
                id: "same-session".into(),
            },
            title: "Task".into(),
            copy_items: vec![],
            project_id: Some("same-project".into()),
            active: false,
            waiting: false,
            background: false,
            unread: false,
            has_changes: None,
        }
    }

    fn list() -> ThreadList {
        ThreadList {
            loading_projects: false,
            loading_chats: false,
            notice: None,
            projects: vec![ProjectSummary {
                host_id: String::new(),
                id: "same-project".into(),
                name: "repo".into(),
                coordinator_url: None,
                roots: vec![],
                icon: None,
                monogram: "R".into(),
                icon_color: 0,
                pinned: false,
                section: None,
                chats_archived: false,
                expanded: true,
                loading: false,
                error: None,
                threads: vec![thread()],
                has_more: false,
            }],
            hidden_projects: vec![],
            threads: vec![thread()],
            has_more: false,
            has_more_projects: false,
        }
    }

    #[test]
    fn all_keeps_the_last_destination_and_removed_destinations_fall_back() {
        let hosts = vec!["laptop".into(), "server".into()];
        assert_eq!(
            resolve_connection_selection(hosts.clone(), Some("server".into()), true),
            ConnectionSelection {
                host_id: Some("server".into()),
                all: true
            }
        );
        assert_eq!(
            resolve_connection_selection(hosts, Some("removed".into()), false),
            ConnectionSelection {
                host_id: Some("laptop".into()),
                all: false
            }
        );
        assert_eq!(
            resolve_connection_selection(vec![], Some("removed".into()), true),
            ConnectionSelection {
                host_id: None,
                all: false
            }
        );
    }

    #[test]
    fn saved_all_scope_restores_the_last_concrete_destination() {
        let saved = ConnectionSelection {
            host_id: Some("server".into()),
            all: true,
        };
        let bytes = serde_json::to_vec(&saved).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            serde_json::json!({"host_id":"server","all":true})
        );
        let restored: ConnectionSelection = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            resolve_connection_selection(
                vec!["local".into(), "server".into()],
                restored.host_id,
                restored.all
            ),
            saved
        );
    }

    #[test]
    fn identical_names_and_ids_remain_independently_addressable() {
        let mut second = list();
        second.has_more = true;
        second.loading_projects = true;
        second.hidden_projects = list().projects;
        let combined =
            combine_connection_lists([("laptop".into(), list()), ("server".into(), second)])
                .unwrap();
        assert_eq!(combined.projects.len(), 2);
        assert_eq!(combined.projects[0].threads[0].host_id, "laptop");
        assert_eq!(combined.projects[1].threads[0].host_id, "server");
        assert_eq!(combined.hidden_projects[0].host_id, "server");
        assert_eq!(combined.threads[0].host_id, "laptop");
        assert_eq!(combined.threads[1].host_id, "server");
        assert_eq!(combined.threads[0].id, combined.threads[1].id);
        assert!(combined.has_more);
        assert!(combined.loading_projects);
        assert!(combine_connection_lists([]).is_none());
    }

    #[test]
    fn partial_results_keep_errors_and_independent_paging_and_loading() {
        let mut first = list();
        first.notice = Some("Provider unavailable".into());
        let mut second = list();
        second.notice = Some("Page failed".into());
        second.loading_chats = true;
        second.has_more_projects = true;
        let combined =
            combine_connection_lists([("one".into(), first), ("two".into(), second)]).unwrap();
        assert_eq!(
            combined.notice.as_deref(),
            Some("Provider unavailable\nPage failed")
        );
        assert!(combined.loading_chats);
        assert!(combined.has_more_projects);
        assert!(!combined.loading_projects);
        assert!(!combined.has_more);

        let mut first = list();
        first.notice = Some("Same failure".into());
        let mut second = list();
        second.notice = first.notice.clone();
        let combined =
            combine_connection_lists([("one".into(), first), ("two".into(), second)]).unwrap();
        assert_eq!(combined.notice.as_deref(), Some("Same failure"));
    }
}

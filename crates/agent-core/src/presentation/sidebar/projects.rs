use crate::models::ProjectMembership;
use crate::presentation::list::{ProjectSummary, ThreadList};
use crate::state::{DraftKey, Navigation, Snapshot};

/// Only route identity and project membership participate in revealing a row;
/// streamed content and changing the filter itself do not reveal it again.
#[derive(Clone, Debug)]
pub struct ProjectRoute {
    pub host_id: String,
    pub draft: DraftKey,
    pub project: ProjectMembership,
}

/// Observe one Host independently of which Host is currently in front. A
/// background route is already seen; only a new foreground route may widen
/// scope, once its project metadata arrives.
#[derive(Clone, Debug, Default)]
pub struct ProjectReveal {
    draft: Option<DraftKey>,
    pending: bool,
}

impl ProjectReveal {
    pub fn observe(
        &self,
        draft: &DraftKey,
        project: &ProjectMembership,
        active: bool,
    ) -> (Self, bool) {
        let pending = active && (self.pending || self.draft.as_ref() != Some(draft));
        let known = !matches!(project, ProjectMembership::Unknown {});
        (
            Self {
                draft: Some(draft.clone()),
                pending: pending && !known,
            },
            pending && known,
        )
    }
}

impl Snapshot {
    /// A retained search page or unsuccessful refresh cannot establish deletion.
    pub fn sidebar_catalog_ready(&self) -> bool {
        use crate::state::operations::OperationKey;
        self.lists.list_query.search_term.trim().is_empty()
            && !self.operations.contains_key(&OperationKey::SessionList)
            && self.lists.subscription.is_some()
            && self.lists.replica.as_ref().is_some_and(|replica| {
                agent_protocol::conversation::records::catalog_ready(replica)
            })
    }

    pub fn sidebar_route(&self, host_id: String, navigation: &Navigation) -> ProjectRoute {
        let project = if !self.lists.list_query.search_term.trim().is_empty() {
            ProjectMembership::Unknown {}
        } else if let Some(id) = &navigation.thread_id {
            crate::state::projects::ChatPlacements::new(
                self.lists
                    .project_preferences
                    .get(&self.connection.storage_scope),
            )
            .get(id)
            .map(|project| ProjectMembership::Assigned(project.to_owned()))
            .or_else(|| {
                self.thread_metadata(id)
                    .map(|thread| thread.project_id.clone())
            })
            .unwrap_or_default()
        } else if matches!(&navigation.draft_key, DraftKey::Local { key } if key.is_empty())
            || !self.sidebar_catalog_ready()
        {
            ProjectMembership::Unknown {}
        } else if let Some(project) = self.directory_project(&navigation.cwd) {
            ProjectMembership::Assigned(project.id)
        } else if self
            .lists
            .threads
            .as_ref()
            .is_some_and(|list| !list.has_more_projects)
        {
            ProjectMembership::Unassigned {}
        } else {
            ProjectMembership::Unknown {}
        };
        ProjectRoute {
            host_id,
            draft: navigation.draft_key.clone(),
            project,
        }
    }
}

/// Whether a route being revealed needs to widen the effective project scope.
pub fn widens_scope(selected: &[ProjectKey], route: &ProjectRoute) -> bool {
    if selected.is_empty() {
        return false;
    }
    match &route.project {
        ProjectMembership::Unknown {} => false,
        ProjectMembership::Unassigned {} => true,
        ProjectMembership::Assigned(id) => {
            !selected.iter().any(|key| key.matches(&route.host_id, id))
        }
    }
}

/// Project IDs are local to a Host, including in a combined sidebar.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ProjectKey {
    pub host_id: String,
    pub project_id: String,
}

impl ProjectKey {
    fn matches(&self, host: &str, project: &str) -> bool {
        self.host_id == host && self.project_id == project
    }
}

/// A partial page, search result or unavailable Host cannot establish that a
/// project was deleted. An explicitly hidden project is already known to be
/// unavailable even while the rest of the catalog is still loading.
/// Catalogs are uncombined per-Host lists. `ready` covers source availability;
/// paging, loading and partial-result errors are resolved here.
pub fn resolve_projects(
    selected: &[ProjectKey],
    catalogs: &[(&str, &ThreadList)],
    ready: bool,
) -> Vec<ProjectKey> {
    let complete = ready
        && !catalogs.is_empty()
        && catalogs.iter().all(|(_, list)| {
            !list.loading_projects && !list.has_more_projects && list.notice.is_none()
        });
    let contains = |key: &ProjectKey, hidden: bool| {
        catalogs.iter().any(|(host, list)| {
            let projects = if hidden {
                &list.hidden_projects
            } else {
                &list.projects
            };
            projects
                .iter()
                .any(|project| key.matches(host, &project.id))
        })
    };
    selected
        .iter()
        .filter(|key| contains(key, false) || (!complete && !contains(key, true)))
        .cloned()
        .collect()
}

/// A normal selection replaces the scope; Shift toggles one project. An empty
/// scope means all projects, so toggling its last project returns to All.
pub fn select_projects(
    selected: &[ProjectKey],
    project: Option<ProjectKey>,
    additive: bool,
) -> Vec<ProjectKey> {
    let Some(project) = project else {
        return Vec::new();
    };
    if !additive {
        return vec![project];
    }
    let mut next = selected.to_vec();
    if selected.contains(&project) {
        next.retain(|key| key != &project);
    } else {
        next.push(project);
    }
    next
}

/// Hold an open panel's order, retaining missing keys for their return and
/// appending newly arrived projects in their incoming order.
pub fn order_projects<'a>(
    projects: &'a [ProjectSummary],
    previous: &[ProjectKey],
) -> (Vec<ProjectKey>, Vec<&'a ProjectSummary>) {
    use std::collections::{HashMap, HashSet};
    let key = |project: &ProjectSummary| ProjectKey {
        host_id: project.host_id.clone(),
        project_id: project.id.clone(),
    };
    let known = previous.iter().collect::<HashSet<_>>();
    let mut order = previous.to_vec();
    order.extend(projects.iter().map(key).filter(|key| !known.contains(key)));
    let mut remaining = projects
        .iter()
        .map(|project| (key(project), project))
        .collect::<HashMap<_, _>>();
    let ordered = order
        .iter()
        .filter_map(|key| remaining.remove(key))
        .collect();
    (order, ordered)
}

pub fn matching_projects<'a>(
    projects: impl IntoIterator<Item = &'a ProjectSummary>,
    query: &str,
) -> Vec<&'a ProjectSummary> {
    let query = query.trim().to_lowercase();
    projects
        .into_iter()
        .filter(|project| project.name.to_lowercase().contains(&query))
        .collect()
}

/// Apply the resolved project selection to the visible rows. Recovery and errors remain
/// available. A missing selected project keeps catalog paging available;
/// subsequent pages are scoped again, so unrelated rows stay out.
pub fn scoped_list(list: &ThreadList, selected: &[ProjectKey]) -> ThreadList {
    let mut result = list.clone();
    if selected.is_empty() {
        return result;
    }
    let includes = |host: &str, id: &str| selected.iter().any(|key| key.matches(host, id));
    result
        .projects
        .retain(|project| includes(&project.host_id, &project.id));
    result.threads.retain(|thread| {
        thread
            .project_id
            .as_deref()
            .is_some_and(|id| includes(&thread.host_id, id))
    });
    result.has_more = false;
    result.has_more_projects = list.has_more_projects
        && selected.iter().any(|key| {
            !result
                .projects
                .iter()
                .any(|project| key.matches(&project.host_id, &project.id))
        });
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presentation::connection_list::combine_connection_lists;
    use crate::state::Snapshot;
    use serde_json::json;

    fn key(host: &str, project: &str) -> ProjectKey {
        ProjectKey {
            host_id: host.into(),
            project_id: project.into(),
        }
    }

    #[test]
    fn an_open_panel_keeps_its_order_across_pins_arrivals_and_temporarily_missing_projects() {
        let project = |host: &str, id: &str| {
            let mut project = list().projects.remove(0);
            project.host_id = host.into();
            project.id = id.into();
            project
        };
        let a = project("host", "a");
        let b = project("host", "b");
        let c = project("other-host", "a");
        let keys = |projects: Vec<&ProjectSummary>| {
            projects
                .into_iter()
                .map(|project| key(&project.host_id, &project.id))
                .collect::<Vec<_>>()
        };
        let initial = [a.clone(), b.clone()];
        let (order, shown) = order_projects(&initial, &[]);
        assert_eq!(keys(shown), vec![key("host", "a"), key("host", "b")]);
        let arrived = [c.clone(), b.clone(), a.clone()];
        let (order, shown) = order_projects(&arrived, &order);
        assert_eq!(
            keys(shown),
            vec![key("host", "a"), key("host", "b"), key("other-host", "a")]
        );
        let (order, shown) = order_projects(&[], &order);
        assert!(shown.is_empty());
        let only_arrived = [c.clone()];
        let (order, shown) = order_projects(&only_arrived, &order);
        assert_eq!(keys(shown), vec![key("other-host", "a")]);
        let (_, shown) = order_projects(&arrived, &order);
        assert_eq!(
            keys(shown),
            vec![key("host", "a"), key("host", "b"), key("other-host", "a")]
        );
        let (_, shown) = order_projects(&arrived, &[]);
        assert_eq!(
            keys(shown),
            vec![key("other-host", "a"), key("host", "b"), key("host", "a")],
            "reopening uses the new pin order"
        );
    }

    #[test]
    fn opening_a_moved_chat_reveals_its_local_destination_instead_of_its_original_project() {
        use crate::state::{Event, Intent, reduce};
        let id: crate::session::SessionRef =
            serde_json::from_value(json!({"provider":"codex", "id":"moved"})).unwrap();
        for original in [None, Some("source")] {
            let mut snapshot = Snapshot::default();
            snapshot.connection.storage_scope = "account".into();
            snapshot.lists.threads = Some(std::sync::Arc::new(
                serde_json::from_value(json!({
                    "projects": [{"id":"source", "name":"Source", "roots":[]},
                                 {"id":"destination", "name":"Destination", "roots":[]}],
                    "data": [{"id":id, "projectId":original}], "limit":60,
                    "hasMore":false, "hasMoreProjects":false
                }))
                .unwrap(),
            ));
            let moved = reduce(
                &snapshot,
                "",
                Event::Intent(Intent::MoveChatToProject {
                    thread_id: id.clone(),
                    project_id: "destination".into(),
                }),
            )
            .0;
            let navigation = Navigation {
                thread_id: Some(id.clone()),
                draft_key: "moved".into(),
                ..Default::default()
            };
            let route = moved.sidebar_route("host".into(), &navigation);
            assert_eq!(
                route.project,
                ProjectMembership::Assigned("destination".into())
            );
            assert!(!widens_scope(&[key("host", "destination")], &route));
            assert!(widens_scope(&[key("host", "source")], &route));
            let mut other_account = moved;
            other_account.connection.storage_scope = "other-account".into();
            assert_eq!(
                other_account
                    .sidebar_route("host".into(), &navigation)
                    .project,
                original
                    .map(|project| ProjectMembership::Assigned(project.into()))
                    .unwrap_or(ProjectMembership::Unassigned {}),
                "local placement belongs to its connection account"
            );
        }
    }

    #[test]
    fn a_revealed_route_widens_only_a_nonempty_scope_that_excludes_its_project() {
        let selected = [key("host", "repo")];
        let route = ProjectRoute {
            host_id: "host".into(),
            draft: "new:/repo".into(),
            project: ProjectMembership::Assigned("repo".into()),
        };
        assert!(!widens_scope(&selected, &route));
        let outside = ProjectRoute {
            draft: "new:/other".into(),
            project: ProjectMembership::Assigned("other".into()),
            ..route.clone()
        };
        assert!(widens_scope(&selected, &outside));
        assert!(!widens_scope(&[], &outside));
        assert!(!widens_scope(
            &[key("host", "repo"), key("host", "other")],
            &outside
        ));
        assert!(widens_scope(
            &selected,
            &ProjectRoute {
                host_id: "another-host".into(),
                ..route.clone()
            }
        ));
        assert!(widens_scope(
            &selected,
            &ProjectRoute {
                project: ProjectMembership::Unassigned {},
                ..route.clone()
            }
        ));
        let pending = ProjectRoute {
            project: ProjectMembership::Unknown {},
            ..outside.clone()
        };
        assert!(!widens_scope(&selected, &pending));
    }

    #[test]
    fn reveals_wait_for_foreground_metadata_and_ignore_previously_observed_background_routes() {
        let one = DraftKey::from("one");
        let two = DraftKey::from("two");
        let known = ProjectMembership::Assigned("repo".into());
        let unknown = ProjectMembership::Unknown {};
        let (seen, reveal) = ProjectReveal::default().observe(&one, &unknown, true);
        assert!(!reveal);
        let (seen, reveal) = seen.observe(&one, &known, true);
        assert!(reveal, "a new route waits for its metadata");
        let (seen, reveal) = seen.observe(&one, &unknown, true);
        assert!(!reveal);
        let (seen, reveal) = seen.observe(&one, &known, true);
        assert!(
            !reveal,
            "transient metadata loss or UI remount is not navigation"
        );
        let (seen, reveal) = seen.observe(&two, &unknown, true);
        assert!(!reveal);
        let (seen, reveal) = seen.observe(&one, &known, true);
        assert!(
            reveal,
            "returning after an unresolved route is new navigation"
        );
        let (seen, reveal) = seen.observe(&two, &unknown, false);
        assert!(!reveal);
        let (seen, reveal) = seen.observe(&two, &known, true);
        assert!(
            !reveal,
            "a background route remains seen when its Host becomes active"
        );
        let (seen, reveal) = seen.observe(&one, &unknown, true);
        assert!(!reveal);
        let (seen, reveal) = seen.observe(&one, &known, true);
        assert!(reveal, "the Host's own new navigation is revealed");
        let (_, reveal) = seen.observe(&one, &ProjectMembership::Unassigned {}, true);
        assert!(!reveal, "metadata-only changes do not navigate");
    }

    #[test]
    fn route_membership_waits_for_history_or_catalog_and_recognizes_project_drafts() {
        let mut snapshot = Snapshot::default();
        let default_route = snapshot.sidebar_route(
            "host".into(),
            &crate::state::Snapshot::default().panes.primary().navigation,
        );
        assert_eq!(default_route.project, ProjectMembership::Unknown {});
        let navigation = Navigation {
            cwd: "/repo".into(),
            draft_key: "new:/repo".into(),
            ..Default::default()
        };
        assert_eq!(
            snapshot.sidebar_route("host".into(), &navigation).project,
            ProjectMembership::Unknown {}
        );
        snapshot.lists.threads = Some(std::sync::Arc::new(
            serde_json::from_value(json!({
                "projects": [{"id":"repo", "name":"Repo", "roots":[{"path":"/repo"}]}, {"id":"z-later", "name":"Later", "roots":[{"path":"/later"}]}],
                "data": [], "limit":60, "hasMore":false, "hasMoreProjects":true
            }))
            .unwrap(),
        ));
        let page = snapshot.lists.threads.as_ref().unwrap().as_ref().clone();
        std::sync::Arc::make_mut(&mut snapshot.lists.list_query).project_limit = 1;
        crate::state::list_sync::seed(&mut snapshot, page);
        snapshot.lists.subscription = Some(uuid::Uuid::new_v4());
        let in_project = snapshot.sidebar_route("host".into(), &navigation);
        assert_eq!(in_project.host_id, "host");
        assert_eq!(in_project.draft, navigation.draft_key);
        assert_eq!(
            in_project.project,
            ProjectMembership::Assigned("repo".into())
        );
        let standalone = Navigation {
            cwd: String::new(),
            draft_key: "new:".into(),
            ..Default::default()
        };
        assert_eq!(
            snapshot.sidebar_route("host".into(), &standalone).project,
            ProjectMembership::Unknown {}
        );
        std::sync::Arc::make_mut(&mut snapshot.lists.list_query).project_limit = 2;
        crate::state::list_sync::project(&mut snapshot, false);
        assert_eq!(
            snapshot.sidebar_route("host".into(), &standalone).project,
            ProjectMembership::Unassigned {}
        );
        assert_eq!(
            snapshot
                .sidebar_route("host".into(), &Navigation::default())
                .project,
            ProjectMembership::Unknown {},
            "an initial empty composer does not discard restored scope"
        );
        let id = serde_json::from_value(json!({"provider":"codex", "id":"linked-thread"})).unwrap();
        let linked = Navigation {
            thread_id: Some(id),
            draft_key: "linked".into(),
            ..navigation
        };
        let pending = snapshot.sidebar_route("host".into(), &linked);
        assert_eq!(pending.project, ProjectMembership::Unknown {});
        let thread: crate::models::Thread = serde_json::from_value(json!({
            "id": linked.thread_id, "projectId":"other", "cwd":"/repo/worktree"
        }))
        .unwrap();
        std::sync::Arc::make_mut(&mut snapshot.sessions.conversations).insert(
            linked.thread_id.clone().unwrap(),
            std::sync::Arc::new(thread),
        );
        let opened = snapshot.sidebar_route("host".into(), &linked);
        assert_eq!(
            opened.project,
            ProjectMembership::Assigned("other".into()),
            "registered membership takes precedence over execution cwd"
        );
        assert!(widens_scope(&[key("host", "repo")], &opened));
    }

    fn list() -> ThreadList {
        let mut snapshot = Snapshot::default();
        snapshot.lists.threads = Some(std::sync::Arc::new(
            serde_json::from_value(json!({
                "projects": [{"id": "repo", "name": "日本語 Project", "roots": []}],
                "data": [{"id":{"provider":"codex","id":"chat"}, "name":"Standalone"}],
                "limit": 60, "hasMore": true, "hasMoreProjects": true
            }))
            .unwrap(),
        ));
        snapshot.thread_list().unwrap()
    }

    #[test]
    fn search_results_and_pending_or_failed_refreshes_cannot_assign_draft_membership() {
        use crate::state::operations::{OperationKey, OperationPhase, OperationState};
        let mut snapshot = Snapshot::default();
        snapshot.lists.threads = Some(std::sync::Arc::new(
            serde_json::from_value(json!({
                "projects": [], "data": [], "limit":60, "hasMore":false,
                "hasMoreProjects":false
            }))
            .unwrap(),
        ));
        let page = snapshot.lists.threads.as_ref().unwrap().as_ref().clone();
        crate::state::list_sync::seed(&mut snapshot, page);
        snapshot.lists.subscription = Some(uuid::Uuid::new_v4());
        let navigation = Navigation {
            cwd: "/repo".into(),
            draft_key: "new:/repo".into(),
            ..Default::default()
        };
        std::sync::Arc::make_mut(&mut snapshot.lists.list_query).search_term = "no matches".into();
        assert_eq!(
            snapshot.sidebar_route("host".into(), &navigation).project,
            ProjectMembership::Unknown {}
        );
        std::sync::Arc::make_mut(&mut snapshot.lists.list_query)
            .search_term
            .clear();
        for phase in [
            OperationPhase::Running,
            OperationPhase::Failed {
                message: "offline".into(),
            },
        ] {
            std::sync::Arc::make_mut(&mut snapshot.operations).insert(
                OperationKey::SessionList,
                OperationState {
                    generation: 1,
                    phase,
                },
            );
            assert_eq!(
                snapshot.sidebar_route("host".into(), &navigation).project,
                ProjectMembership::Unknown {},
                "clearing search retains its old response until a successful refresh"
            );
        }
        std::sync::Arc::make_mut(&mut snapshot.operations).clear();
        snapshot.lists.subscription = None;
        assert_eq!(
            snapshot.sidebar_route("host".into(), &navigation).project,
            ProjectMembership::Unknown {}
        );
        snapshot.lists.subscription = Some(uuid::Uuid::new_v4());
        assert_eq!(
            snapshot.sidebar_route("host".into(), &navigation).project,
            ProjectMembership::Unassigned {}
        );
    }

    #[test]
    fn selecting_replacing_and_shift_toggling_never_duplicate_projects() {
        use std::slice::from_ref;
        let one = key("one", "repo");
        let two = key("two", "repo");
        assert_eq!(
            select_projects(&[], Some(one.clone()), false),
            from_ref(&one)
        );
        assert_eq!(
            select_projects(from_ref(&one), Some(two.clone()), true),
            [one.clone(), two.clone()]
        );
        assert_eq!(
            select_projects(&[one.clone(), two.clone()], Some(one.clone()), true),
            from_ref(&two)
        );
        assert_eq!(
            select_projects(&[one.clone(), two.clone()], Some(one.clone()), false),
            from_ref(&one)
        );
        assert!(select_projects(&[one], Some(key("one", "repo")), true).is_empty());
        assert!(select_projects(&[two], None, true).is_empty());
    }

    #[test]
    fn hiding_or_deleting_the_scoped_project_returns_to_all_projects_and_standalone_chats() {
        let mut all = combine_connection_lists([("host".into(), list())]).unwrap();
        all.has_more_projects = false;
        let mut other = all.projects[0].clone();
        other.id = "other".into();
        all.projects.push(other);
        let saved = [key("host", "repo")];
        let selection = resolve_projects(&saved, &[("host", &all)], true);
        assert_eq!(scoped_list(&all, &selection).projects.len(), 1);
        all.hidden_projects.push(all.projects.remove(0));
        let selection = resolve_projects(&saved, &[("host", &all)], false);
        assert!(
            selection.is_empty(),
            "a known hidden project is removed even from a partial list"
        );
        let visible = scoped_list(&all, &selection);
        assert_eq!(visible.projects[0].id, "other");
        assert_eq!(visible.threads.len(), 1);
        assert!(visible.has_more);
        all.hidden_projects.clear();
        assert!(resolve_projects(&saved, &[("host", &all)], true).is_empty());
    }

    #[test]
    fn a_project_on_a_later_page_keeps_catalog_paging_until_all_selected_projects_are_loaded() {
        let mut all = combine_connection_lists([("host".into(), list())]).unwrap();
        let saved = [key("host", "later")];
        let selection = resolve_projects(&saved, &[("host", &all)], false);
        let visible = scoped_list(&all, &selection);
        assert!(visible.projects.is_empty());
        assert!(visible.has_more_projects);
        assert!(scoped_list(&all, &[key("host", "repo"), key("host", "later")]).has_more_projects);
        let mut later = all.projects[0].clone();
        later.id = "later".into();
        all.projects.push(later);
        let visible = scoped_list(&all, &selection);
        assert_eq!(visible.projects.len(), 1);
        assert_eq!(visible.projects[0].id, "later");
        assert!(!visible.has_more_projects);
        assert!(!scoped_list(&all, &[key("host", "repo"), key("host", "later")]).has_more_projects);
        all.has_more_projects = false;
        assert!(!scoped_list(&all, &[key("host", "gone")]).has_more_projects);
    }

    #[test]
    fn partial_loading_search_and_offline_results_preserve_unknown_selection_until_authoritative() {
        let mut all = combine_connection_lists([("host".into(), list())]).unwrap();
        let saved = [
            key("host", "repo"),
            key("other-host", "repo"),
            key("host", "other"),
        ];
        assert_eq!(
            resolve_projects(&saved, &[("host", &all)], true),
            saved,
            "paging is not authoritative"
        );
        all.has_more_projects = false;
        assert_eq!(resolve_projects(&saved, &[("host", &all)], false), saved);
        assert_eq!(
            resolve_projects(&saved, &[("host", &all)], true),
            [key("host", "repo")]
        );
        assert_eq!(
            resolve_projects(&saved, &[], true),
            saved,
            "no catalog is still loading"
        );
        let mut empty = all.clone();
        empty.projects.clear();
        assert!(resolve_projects(&saved, &[("host", &empty)], true).is_empty());
        let selection = resolve_projects(&saved[..1], &[], false);
        let mut search_results = all;
        search_results.projects.clear();
        let scoped = scoped_list(&search_results, &selection);
        assert!(scoped.projects.is_empty());
        assert!(
            scoped.threads.is_empty(),
            "a search must not widen the project scope"
        );
    }

    #[test]
    fn effective_scope_uses_each_hosts_metadata_and_waits_for_loading_or_partial_results() {
        let mut one = list();
        one.has_more_projects = false;
        let two = one.clone();
        let saved = [
            key("one", "repo"),
            key("two", "repo"),
            key("one", "missing"),
        ];
        assert_eq!(
            resolve_projects(&saved, &[("one", &one), ("two", &two)], true),
            saved[..2]
        );
        one.loading_projects = true;
        assert_eq!(
            resolve_projects(&saved, &[("one", &one), ("two", &two)], true),
            saved
        );
        one.loading_projects = false;
        one.notice = Some("partial result".into());
        assert_eq!(
            resolve_projects(&saved, &[("one", &one), ("two", &two)], true),
            saved
        );
        one.hidden_projects.push(one.projects.remove(0));
        assert_eq!(
            resolve_projects(&saved[..2], &[("one", &one), ("two", &two)], false),
            [key("two", "repo")]
        );
    }

    #[test]
    fn scope_keeps_host_identity_errors_and_recovery_without_mutating_the_source() {
        let mut source = list();
        source.notice = Some("Host unavailable".into());
        source.hidden_projects = source.projects.clone();
        let all =
            combine_connection_lists([("one".into(), source), ("two".into(), list())]).unwrap();
        let scoped = scoped_list(&all, &[key("two", "repo")]);
        assert_eq!(scoped.projects.len(), 1);
        assert_eq!(scoped.projects[0].host_id, "two");
        assert_eq!(scoped.hidden_projects.len(), 1);
        assert_eq!(scoped.notice.as_deref(), Some("Host unavailable"));
        assert!(!scoped.has_more);
        assert!(!scoped.has_more_projects);
        assert_eq!(all.projects.len(), 2);
        assert!(all.has_more);
        assert_eq!(scoped_list(&all, &[]).projects.len(), 2);
        assert_eq!(
            scoped_list(&all, &[key("one", "repo"), key("two", "repo")])
                .projects
                .len(),
            2
        );
        assert!(
            scoped_list(&all, &[key("gone", "repo")])
                .projects
                .is_empty()
        );
    }

    #[test]
    fn scoping_standalone_rows_requires_both_project_and_host_and_preserves_project_paging() {
        let mut source = list();
        source.projects[0].has_more = true;
        let standalone = source.threads[0].clone();
        source.threads[0].project_id = Some("repo".into());
        source.threads.push(standalone);
        let all =
            combine_connection_lists([("one".into(), source.clone()), ("two".into(), source)])
                .unwrap();
        let scoped = scoped_list(&all, &[key("two", "repo")]);
        assert_eq!(scoped.threads.len(), 1);
        assert_eq!(scoped.threads[0].host_id, "two");
        assert!(scoped.projects[0].has_more);
        assert_eq!(all.threads.len(), 4);
        assert!(scoped_list(&all, &[]).has_more_projects);
        assert_eq!(scoped_list(&all, &[]).threads.len(), 4);
    }

    #[test]
    fn project_search_trims_and_matches_unicode_names_without_reordering() {
        let mut list = list();
        let mut other = list.projects[0].clone();
        other.id = "other".into();
        other.name = "Project Two".into();
        list.projects.push(other);
        assert_eq!(
            matching_projects(&list.projects, " PROJECT ")
                .iter()
                .map(|project| project.id.as_str())
                .collect::<Vec<_>>(),
            ["repo", "other"]
        );
        assert_eq!(matching_projects(&list.projects, "日本語").len(), 1);
        assert_eq!(matching_projects(&list.projects, "  ").len(), 2);
        assert!(matching_projects(&list.projects, "missing").is_empty());
    }
}

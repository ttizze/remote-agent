//! Reversible, device-local project organization. Provider files remain unchanged.
use super::*;
use std::collections::HashMap;

pub(crate) fn project_section(section: &str) -> Option<&str> {
    let section = section.trim();
    (section.chars().count() <= 60).then_some(section)
}

pub fn validate_project_labels<'a>(
    name: &'a str,
    section: &'a str,
) -> Result<(&'a str, &'a str), &'static str> {
    let name = name.trim();
    match project_section(section) {
        Some(section) if !name.is_empty() && name.chars().count() <= 100 => Ok((name, section)),
        _ => Err("表示名は1〜100文字、セクションは60文字以内で入力してください"),
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlacedChat {
    pub id: crate::session::SessionRef,
    /// A label for the user's saved shortcut; live metadata takes precedence.
    pub title: String,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProjectPreferences {
    pub name: Option<String>,
    pub pinned: bool,
    pub section: Option<String>,
    pub hidden: bool,
    pub archived_before: Option<i64>,
    pub archived_ids: BTreeSet<crate::session::SessionRef>,
    #[serde(default)]
    pub order: Option<usize>,
    #[serde(default)]
    pub placed_chats: Vec<PlacedChat>,
}

pub(crate) struct ChatPlacements<'a>(HashMap<&'a crate::session::SessionRef, &'a str>);

impl<'a> ChatPlacements<'a> {
    pub(crate) fn new(preferences: Option<&'a BTreeMap<String, ProjectPreferences>>) -> Self {
        let mut indexed = HashMap::new();
        for (project, preferences) in preferences.into_iter().flat_map(|prefs| prefs.iter()) {
            for chat in &preferences.placed_chats {
                // Duplicate shortcuts keep the first project in saved key order.
                indexed.entry(&chat.id).or_insert(project.as_str());
            }
        }
        Self(indexed)
    }

    pub(crate) fn get(&self, id: &crate::session::SessionRef) -> Option<&'a str> {
        self.0.get(id).copied()
    }
}

pub(crate) fn project_chat_visible(
    archived_before: Option<i64>,
    archived_ids: Option<&BTreeSet<crate::session::SessionRef>>,
    id: Option<&crate::session::SessionRef>,
    active: bool,
    updated_at: Option<f64>,
    placed: bool,
) -> bool {
    active
        || (!id.is_some_and(|id| archived_ids.is_some_and(|ids| ids.contains(id)))
            && (placed
                || !archived_before
                    .zip(updated_at)
                    .is_some_and(|(before, at)| at.is_finite() && at <= before as f64)))
}

impl Snapshot {
    pub(crate) fn project_preference(&self, id: &str) -> Option<&ProjectPreferences> {
        self.lists
            .project_preferences
            .get(&self.connection.storage_scope)?
            .get(id)
    }
}
pub(super) fn reorder(snapshot: &mut Snapshot, id: &str, target: &str, after: bool) {
    if id == target {
        return;
    }
    let Some(list) = snapshot.thread_list() else {
        return;
    };
    let Some(source_index) = list.projects.iter().position(|p| p.id == id) else {
        return;
    };
    let Some(destination) = list.projects.iter().find(|p| p.id == target) else {
        return;
    };
    let (pinned, section) = (destination.pinned, destination.section.clone());
    let mut ids = list
        .projects
        .iter()
        .map(|p| p.id.clone())
        .collect::<Vec<_>>();
    let moved = ids.remove(source_index);
    let at = ids.iter().position(|p| p == target).unwrap() + usize::from(after);
    ids.insert(at, moved);
    update(snapshot, id, |p| {
        p.pinned = pinned;
        p.section = section;
    });
    for (order, id) in ids.iter().enumerate() {
        update(snapshot, id, |p| p.order = Some(order));
    }
}

pub(super) fn move_chat(snapshot: &mut Snapshot, id: &crate::session::SessionRef, project: &str) {
    if !snapshot
        .lists
        .threads
        .as_ref()
        .is_some_and(|list| list.projects.iter().any(|p| p.id == project))
    {
        return;
    }
    let title = if let Some(thread) = snapshot.thread_metadata(id) {
        if thread.parent_id.is_some() {
            return;
        }
        crate::models::task_title(thread.name.as_deref(), thread.preview.as_deref())
            .unwrap_or_default()
            .to_owned()
    } else if let Some(chat) = snapshot
        .lists
        .project_preferences
        .get(&snapshot.connection.storage_scope)
        .into_iter()
        .flat_map(|p| p.values())
        .flat_map(|p| &p.placed_chats)
        .find(|chat| &chat.id == id)
    {
        chat.title.clone()
    } else {
        return;
    };
    let preferences = Arc::make_mut(&mut snapshot.lists.project_preferences)
        .entry(snapshot.connection.storage_scope.clone())
        .or_default();
    for p in preferences.values_mut() {
        p.placed_chats.retain(|chat| &chat.id != id);
    }
    let destination = preferences.entry(project.into()).or_default();
    destination.archived_ids.remove(id);
    destination.placed_chats.push(PlacedChat {
        id: id.clone(),
        title,
    });
    // Make the result visible even when the destination has never been opened.
    Arc::make_mut(&mut snapshot.lists.expanded_projects)
        .entry(project.into())
        .or_insert(5);
}
pub(super) fn update(
    snapshot: &mut Snapshot,
    id: &str,
    change: impl FnOnce(&mut ProjectPreferences),
) {
    if !snapshot
        .lists
        .threads
        .as_ref()
        .is_some_and(|list| list.projects.iter().any(|p| p.id == id))
    {
        return;
    }
    let preferences = Arc::make_mut(&mut snapshot.lists.project_preferences)
        .entry(snapshot.connection.storage_scope.clone())
        .or_default()
        .entry(id.into())
        .or_default();
    change(preferences);
}
pub(super) fn archive(snapshot: &mut Snapshot, project: &str, before: i64) {
    if before <= 0 {
        return;
    }
    let ids = snapshot
        .lists
        .project_threads
        .get(project)
        .into_iter()
        .flat_map(|page| &page.data)
        .chain(
            snapshot
                .lists
                .threads
                .iter()
                .flat_map(|page| &page.data)
                .filter(|t| t.project_id.as_ref().is_some_and(|id| id == project)),
        )
        .filter(|thread| {
            !crate::models::task_active(
                thread
                    .id
                    .as_ref()
                    .and_then(|id| snapshot.lists.activity.active.get(id).copied()),
                thread.status,
            )
        })
        .filter_map(|thread| thread.id.clone())
        .chain(
            snapshot
                .project_preference(project)
                .into_iter()
                .flat_map(|p| &p.placed_chats)
                .filter(|chat| {
                    !crate::models::task_active(
                        snapshot.lists.activity.active.get(&chat.id).copied(),
                        snapshot
                            .thread_metadata(&chat.id)
                            .map_or_default(|t| t.status),
                    )
                })
                .map(|chat| chat.id.clone()),
        )
        .collect::<BTreeSet<_>>();
    update(snapshot, project, |p| {
        p.archived_before = Some(p.archived_before.unwrap_or_default().max(before));
        p.archived_ids.extend(ids);
    });
}
#[cfg(test)]
mod tests {
    #[test]
    fn project_labels_accept_unicode_boundaries_and_reject_empty_or_overlong_values() {
        assert_eq!(
            super::validate_project_labels("  日本語 👋  ", "  個人  "),
            Ok(("日本語 👋", "個人"))
        );
        assert!(super::validate_project_labels(&"名".repeat(100), &"節".repeat(60)).is_ok());
        for (name, section) in [
            ("  ".to_string(), String::new()),
            ("名".repeat(101), String::new()),
            ("有効".into(), "節".repeat(61)),
        ] {
            assert_eq!(
                super::validate_project_labels(&name, &section),
                Err("表示名は1〜100文字、セクションは60文字以内で入力してください")
            );
        }
    }

    use super::*;
    use serde_json::json;
    fn fixture() -> Snapshot {
        let list: ThreadList = serde_json::from_value(json!({
            "data":[],"projects":[{"id":"a","name":"Alpha","roots":[{"path":"C:/alpha"}]},{"id":"b","name":"Beta","roots":[]}],
            "hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5
        })).unwrap();
        Snapshot {
            connection: crate::state::Connection {
                storage_scope: "host-one".into(),
                ..Default::default()
            },
            lists: crate::state::Lists {
                threads: Some(Arc::new(list)),
                ..Default::default()
            },
            ..Default::default()
        }
    }
    fn dispatch(s: &Snapshot, intent: Intent) -> Snapshot {
        let (next, effects) = reduce(s, "", Event::Intent(intent));
        assert!(
            effects.is_empty(),
            "organization must not write to the provider or Host"
        );
        next
    }
    fn task(id: &str, at: Option<f64>, status: &str) -> Thread {
        serde_json::from_value(json!({"id":{"provider":"claude","id":id},"projectId":"a","updatedAt":at,"status":status})).unwrap()
    }
    #[test]
    fn saved_shortcuts_keep_raw_titles_and_localize_only_missing_titles() {
        for (name, expected) in [
            (None, "Untitled task"),
            (Some("無題のタスク"), "無題のタスク"),
            (Some("設定{0}"), "設定{0}"),
        ] {
            let mut state = fixture();
            let pages = state.lists.threads.clone();
            let mut thread = task("shortcut", None, "idle");
            thread.name = name.map(str::to_owned);
            let id = thread.id.clone().unwrap();
            Arc::make_mut(state.lists.threads.as_mut().unwrap()).data = vec![thread];
            let moved = dispatch(
                &state,
                Intent::MoveChatToProject {
                    thread_id: id,
                    project_id: "b".into(),
                },
            );
            let mut restored =
                crate::persistence::decode(&crate::persistence::encode(&moved).unwrap()).unwrap();
            restored.lists.threads = pages;
            Arc::make_mut(&mut restored.lists.expanded_projects).insert("b".into(), 5);
            restored.catalog.ui_language = crate::localization::UiLanguage::English;
            let list = restored.thread_list().unwrap();
            assert_eq!(
                list.projects
                    .iter()
                    .find(|project| project.id == "b")
                    .unwrap()
                    .threads[0]
                    .title,
                expected
            );
        }
    }

    #[test]
    fn project_drop_reorders_both_directions_and_adopts_the_destination_group() {
        let s = fixture();
        let s = dispatch(
            &s,
            Intent::ReorderProject {
                project_id: "a".into(),
                target_id: "b".into(),
                after: true,
            },
        );
        assert_eq!(
            s.thread_list()
                .unwrap()
                .projects
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>(),
            ["b", "a"]
        );
        let s = dispatch(
            &s,
            Intent::SetProjectPinned {
                project_id: "b".into(),
                pinned: true,
            },
        );
        let s = dispatch(
            &s,
            Intent::SetProjectSection {
                project_id: "b".into(),
                section: "Work".into(),
            },
        );
        let s = dispatch(
            &s,
            Intent::ReorderProject {
                project_id: "a".into(),
                target_id: "b".into(),
                after: false,
            },
        );
        let list = s.thread_list().unwrap();
        assert_eq!(
            list.projects
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert!(list.projects[0].pinned);
        assert_eq!(list.projects[0].section.as_deref(), Some("Work"));
        let mut restored =
            crate::persistence::decode(&crate::persistence::encode(&s).unwrap()).unwrap();
        restored.lists.threads = s.lists.threads.clone();
        assert_eq!(restored.thread_list().unwrap().projects[0].id, "a");
        restored.connection.storage_scope = "host-two".into();
        assert!(
            restored
                .thread_list()
                .unwrap()
                .projects
                .iter()
                .all(|p| !p.pinned)
        );
        for (id, target) in [("a", "a"), ("missing", "a"), ("a", "missing")] {
            assert_eq!(
                dispatch(
                    &s,
                    Intent::ReorderProject {
                        project_id: id.into(),
                        target_id: target.into(),
                        after: true
                    }
                )
                .lists
                .project_preferences,
                s.lists.project_preferences
            );
        }
    }

    #[test]
    fn moved_chats_survive_refresh_restart_and_reassignment_without_changing_native_history() {
        let mut s = fixture();
        let mut page = s.lists.threads.as_ref().unwrap().as_ref().clone();
        let mut thread = task("moving", Some(10.), "running");
        thread.name = Some("Moving chat".into());
        thread.cwd = Some("C:/alpha".into());
        let id = thread.id.clone().unwrap();
        page.data = vec![thread.clone()];
        Arc::make_mut(&mut s.lists.project_threads).insert("a".into(), Arc::new(page));
        Arc::make_mut(&mut s.lists.expanded_projects).insert("a".into(), 5);
        Arc::make_mut(&mut s.lists.activity)
            .unread
            .insert(id.clone());
        let native = s.lists.project_threads.clone();
        let s = dispatch(
            &s,
            Intent::MoveChatToProject {
                thread_id: id.clone(),
                project_id: "b".into(),
            },
        );
        let list = s.thread_list().unwrap();
        assert!(list.projects[0].threads.is_empty());
        assert_eq!(list.projects[1].threads.len(), 1);
        let moved = &list.projects[1].threads[0];
        assert_eq!(moved.project_id.as_deref(), Some("b"));
        assert!(moved.active && moved.unread);
        assert!(Arc::ptr_eq(&s.lists.project_threads, &native));
        assert_eq!(
            s.thread_metadata(&id).unwrap().cwd.as_deref(),
            Some("C:/alpha")
        );
        let mut restored =
            crate::persistence::decode(&crate::persistence::encode(&s).unwrap()).unwrap();
        restored.lists.threads = s.lists.threads.clone();
        Arc::make_mut(&mut restored.lists.expanded_projects).insert("b".into(), 5);
        assert!(restored.lists.project_threads.is_empty());
        let list = restored.thread_list().unwrap();
        assert_eq!(list.projects[1].threads[0].title, "Moving chat");
        assert!(
            !list.projects[1].threads[0].active,
            "saved shortcuts must not restore live status"
        );
        // Reassign even before the native source page has been fetched again.
        let restored = dispatch(
            &restored,
            Intent::MoveChatToProject {
                thread_id: id.clone(),
                project_id: "a".into(),
            },
        );
        let list = restored.thread_list().unwrap();
        assert!(list.projects[1].threads.is_empty());
        assert_eq!(list.projects[0].threads[0].id, id);
        let mut refreshed = restored.clone();
        refreshed.lists.project_threads = native;
        assert_eq!(
            refreshed.thread_list().unwrap().projects[0].threads.len(),
            1,
            "refresh must not duplicate a moved chat"
        );
        let mut other = s.clone();
        other.connection.storage_scope = "host-two".into();
        assert_eq!(
            other.thread_list().unwrap().projects[0].threads[0]
                .project_id
                .as_deref(),
            Some("a")
        );
        assert_eq!(
            dispatch(
                &s,
                Intent::MoveChatToProject {
                    thread_id: id,
                    project_id: "missing".into()
                }
            )
            .lists
            .project_preferences,
            s.lists.project_preferences
        );
    }
    #[test]
    fn moved_standalone_chats_keep_provider_identity_search_and_archive_behavior() {
        let mut s = fixture();
        let mut claude = task("same-id", Some(10.), "idle");
        claude.name = Some("Searchable task".into());
        claude.project_id = crate::models::ProjectMembership::Unassigned {};
        let mut codex = claude.clone();
        codex.id.as_mut().unwrap().provider = Some(crate::session::ProviderKind::Codex);
        let id = claude.id.clone().unwrap();
        Arc::make_mut(s.lists.threads.as_mut().unwrap()).data = vec![claude, codex];
        let s = dispatch(
            &s,
            Intent::ArchiveProjectChats {
                project_id: "b".into(),
                before: 100,
            },
        );
        let s = dispatch(
            &s,
            Intent::MoveChatToProject {
                thread_id: id.clone(),
                project_id: "b".into(),
            },
        );
        let list = s.thread_list().unwrap();
        assert_eq!(list.threads.len(), 1);
        assert_eq!(
            list.threads[0].id.provider,
            Some(crate::session::ProviderKind::Codex)
        );
        assert_eq!(
            list.projects[1].threads[0].id, id,
            "a new drop must be visible even after an earlier archive"
        );
        let mut search = s.clone();
        Arc::make_mut(&mut search.lists.list_query).search_term = "SEARCHABLE".into();
        assert_eq!(search.thread_list().unwrap().projects[1].threads.len(), 1);
        Arc::make_mut(&mut search.lists.list_query).search_term = "not matching".into();
        assert!(search.thread_list().unwrap().projects[1].threads.is_empty());
        let archived = dispatch(
            &s,
            Intent::ArchiveProjectChats {
                project_id: "b".into(),
                before: 200,
            },
        );
        assert!(
            archived.thread_list().unwrap().projects[1]
                .threads
                .is_empty()
        );
        let restored = dispatch(
            &archived,
            Intent::RestoreProjectChats {
                project_id: "b".into(),
            },
        );
        assert_eq!(restored.thread_list().unwrap().projects[1].threads.len(), 1);
        let mut unknown = id.clone();
        unknown.id = "missing".into();
        assert_eq!(
            dispatch(
                &s,
                Intent::MoveChatToProject {
                    thread_id: unknown,
                    project_id: "b".into()
                }
            )
            .lists
            .project_preferences,
            s.lists.project_preferences
        );
    }

    #[test]
    fn project_organization_is_reversible_persistent_and_host_scoped() {
        let s = fixture();
        let native = s.lists.threads.clone();
        let s = dispatch(
            &s,
            Intent::EditProject {
                project_id: "b".into(),
                name: "  Team  ".into(),
                section: "  Work  ".into(),
            },
        );
        let s = dispatch(
            &s,
            Intent::SetProjectPinned {
                project_id: "b".into(),
                pinned: true,
            },
        );
        let list = s.thread_list().unwrap();
        assert_eq!(list.projects[0].id, "b");
        assert_eq!(list.projects[0].name, "Team");
        assert_eq!(list.projects[0].section.as_deref(), Some("Work"));
        let s = dispatch(
            &s,
            Intent::SetProjectHidden {
                project_id: "b".into(),
                hidden: true,
            },
        );
        let list = s.thread_list().unwrap();
        assert_eq!(list.projects.len(), 1);
        assert_eq!(list.hidden_projects[0].name, "Team");
        assert!(Arc::ptr_eq(
            s.lists.threads.as_ref().unwrap(),
            native.as_ref().unwrap()
        ));
        let restored =
            crate::persistence::decode(&crate::persistence::encode(&s).unwrap()).unwrap();
        assert_eq!(
            restored.lists.project_preferences,
            s.lists.project_preferences
        );
        let s = dispatch(
            &s,
            Intent::SetProjectHidden {
                project_id: "b".into(),
                hidden: false,
            },
        );
        assert_eq!(s.thread_list().unwrap().projects[0].name, "Team");
        let mut other = s.clone();
        other.connection.storage_scope = "host-two".into();
        assert_eq!(other.thread_list().unwrap().projects[1].name, "Beta");
        let invalid = dispatch(
            &s,
            Intent::EditProject {
                project_id: "b".into(),
                name: " ".into(),
                section: String::new(),
            },
        );
        assert!(invalid.panes.primary().error.is_some());
        assert_eq!(
            invalid.lists.project_preferences,
            s.lists.project_preferences
        );
        let unknown = dispatch(
            &s,
            Intent::SetProjectPinned {
                project_id: "unknown".into(),
                pinned: true,
            },
        );
        assert_eq!(
            unknown.lists.project_preferences,
            s.lists.project_preferences
        );
    }
    #[test]
    fn archive_covers_later_pages_keeps_running_tasks_and_can_restore_every_chat() {
        let mut s = fixture();
        let mut page = s.lists.threads.as_ref().unwrap().as_ref().clone();
        page.data = vec![
            task("old", Some(50.), "idle"),
            task("no-date", None, "idle"),
            task("running", Some(30.), "running"),
        ];
        Arc::make_mut(&mut s.lists.project_threads).insert("a".into(), Arc::new(page));
        Arc::make_mut(&mut s.lists.expanded_projects).insert("a".into(), 5);
        let native = s.lists.project_threads.clone();
        let s = dispatch(
            &s,
            Intent::ArchiveProjectChats {
                project_id: "a".into(),
                before: 100,
            },
        );
        let rows = s.thread_list().unwrap().projects.remove(0).threads;
        assert_eq!(
            rows.iter().map(|t| t.id.id.as_str()).collect::<Vec<_>>(),
            ["running"]
        );
        assert!(Arc::ptr_eq(&s.lists.project_threads, &native));
        let mut s = s;
        let page = Arc::make_mut(
            Arc::make_mut(&mut s.lists.project_threads)
                .get_mut("a")
                .unwrap(),
        );
        page.data.extend([
            task("later-page", Some(70.), "idle"),
            task("new", Some(101.), "idle"),
            task("unknown-new", None, "idle"),
        ]);
        let rows = s.thread_list().unwrap().projects.remove(0).threads;
        assert_eq!(
            rows.iter().map(|t| t.id.id.as_str()).collect::<Vec<_>>(),
            ["running", "new", "unknown-new"]
        );
        let s = dispatch(
            &s,
            Intent::RestoreProjectChats {
                project_id: "a".into(),
            },
        );
        assert_eq!(s.thread_list().unwrap().projects[0].threads.len(), 6);
        assert!(!s.thread_list().unwrap().projects[0].chats_archived);
    }
    proptest::proptest! {
        #[test]
        fn visibility_round_trips_without_mutating_host_projects(steps in proptest::collection::vec((proptest::bool::ANY, proptest::bool::ANY),0..30)) {
            let mut s=fixture(); let native=s.lists.threads.clone();
            for (pinned,hidden) in steps {
                s=dispatch(&s,Intent::SetProjectPinned {project_id:"a".into(),pinned});
                s=dispatch(&s,Intent::SetProjectHidden {project_id:"a".into(),hidden});
                let list=s.thread_list().unwrap();
                proptest::prop_assert_eq!(list.projects.iter().any(|p| p.id=="a"), !hidden);
                proptest::prop_assert_eq!(list.hidden_projects.iter().any(|p| p.id=="a"), hidden);
                proptest::prop_assert!(Arc::ptr_eq(s.lists.threads.as_ref().unwrap(),native.as_ref().unwrap()));
                let decoded=crate::persistence::decode(&crate::persistence::encode(&s).unwrap()).unwrap();
                proptest::prop_assert_eq!(&decoded.lists.project_preferences,&s.lists.project_preferences);
            }
        }
    }
}

#[cfg(test)]
mod placement_tests {
    use super::*;
    use crate::session::{ProviderKind, SessionRef};

    fn id(provider: ProviderKind, index: usize) -> SessionRef {
        SessionRef::new(provider, format!("chat-{index}")).unwrap()
    }

    #[test]
    fn placement_uses_saved_project_order_and_full_provider_identity_at_every_size() {
        for size in [0, 16, 127, 128, 129, 512, 1000] {
            let codex = id(ProviderKind::Codex, 0);
            let claude = id(ProviderKind::Claude, 0);
            let mut preferences = BTreeMap::new();
            preferences.insert(
                "z-visible".into(),
                ProjectPreferences {
                    placed_chats: vec![PlacedChat {
                        id: codex.clone(),
                        title: "Last".into(),
                    }],
                    ..Default::default()
                },
            );
            preferences.insert(
                "b-dangling".into(),
                ProjectPreferences {
                    placed_chats: vec![PlacedChat {
                        id: claude.clone(),
                        title: "Other provider".into(),
                    }],
                    ..Default::default()
                },
            );
            preferences.insert(
                "a-hidden".into(),
                ProjectPreferences {
                    hidden: true,
                    placed_chats: std::iter::repeat_n(
                        PlacedChat {
                            id: codex.clone(),
                            title: "First".into(),
                        },
                        size,
                    )
                    .collect(),
                    ..Default::default()
                },
            );
            let lookup = ChatPlacements::new(Some(&preferences));
            assert_eq!(
                lookup.get(&codex),
                Some(if size == 0 { "z-visible" } else { "a-hidden" })
            );
            assert_eq!(lookup.get(&claude), Some("b-dangling"));
            assert_eq!(lookup.get(&id(ProviderKind::Codex, 1)), None);
        }
        let empty = [("empty".into(), ProjectPreferences::default())].into();
        for preferences in [None, Some(&empty)] {
            assert_eq!(
                ChatPlacements::new(preferences).get(&id(ProviderKind::Codex, 0)),
                None
            );
        }
    }

    proptest::proptest! {
        #[test]
        fn every_shortcut_keeps_its_first_saved_project(
            entries in proptest::collection::vec((0usize..12, 0usize..64, proptest::bool::ANY), 0..400)
        ) {
            let mut preferences: BTreeMap<String, ProjectPreferences> = BTreeMap::new();
            let mut expected = BTreeMap::<SessionRef, usize>::new();
            for (project, chat, claude) in entries {
                let id = id(if claude { ProviderKind::Claude } else { ProviderKind::Codex }, chat);
                expected.entry(id.clone()).and_modify(|first| *first = (*first).min(project)).or_insert(project);
                preferences.entry(format!("project-{project:02}")).or_default().placed_chats.push(PlacedChat { id, title: String::new() });
            }
            let lookup = ChatPlacements::new(Some(&preferences));
            for (id, project) in &expected {
                let project = format!("project-{project:02}");
                proptest::prop_assert_eq!(lookup.get(id), Some(project.as_str()));
            }
            proptest::prop_assert_eq!(lookup.get(&id(ProviderKind::Codex, 64)), None);
        }
    }
}

#[cfg(test)]
mod visibility_tests {
    use super::*;
    use crate::session::{ProviderKind, SessionRef};

    #[test]
    fn archive_visibility_keeps_active_and_placed_overrides_and_date_boundaries() {
        let id = SessionRef::new(ProviderKind::Codex, "chat".into()).unwrap();
        let archived = [id.clone()].into();
        for (before, at, explicit, active, placed, expected) in [
            (None, Some(1.), false, false, false, true),
            (Some(100), Some(99.), false, false, false, false),
            (Some(100), Some(100.), false, false, false, false),
            (Some(100), Some(100.01), false, false, false, true),
            (Some(100), None, false, false, false, true),
            (Some(100), Some(f64::NAN), false, false, false, true),
            (Some(100), Some(f64::INFINITY), false, false, false, true),
            (
                Some(100),
                Some(f64::NEG_INFINITY),
                false,
                false,
                false,
                true,
            ),
            (Some(100), Some(1.), false, false, true, true),
            (Some(100), Some(1.), false, true, false, true),
            (None, None, true, false, false, false),
            (Some(100), Some(1.), true, false, true, false),
            (Some(100), Some(1.), true, true, false, true),
        ] {
            assert_eq!(
                project_chat_visible(
                    before,
                    explicit.then_some(&archived),
                    Some(&id),
                    active,
                    at,
                    placed
                ),
                expected,
                "before={before:?}, at={at:?}, explicit={explicit}, active={active}, placed={placed}"
            );
        }
        assert!(!project_chat_visible(
            Some(100),
            Some(&archived),
            None,
            false,
            Some(1.),
            false
        ));
        assert!(project_chat_visible(
            None,
            Some(&archived),
            None,
            false,
            Some(1.),
            false
        ));
    }
}

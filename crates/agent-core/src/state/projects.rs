//! Reversible, device-local project organization. Provider files and memberships are unchanged.
use super::*;
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProjectPreferences {
    pub name: Option<String>,
    pub pinned: bool,
    pub section: Option<String>,
    pub hidden: bool,
    pub archived_before: Option<i64>,
    pub archived_ids: BTreeSet<crate::session::SessionRef>,
}
impl Snapshot {
    pub(crate) fn project_preference(&self, id: &str) -> Option<&ProjectPreferences> {
        self.project_preferences.get(&self.storage_scope)?.get(id)
    }
    pub(crate) fn project_chat_visible(&self, project: &str, thread: &Thread) -> bool {
        let Some(p) = self.project_preference(project) else {
            return true;
        };
        if crate::models::task_active(
            thread
                .id
                .as_ref()
                .and_then(|id| self.activity.active.get(id).copied()),
            thread.status,
        ) {
            return true;
        }
        !thread
            .id
            .as_ref()
            .is_some_and(|id| p.archived_ids.contains(id))
            && !p.archived_before.is_some_and(|before| {
                thread
                    .updated_at
                    .is_some_and(|at| at.is_finite() && at <= before as f64)
            })
    }
}
pub(super) fn update(
    snapshot: &mut Snapshot,
    id: &str,
    change: impl FnOnce(&mut ProjectPreferences),
) {
    if !snapshot
        .threads
        .as_ref()
        .is_some_and(|list| list.projects.iter().any(|p| p.id == id))
    {
        return;
    }
    let preferences = Arc::make_mut(&mut snapshot.project_preferences)
        .entry(snapshot.storage_scope.clone())
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
        .project_threads
        .get(project)
        .into_iter()
        .flat_map(|page| &page.data)
        .chain(
            snapshot
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
                    .and_then(|id| snapshot.activity.active.get(id).copied()),
                thread.status,
            )
        })
        .filter_map(|thread| thread.id.clone())
        .collect::<BTreeSet<_>>();
    update(snapshot, project, |p| {
        p.archived_before = Some(p.archived_before.unwrap_or_default().max(before));
        p.archived_ids.extend(ids);
    });
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture() -> Snapshot {
        let list: ThreadList = serde_json::from_value(json!({
            "data":[],"projects":[{"id":"a","name":"Alpha","roots":[{"path":"C:/alpha"}]},{"id":"b","name":"Beta","roots":[]}],
            "hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5
        })).unwrap();
        Snapshot {
            storage_scope: "host-one".into(),
            threads: Some(Arc::new(list)),
            ..Default::default()
        }
    }
    fn dispatch(s: &Snapshot, intent: Intent) -> Snapshot {
        let (next, effects) = reduce(s, Event::Intent(intent));
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
    fn project_organization_is_reversible_persistent_and_host_scoped() {
        let s = fixture();
        let native = s.threads.clone();
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
            s.threads.as_ref().unwrap(),
            native.as_ref().unwrap()
        ));
        let restored =
            crate::persistence::decode(&crate::persistence::encode(&s).unwrap()).unwrap();
        assert_eq!(restored.project_preferences, s.project_preferences);
        let s = dispatch(
            &s,
            Intent::SetProjectHidden {
                project_id: "b".into(),
                hidden: false,
            },
        );
        assert_eq!(s.thread_list().unwrap().projects[0].name, "Team");
        let mut other = s.clone();
        other.storage_scope = "host-two".into();
        assert_eq!(other.thread_list().unwrap().projects[1].name, "Beta");
        let invalid = dispatch(
            &s,
            Intent::EditProject {
                project_id: "b".into(),
                name: " ".into(),
                section: String::new(),
            },
        );
        assert!(invalid.error.is_some());
        assert_eq!(invalid.project_preferences, s.project_preferences);
        let unknown = dispatch(
            &s,
            Intent::SetProjectPinned {
                project_id: "unknown".into(),
                pinned: true,
            },
        );
        assert_eq!(unknown.project_preferences, s.project_preferences);
    }
    #[test]
    fn archive_covers_later_pages_keeps_running_tasks_and_can_restore_every_chat() {
        let mut s = fixture();
        let mut page = s.threads.as_ref().unwrap().as_ref().clone();
        page.data = vec![
            task("old", Some(50.), "idle"),
            task("no-date", None, "idle"),
            task("running", Some(30.), "running"),
        ];
        Arc::make_mut(&mut s.project_threads).insert("a".into(), Arc::new(page));
        Arc::make_mut(&mut s.expanded_projects).insert("a".into(), 5);
        let native = s.project_threads.clone();
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
        assert!(Arc::ptr_eq(&s.project_threads, &native));
        let mut s = s;
        let page = Arc::make_mut(Arc::make_mut(&mut s.project_threads).get_mut("a").unwrap());
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
        fn visibility_round_trips_without_mutating_native_projects(steps in proptest::collection::vec((proptest::bool::ANY, proptest::bool::ANY),0..30)) {
            let mut s=fixture(); let native=s.threads.clone();
            for (pinned,hidden) in steps {
                s=dispatch(&s,Intent::SetProjectPinned {project_id:"a".into(),pinned});
                s=dispatch(&s,Intent::SetProjectHidden {project_id:"a".into(),hidden});
                let list=s.thread_list().unwrap();
                proptest::prop_assert_eq!(list.projects.iter().any(|p| p.id=="a"), !hidden);
                proptest::prop_assert_eq!(list.hidden_projects.iter().any(|p| p.id=="a"), hidden);
                proptest::prop_assert!(Arc::ptr_eq(s.threads.as_ref().unwrap(),native.as_ref().unwrap()));
                let decoded=crate::persistence::decode(&crate::persistence::encode(&s).unwrap()).unwrap();
                proptest::prop_assert_eq!(&decoded.project_preferences,&s.project_preferences);
            }
        }
    }
}

use super::*;
use crate::{
    models::{Project, Thread, ThreadList, WorktreeStatus},
    session::{ProviderKind, SessionRef},
};
use agent_protocol::operations::{ListDecorationScope, ListDecorations};

fn project_icon(png: &str) -> crate::models::ProjectIcon {
    crate::models::ProjectIcon {
        light_png: png.into(),
        dark_png: png.into(),
    }
}

fn row(provider: ProviderKind, id: &str, cwd: Option<&str>, branch: Option<&str>) -> Thread {
    Thread {
        id: Some(SessionRef::new(provider, id.into()).unwrap()),
        cwd: cwd.map(str::to_owned),
        git_branch: branch.map(str::to_owned),
        worktree_status: Some(WorktreeStatus::Unmerged),
        ..Default::default()
    }
}

fn list(data: Vec<Thread>, projects: Vec<Project>) -> Arc<ThreadList> {
    Arc::new(ThreadList {
        revision: 0,
        limit: 5,
        data,
        projects,
        has_more: false,
        has_more_projects: false,
        project_pages: Default::default(),
    })
}

fn read(threads: &[Thread]) -> ReadListDecorations {
    ReadListDecorations {
        scope: ListDecorationScope::Root { part: None },
        projects: Vec::new(),
        threads: threads
            .iter()
            .filter_map(|thread| {
                Some((
                    thread.id.clone()?,
                    thread.cwd.clone()?,
                    thread.git_branch.clone(),
                ))
            })
            .collect(),
    }
}

#[test]
fn decorations_match_captured_sources_across_root_and_project_pages() {
    let current = row(ProviderKind::Codex, "task", Some("/repo"), Some("main"));
    let cleared = row(ProviderKind::Claude, "clear", Some("/repo"), None);
    let mut operation = read(&[current.clone(), cleared.clone()]);
    // The original lookup uses the first captured source for a session.
    operation.threads.push((
        current.id.clone().unwrap(),
        "/elsewhere".into(),
        Some("main".into()),
    ));
    let stale = vec![
        row(
            ProviderKind::Codex,
            "task",
            Some("/elsewhere"),
            Some("main"),
        ),
        row(ProviderKind::Codex, "task", Some("/repo"), Some("next")),
        row(ProviderKind::Codex, "task", Some("/repo"), None),
        row(ProviderKind::Codex, "task", None, Some("main")),
        row(ProviderKind::Claude, "task", Some("/repo"), Some("main")),
        row(
            ProviderKind::Codex,
            "unrequested",
            Some("/repo"),
            Some("main"),
        ),
        Thread::default(),
    ];
    let project = Project {
        id: "p".into(),
        icon: Some(project_icon("old")),
        ..Default::default()
    };
    let mut state = Snapshot {
        lists: crate::state::Lists {
            threads: Some(list(vec![current.clone(), cleared.clone()], vec![project])),
            project_threads: Arc::new(
                [
                    ("changed".into(), list(vec![current.clone()], Vec::new())),
                    ("stale".into(), list(stale, Vec::new())),
                ]
                .into(),
            ),
            ..Default::default()
        },
        ..Default::default()
    };
    let previous = state.clone();
    let output = ListDecorations {
        icons: [
            ("p".into(), None),
            ("absent".into(), Some(project_icon("unused"))),
        ]
        .into(),
        statuses: vec![
            (current.id.clone().unwrap(), None),
            (current.id.clone().unwrap(), Some(WorktreeStatus::Merged)),
            (cleared.id.clone().unwrap(), None),
            (
                SessionRef::new(ProviderKind::Codex, "unrequested".into()).unwrap(),
                Some(WorktreeStatus::Merged),
            ),
        ],
    };
    assert!(operation.apply(&mut state, output).is_empty());
    let root = state.lists.threads.as_ref().unwrap();
    assert_eq!(root.data[0].worktree_status, Some(WorktreeStatus::Merged));
    assert_eq!(root.data[1].worktree_status, None);
    assert_eq!(root.projects[0].icon, None);
    assert_eq!(
        state.lists.project_threads["changed"].data[0].worktree_status,
        Some(WorktreeStatus::Merged)
    );
    assert!(Arc::ptr_eq(
        &state.lists.project_threads["stale"],
        &previous.lists.project_threads["stale"]
    ));
    assert_eq!(
        previous.lists.threads.as_ref().unwrap().data[0].worktree_status,
        Some(WorktreeStatus::Unmerged)
    );
    assert_eq!(
        previous.lists.threads.as_ref().unwrap().projects[0]
            .icon
            .as_ref()
            .map(|icon| icon.light_png.as_str()),
        Some("old")
    );
}

#[test]
fn unchanged_decorations_preserve_shared_lists() {
    let current = row(ProviderKind::Codex, "task", Some("/repo"), None);
    let project = Project {
        id: "p".into(),
        icon: Some(project_icon("icon")),
        ..Default::default()
    };
    let original = Snapshot {
        lists: crate::state::Lists {
            threads: Some(list(vec![current.clone()], vec![project])),
            project_threads: Arc::new(
                [("p".into(), list(vec![current.clone()], Vec::new()))].into(),
            ),
            ..Default::default()
        },
        ..Default::default()
    };
    for statuses in [
        Vec::new(),
        vec![(current.id.clone().unwrap(), current.worktree_status)],
    ] {
        let mut state = original.clone();
        read(std::slice::from_ref(&current)).apply(
            &mut state,
            ListDecorations {
                icons: [("p".into(), Some(project_icon("icon")))].into(),
                statuses,
            },
        );
        assert!(Arc::ptr_eq(
            state.lists.threads.as_ref().unwrap(),
            original.lists.threads.as_ref().unwrap()
        ));
        assert!(Arc::ptr_eq(
            &state.lists.project_threads,
            &original.lists.project_threads
        ));
    }
}

#[test]
fn icons_apply_without_root_threads_or_status_changes() {
    let mut state = Snapshot {
        lists: crate::state::Lists {
            threads: Some(list(
                Vec::new(),
                vec![Project {
                    id: "p".into(),
                    ..Default::default()
                }],
            )),
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(
        read(&[])
            .apply(
                &mut state,
                ListDecorations {
                    icons: [("p".into(), Some(project_icon("icon")))].into(),
                    statuses: Vec::new(),
                },
            )
            .is_empty()
    );
    assert_eq!(
        state.lists.threads.as_ref().unwrap().projects[0]
            .icon
            .as_ref()
            .map(|icon| icon.light_png.as_str()),
        Some("icon")
    );
    state.lists.threads = None;
    assert!(
        read(&[])
            .apply(
                &mut state,
                ListDecorations {
                    icons: Default::default(),
                    statuses: Vec::new()
                },
            )
            .is_empty()
    );
}

#[test]
fn project_decorations_do_not_require_a_root_list() {
    let current = row(ProviderKind::Codex, "task", Some("/repo"), None);
    let original = Snapshot {
        lists: crate::state::Lists {
            project_threads: Arc::new(
                [("p".into(), list(vec![current.clone()], Vec::new()))].into(),
            ),
            ..Default::default()
        },
        ..Default::default()
    };
    let output = ListDecorations {
        icons: [("p".into(), Some(project_icon("unused")))].into(),
        statuses: vec![(current.id.clone().unwrap(), Some(WorktreeStatus::Merged))],
    };
    let mut state = original.clone();
    read(&[]).apply(&mut state, output.clone());
    assert!(Arc::ptr_eq(
        &state.lists.project_threads,
        &original.lists.project_threads
    ));
    read(&[current]).apply(&mut state, output);
    assert!(state.lists.threads.is_none());
    assert_eq!(
        state.lists.project_threads["p"].data[0].worktree_status,
        Some(WorktreeStatus::Merged)
    );
    assert_eq!(
        original.lists.project_threads["p"].data[0].worktree_status,
        Some(WorktreeStatus::Unmerged)
    );
}

#[test]
fn a_dark_only_logo_update_does_not_require_the_light_logo_to_change() {
    let original = project_icon("light");
    let mut state = Snapshot {
        lists: crate::state::Lists {
            threads: Some(list(
                Vec::new(),
                vec![Project {
                    id: "p".into(),
                    icon: Some(original.clone()),
                    ..Default::default()
                }],
            )),
            ..Default::default()
        },
        ..Default::default()
    };
    let previous = state.clone();
    let changed = crate::models::ProjectIcon {
        dark_png: "dark".into(),
        ..original
    };
    read(&[]).apply(
        &mut state,
        ListDecorations {
            icons: [("p".into(), Some(changed.clone()))].into(),
            statuses: Vec::new(),
        },
    );
    assert_eq!(
        state.lists.threads.as_ref().unwrap().projects[0]
            .icon
            .as_ref(),
        Some(&changed)
    );
    assert_eq!(
        previous.lists.threads.as_ref().unwrap().projects[0]
            .icon
            .as_ref()
            .unwrap()
            .dark_png,
        "light"
    );
}

use super::*;
use crate::{
    models::{ListPage, ListQuery, Project, ProjectMembership, Thread, ThreadList},
    session::{ProviderKind, SessionRef},
};
use proptest::prelude::*;

fn row(name: &str, project: ProjectMembership) -> Thread {
    Thread {
        id: Some(SessionRef::new(ProviderKind::Codex, name.into()).unwrap()),
        name: Some(name.into()),
        project_id: project,
        ..Default::default()
    }
}

fn list(data: Vec<Thread>, limit: u32) -> ThreadList {
    ThreadList {
        revision: 1,
        limit,
        data,
        projects: Vec::new(),
        has_more: false,
        has_more_projects: false,
        project_pages: Default::default(),
    }
}

#[test]
fn project_grouping_moves_visible_rows_and_keeps_hidden_rows_in_both_lists() {
    let visible = row("visible", ProjectMembership::Assigned("p".into()));
    let hidden = row("hidden", ProjectMembership::Assigned("hidden".into()));
    let unknown = row("unknown", ProjectMembership::Unknown {});
    let unassigned = row("unassigned", ProjectMembership::Unassigned {});
    let empty = row("empty", ProjectMembership::Assigned(String::new()));
    let visible_name = visible.name.as_ref().unwrap().as_ptr();
    let hidden_name = hidden.name.as_ref().unwrap().as_ptr();
    let expected_visible = visible.clone();
    let expected_hidden = hidden.clone();
    let expected_remaining = vec![
        hidden.clone(),
        unknown.clone(),
        empty.clone(),
        unassigned.clone(),
    ];
    let (remaining, grouped) = group_project_rows(
        vec![visible, hidden, unknown, empty.clone(), unassigned],
        ["p", "hidden", "", "no-rows"].into_iter(),
        &["p"].into(),
    );
    assert_eq!(remaining, expected_remaining);
    assert_eq!(grouped["p"], [expected_visible]);
    assert_eq!(grouped["hidden"], [expected_hidden]);
    assert_eq!(grouped[""], [empty]);
    assert!(grouped["no-rows"].is_empty());
    assert_eq!(
        grouped["p"][0].name.as_ref().unwrap().as_ptr(),
        visible_name
    );
    assert_eq!(remaining[0].name.as_ref().unwrap().as_ptr(), hidden_name);
    assert_ne!(
        grouped["hidden"][0].name.as_ref().unwrap().as_ptr(),
        hidden_name
    );
    let allocation = remaining.as_ptr();
    let (remaining, grouped) = group_project_rows(remaining, std::iter::empty(), &HashSet::new());
    assert_eq!(remaining.as_ptr(), allocation);
    assert!(grouped.is_empty());
}

#[test]
fn empty_project_pages_keep_unassigned_root_rows_without_grouping() {
    let rows = vec![
        row("unknown", ProjectMembership::Unknown {}),
        row("unassigned", ProjectMembership::Unassigned {}),
    ];
    let expected = rows.clone();
    let allocation = rows.as_ptr();
    let (rows, grouped) = group_project_rows(rows, ["empty"].into_iter(), &["empty"].into());
    assert_eq!(rows.as_ptr(), allocation);
    assert!(grouped.is_empty());
    let mut input = list(rows, 5);
    input.projects.push(Project {
        id: "empty".into(),
        ..Default::default()
    });
    input.project_pages.insert(
        "empty".into(),
        ListPage {
            limit: 5,
            has_more: true,
        },
    );
    let mut snapshot = Snapshot::default();
    ListSessions::new(Default::default()).apply(&mut snapshot, input);
    assert_eq!(snapshot.lists.threads.as_ref().unwrap().data, expected);
    assert!(snapshot.lists.project_threads["empty"].data.is_empty());
    assert!(snapshot.lists.project_threads["empty"].has_more);
}

#[test]
fn root_refresh_groups_pages_without_replacing_larger_or_omitted_cached_pages() {
    let assigned = |id: &str| ProjectMembership::Assigned(id.into());
    let root = row("root", ProjectMembership::Unassigned {});
    let hidden = row("hidden", assigned("hidden"));
    let other = row("other", assigned("other"));
    let first = row("first", assigned("loaded"));
    let second = row("second", assigned("loaded"));
    let first_name = first.name.as_ref().unwrap().as_ptr();
    let mut input = list(
        vec![
            first,
            row("ignored-smaller", assigned("larger")),
            root.clone(),
            hidden.clone(),
            second,
            row("lookahead", assigned("loaded")),
            row("omitted-input", assigned("omitted")),
            other.clone(),
        ],
        10,
    );
    input.projects = ["loaded", "larger", "omitted", "empty"]
        .map(|id| Project {
            id: id.into(),
            ..Default::default()
        })
        .into();
    input.project_pages = [
        ("loaded", 2, false),
        ("larger", 5, false),
        ("empty", 5, true),
        ("hidden", 5, false),
    ]
    .map(|(id, limit, has_more)| (id.into(), ListPage { limit, has_more }))
    .into();
    let larger = Arc::new(list(vec![row("cached-larger", assigned("larger"))], 15));
    let omitted = Arc::new(list(vec![row("cached-omitted", assigned("omitted"))], 5));
    let mut snapshot = Snapshot {
        lists: crate::state::Lists {
            list_query: Arc::new(ListQuery {
                limit: 10,
                ..Default::default()
            }),
            project_threads: Arc::new(
                [
                    ("larger".into(), larger.clone()),
                    ("omitted".into(), omitted.clone()),
                ]
                .into(),
            ),
            unconfirmed_lists: Arc::new(
                [
                    OperationKey::ProjectList {
                        project_id: "larger".into(),
                    },
                    OperationKey::ProjectList {
                        project_id: "loaded".into(),
                    },
                ]
                .into(),
            ),
            ..Default::default()
        },
        ..Default::default()
    };
    ListSessions::new((*snapshot.lists.list_query).clone()).apply(&mut snapshot, input);
    assert_eq!(
        snapshot.lists.threads.as_ref().unwrap().data,
        [root, hidden.clone(), other]
    );
    let loaded = &snapshot.lists.project_threads["loaded"];
    assert_eq!(
        loaded
            .data
            .iter()
            .map(|row| row.name.as_deref())
            .collect::<Vec<_>>(),
        [Some("first"), Some("second")]
    );
    assert!(loaded.has_more);
    assert_eq!(loaded.data[0].name.as_ref().unwrap().as_ptr(), first_name);
    assert!(Arc::ptr_eq(
        &larger,
        &snapshot.lists.project_threads["larger"]
    ));
    assert!(Arc::ptr_eq(
        &omitted,
        &snapshot.lists.project_threads["omitted"]
    ));
    assert_eq!(snapshot.lists.project_threads["hidden"].data, [hidden]);
    assert!(snapshot.lists.project_threads["empty"].data.is_empty());
    assert!(snapshot.lists.project_threads["empty"].has_more);
    assert_eq!(
        *snapshot.lists.unconfirmed_lists,
        [
            OperationKey::ProjectList {
                project_id: "larger".into()
            },
            OperationKey::ProjectList {
                project_id: "omitted".into()
            },
        ]
        .into()
    );
}

proptest! {
    #[test]
    fn grouping_preserves_input_order_and_every_unselected_membership(
        roles in proptest::collection::vec(0u8..5, 0..512),
    ) {
        let rows: Vec<_> = roles.iter().enumerate().map(|(index, role)| row(
            &format!("row{index}"),
            match role {
                0 => ProjectMembership::Assigned("visible".into()),
                1 => ProjectMembership::Assigned("hidden".into()),
                2 => ProjectMembership::Assigned("unselected".into()),
                3 => ProjectMembership::Unknown {},
                _ => ProjectMembership::Unassigned {},
            },
        )).collect();
        let expected = |role| roles.iter().zip(&rows).filter(|(actual, _)| **actual == role).map(|(_, row)| row.clone()).collect::<Vec<_>>();
        let visible = expected(0);
        let hidden = expected(1);
        let remaining: Vec<_> = roles.iter().zip(&rows).filter(|(role, _)| **role != 0).map(|(_, row)| row.clone()).collect();
        let (actual_remaining, grouped) = group_project_rows(rows, ["visible", "hidden"].into_iter(), &["visible"].into());
        prop_assert_eq!(actual_remaining, remaining);
        prop_assert_eq!(grouped.get("visible").map(Vec::as_slice).unwrap_or_default(), visible.as_slice());
        prop_assert_eq!(grouped.get("hidden").map(Vec::as_slice).unwrap_or_default(), hidden.as_slice());
    }
}

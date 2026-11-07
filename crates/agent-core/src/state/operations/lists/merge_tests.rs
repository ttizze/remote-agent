use super::*;
use crate::{
    models::{Project, ProjectMembership, ProjectRoot, Thread, ThreadList, WorktreeStatus},
    session::{ProviderKind, SessionRef},
};
use proptest::prelude::*;

fn project_icon(png: &str) -> crate::models::ProjectIcon {
    crate::models::ProjectIcon {
        light_png: png.into(),
        dark_png: png.into(),
    }
}

fn session(provider: ProviderKind, id: &str) -> SessionRef {
    SessionRef::new(provider, id.into()).unwrap()
}

fn row(
    id: Option<SessionRef>,
    cwd: Option<&str>,
    branch: Option<&str>,
    status: Option<WorktreeStatus>,
) -> Thread {
    Thread {
        id,
        cwd: cwd.map(str::to_owned),
        git_branch: branch.map(str::to_owned),
        worktree_status: status,
        ..Default::default()
    }
}

fn list(data: Vec<Thread>) -> ThreadList {
    ThreadList {
        revision: 1,
        limit: data.len() as u32,
        data,
        projects: Vec::new(),
        has_more: false,
        has_more_projects: false,
        project_pages: Default::default(),
    }
}

fn versioned(id: Option<SessionRef>, name: &str, revision: u64) -> Thread {
    Thread {
        id,
        name: Some(name.into()),
        preview: Some(format!("preview-{name}")),
        cwd: Some(format!("/repo/{name}")),
        status: crate::models::SessionStatus::Idle,
        list_revision: revision,
        ..Default::default()
    }
}

fn project(id: &str, name: &str) -> Project {
    Project {
        id: id.into(),
        name: name.into(),
        roots: vec![ProjectRoot {
            path: format!("/repo/{name}"),
        }],
        icon: Some(project_icon(&format!("icon-{name}"))),
        ..Default::default()
    }
}

fn linked(id: Option<SessionRef>, project: &str) -> Thread {
    Thread {
        project_id: ProjectMembership::Assigned(project.into()),
        ..versioned(id, "linked", 0)
    }
}

#[test]
fn newer_project_notices_precede_received_rows_with_every_duplicate_in_cached_order() {
    let notice = |id: &str, name: &str, revision| Project {
        list_revision: revision,
        ..project(id, name)
    };
    for count in [16, 127, 128, 129, 512] {
        let mut cached = vec![
            notice("new", "first-new", 3),
            notice("existing", "replace", 2),
            notice("new", "second-new", 2),
            notice("equal", "ignore-equal", 1),
            notice("old", "ignore-old", 0),
            notice("existing", "duplicate-replace", 4),
            notice("", "empty-id", 2),
        ];
        let retained = vec![
            notice("equal", "received-equal", 0),
            notice("old", "received-old", 0),
            notice("other", "first-other", 0),
            notice("other", "second-other", 0),
        ];
        let mut received = retained.clone();
        received.extend([
            notice("existing", "first-existing", 0),
            notice("existing", "second-existing", 0),
            notice("new", "received-new", 0),
            notice("", "received-empty", 0),
        ]);
        let mut expected = vec![
            cached[0].clone(),
            cached[1].clone(),
            cached[2].clone(),
            cached[5].clone(),
            cached[6].clone(),
        ];
        for index in expected.len()..count {
            let id = format!("padding{index}");
            let updated = notice(&id, "updated", 2);
            expected.push(updated.clone());
            cached.push(updated);
            received.push(notice(&id, "received", 0));
        }
        expected.extend(retained);
        let mut input = list(Vec::new());
        input.projects = received;
        let mut previous = list(Vec::new());
        previous.projects = cached;
        assert_eq!(merge_received(input, Some(&previous)).projects, expected);
    }
}

proptest! {
    #[test]
    fn project_notices_replace_only_ids_changed_after_the_read(
        fresh in proptest::collection::vec(any::<bool>(), 0..512),
    ) {
        let mut cached = Vec::new();
        let mut received = Vec::new();
        let mut expected = Vec::new();
        let mut unchanged = Vec::new();
        for (index, fresh) in fresh.into_iter().enumerate() {
            let id = format!("project{index}");
            let notice = Project {
                list_revision: if fresh { 2 } else { 1 },
                ..project(&id, "notice")
            };
            let read = project(&id, "received");
            if fresh { expected.push(notice.clone()); } else { unchanged.push(read.clone()); }
            cached.push(notice);
            received.push(read);
        }
        received.reverse();
        expected.extend(unchanged.into_iter().rev());
        let mut input = list(Vec::new());
        input.projects = received;
        let mut previous = list(Vec::new());
        previous.projects = cached;
        prop_assert_eq!(merge_received(input, Some(&previous)).projects, expected);
    }
}

#[test]
fn project_icons_match_ordered_roots_keep_first_values_and_reuse_unchanged_images() {
    let entry = |id: &str, name: &str, roots: &[&str], icon: Option<&str>| Project {
        id: id.into(),
        name: name.into(),
        roots: roots
            .iter()
            .map(|path| ProjectRoot {
                path: (*path).into(),
            })
            .collect(),
        icon: icon.map(project_icon),
        ..Default::default()
    };
    for count in [16, 127, 128, 129, 512] {
        let mut cached = vec![
            entry("same", "first", &["/a", "/b"], Some("first")),
            entry("same", "duplicate", &["/a", "/b"], Some("later")),
            entry("same", "negative", &["/a"], None),
            entry("empty", "old-empty", &[], Some("empty")),
        ];
        let mut received = vec![
            entry("same", "current", &["/a", "/b"], Some("current")),
            entry("same", "current-negative", &["/a"], Some("current")),
            entry("same", "ordered", &["/b", "/a"], Some("keep-order")),
            entry("other", "different", &["/a", "/b"], Some("keep-id")),
            entry("empty", "current-empty", &[], None),
            entry("same", "unchanged", &["/a", "/b"], Some("first")),
        ];
        let mut expected = received.clone();
        expected[0].icon = Some(project_icon("first"));
        expected[1].icon = None;
        expected[4].icon = Some(project_icon("empty"));
        while cached.len() < count {
            cached.push(entry(
                &format!("old{}", cached.len()),
                "old",
                &[],
                Some("old"),
            ));
        }
        while received.len() < count {
            let row = entry(
                &format!("new{}", received.len()),
                "new",
                &[],
                Some("uncached"),
            );
            expected.push(row.clone());
            received.push(row);
        }
        let original_image = received[5].icon.as_ref().unwrap().light_png.as_ptr();
        let mut input = list(Vec::new());
        input.projects = received;
        let mut previous = list(Vec::new());
        previous.projects = cached;
        let result = merge_received(input, Some(&previous));
        assert_eq!(result.projects, expected);
        assert_eq!(
            result.projects[5].icon.as_ref().unwrap().light_png.as_ptr(),
            original_image
        );
    }
}

#[test]
fn received_rows_restore_only_referenced_projects_in_first_cached_order() {
    for count in [16, 127, 128, 129, 512] {
        let current = vec![
            project("present", "current"),
            project("present", "current-duplicate"),
        ];
        let first = project("missing", "first");
        let anonymous = project("anonymous", "anonymous");
        let mut cached = vec![
            project("present", "stale"),
            first.clone(),
            project("unused", "unused"),
            project("missing", "duplicate"),
            anonymous.clone(),
        ];
        let mut rows = vec![
            linked(Some(session(ProviderKind::Claude, "present")), "present"),
            linked(Some(session(ProviderKind::Claude, "missing")), "missing"),
            linked(None, "anonymous"),
            Thread {
                cwd: Some("/repo/unused".into()),
                project_id: ProjectMembership::Unassigned {},
                ..Default::default()
            },
        ];
        let mut expected = current.clone();
        expected.extend([first, anonymous]);
        while cached.len() < count {
            let id = format!("project{}", cached.len());
            let row_id = format!("thread{}", cached.len());
            rows.push(linked(Some(session(ProviderKind::Claude, &row_id)), &id));
            let project = project(&id, "padding");
            expected.push(project.clone());
            cached.push(project);
        }
        let mut input = list(rows);
        input.projects = current;

        let mut previous = list(Vec::new());
        previous.projects = cached;
        assert_eq!(merge_received(input, Some(&previous)).projects, expected);
    }
}

proptest! {
    #[test]
    fn project_recovery_keeps_current_catalogue_and_uses_membership_from_every_provider(
        cases in proptest::collection::vec((any::<bool>(), any::<bool>()), 0..512),
    ) {
        let mut cached = Vec::new();
        let mut received = Vec::new();
        let mut restored = Vec::new();
        let mut rows = Vec::new();
        for (index, &(used, returned)) in cases.iter().enumerate() {
            let id = format!("project{index}");
            let old = project(&id, "cached");
            if returned { received.push(project(&id, "received")); }
            if used {
                let provider = if index % 2 == 0 { ProviderKind::Codex } else { ProviderKind::Claude };
                rows.push(linked(Some(session(provider, &format!("thread{index}"))), &id));
                if !returned { restored.push(old.clone()); }
            }
            cached.push(old);
        }
        received.reverse();
        let mut expected = received.clone();
        expected.extend(restored);
        let mut input = list(rows);
        input.projects = received;

        let mut previous = list(Vec::new());
        previous.projects = cached;
        let result = merge_received(input, Some(&previous));
        prop_assert_eq!(result.projects, expected);
    }
}

#[test]
fn newer_notices_keep_last_values_first_positions_and_duplicate_received_rows() {
    for count in [16, 127, 128, 129, 512] {
        let existing = Some(session(ProviderKind::Codex, "existing"));
        let a = Some(session(ProviderKind::Codex, "a"));
        let b = Some(session(ProviderKind::Codex, "b"));
        let last = versioned(existing.clone(), "last", 2);
        let last_anonymous = versioned(None, "anonymous-last", 2);
        let appended_a = versioned(a.clone(), "a-last", 2);
        let appended_b = versioned(b, "b", 2);
        let duplicate = versioned(existing.clone(), "duplicate", 0);
        let anonymous_duplicate = versioned(None, "anonymous-duplicate", 0);
        let mut received = vec![
            versioned(existing.clone(), "received", 0),
            duplicate.clone(),
            versioned(None, "anonymous", 0),
            anonymous_duplicate.clone(),
        ];
        let mut expected = vec![
            last.clone(),
            duplicate,
            last_anonymous.clone(),
            anonymous_duplicate,
        ];
        let mut previous = vec![
            versioned(a, "a-first", 2),
            versioned(existing, "first", 3),
            appended_b.clone(),
            appended_a.clone(),
            last,
            versioned(None, "anonymous-first", 4),
            last_anonymous,
        ];
        while previous.len() < count {
            let id = Some(session(
                ProviderKind::Codex,
                &format!("padding{}", previous.len()),
            ));
            let cached = versioned(id.clone(), "cached-padding", 2);
            received.push(versioned(id, "received-padding", 0));
            expected.push(cached.clone());
            previous.push(cached);
        }
        previous.push(versioned(
            Some(session(ProviderKind::Codex, "ignored")),
            "old",
            1,
        ));
        expected.extend([appended_a, appended_b]);
        assert_eq!(
            merge_received(list(received), Some(&list(previous))).data,
            expected
        );
    }
}

proptest! {
    #[test]
    fn only_notices_newer_than_the_read_replace_each_provider_scoped_row(
        fresh in proptest::collection::vec(any::<bool>(), 0..512),
    ) {
        let mut previous = Vec::new();
        let mut received = Vec::new();
        let mut expected = Vec::new();
        for (index, fresh) in fresh.into_iter().enumerate() {
            let provider = if index % 2 == 0 { ProviderKind::Codex } else { ProviderKind::Claude };
            let id = Some(session(provider, &format!("task{}", index / 2)));
            let cached = versioned(id.clone(), "cached", if fresh { 2 } else { 1 });
            let read = versioned(id, "received", 0);
            expected.push(if fresh { cached.clone() } else { read.clone() });
            previous.push(cached);
            received.push(read);
        }
        received.reverse();
        expected.reverse();
        let merged = merge_received(list(received), Some(&list(previous)));
        prop_assert_eq!(merged.data, expected);
    }
}

#[test]
fn received_lists_keep_the_first_exact_source_including_absent_fields() {
    for count in [16, 63, 64, 65, 127, 128, 129, 512] {
        let codex = Some(session(ProviderKind::Codex, "same"));
        let claude = Some(session(ProviderKind::Claude, "same"));
        let merged = Some(WorktreeStatus::Merged);
        let unmerged = Some(WorktreeStatus::Unmerged);
        let mut previous = vec![
            row(codex.clone(), Some("/repo"), Some("main"), merged),
            row(codex.clone(), Some("/repo"), Some("main"), None),
            row(codex.clone(), Some("/other"), Some("main"), None),
            row(codex.clone(), Some("/repo"), None, None),
            row(claude.clone(), Some("/repo"), Some("main"), None),
            row(None, None, None, merged),
            row(None, None, None, None),
        ];
        let mut received = vec![
            row(codex.clone(), Some("/repo"), Some("main"), unmerged),
            row(codex.clone(), Some("/other"), Some("main"), unmerged),
            row(codex.clone(), Some("/repo"), Some("changed"), unmerged),
            row(codex.clone(), Some("/repo"), None, unmerged),
            row(codex, None, Some("main"), unmerged),
            row(claude, Some("/repo"), Some("main"), unmerged),
            row(None, None, None, unmerged),
        ];
        while previous.len() < count {
            previous.push(row(
                Some(session(
                    ProviderKind::Codex,
                    &format!("old{}", previous.len()),
                )),
                None,
                None,
                None,
            ));
        }
        while received.len() < count {
            received.push(row(
                Some(session(
                    ProviderKind::Codex,
                    &format!("new{}", received.len()),
                )),
                None,
                None,
                unmerged,
            ));
        }
        let expected_ids: Vec<_> = received.iter().map(|thread| thread.id.clone()).collect();
        let result = merge_received(list(received), Some(&list(previous)));
        assert_eq!(
            result
                .data
                .iter()
                .map(|thread| thread.id.clone())
                .collect::<Vec<_>>(),
            expected_ids
        );
        assert_eq!(
            result.data[..7]
                .iter()
                .map(|thread| thread.worktree_status)
                .collect::<Vec<_>>(),
            [merged, None, unmerged, None, unmerged, None, merged]
        );
        assert!(
            result.data[7..]
                .iter()
                .all(|thread| thread.worktree_status == unmerged)
        );
    }
}

proptest! {
    #[test]
    fn refreshed_metadata_never_crosses_a_changed_source(
        cases in proptest::collection::vec((0u8..4, 0u8..3), 0..256),
    ) {
        let mut previous = Vec::new();
        let mut received = Vec::new();
        let mut expected = Vec::new();
        for (index, &(change, status)) in cases.iter().enumerate() {
            let id = format!("task{index}");
            let old = match status {
                0 => None,
                1 => Some(WorktreeStatus::Merged),
                _ => Some(WorktreeStatus::Unmerged),
            };
            previous.push(row(Some(session(ProviderKind::Codex, &id)), Some("/repo"), Some("main"), old));
            let provider = if change == 3 { ProviderKind::Claude } else { ProviderKind::Codex };
            received.push(row(
                Some(session(provider, &id)),
                Some(if change == 1 { "/other" } else { "/repo" }),
                Some(if change == 2 { "other" } else { "main" }),
                Some(WorktreeStatus::Unmerged),
            ));
            expected.push(if change == 0 { old } else { Some(WorktreeStatus::Unmerged) });
        }
        received.reverse();
        expected.reverse();
        let result = merge_received(list(received), Some(&list(previous)));
        prop_assert_eq!(result.data.iter().map(|thread| thread.worktree_status).collect::<Vec<_>>(), expected);
    }
}

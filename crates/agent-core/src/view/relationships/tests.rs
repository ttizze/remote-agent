use super::*;
use crate::models::{Model, ModelRef, Project, ProjectRoot};
use crate::provider::ProviderKind;
use crate::sync::fixtures::thread_state;
use crate::sync::{ShellCache, ThreadSync, fixtures::run};
use agent_domain::{
    CompletionWake, ContextDelivery, ContextTransferId, DeliveryState, HistoricalContext, NodeId,
    RunAttemptId, RunId, Timestamp, TransferKind, Workspace,
};
use agent_protocol::conversation::ShellSnapshot;
use rstest::rstest;
use std::sync::Arc;

fn id(value: &str) -> ThreadId {
    ThreadId::new(value).unwrap()
}

fn at(iso: &str) -> Timestamp {
    Timestamp::parse(iso).unwrap()
}

fn shell(thread: &str) -> ThreadShell {
    let mut row = agent_domain::shell(&thread_state(thread)).unwrap();
    row.id = id(thread);
    row
}

fn child_of(thread: &str, parent: &str, fork: bool) -> ThreadShell {
    let mut row = shell(thread);
    row.parent = Some(id(parent));
    row.fork_boundary = fork.then_some(1);
    row
}

fn with_shell(mut row: ThreadShell, change: impl FnOnce(&mut ThreadShell)) -> ThreadShell {
    change(&mut row);
    row
}

fn projection(thread: &str) -> State {
    let mut state = thread_state(thread);
    state.thread.as_mut().unwrap().id = id(thread);
    state
}

fn task(id_: &str, child: &str, status: ItemStatus) -> Task {
    Task {
        original_message: None,
        native_task: None,
        background: false,
        id: NodeId::new(id_).unwrap(),
        native_key: id_.into(),
        run: None,
        attempt: RunAttemptId::new("attempt").unwrap(),
        child_thread: id(child),
        parent_task: None,
        prompt: "Check the change".into(),
        title: None,
        started_at: at("2026-09-16T12:00:00Z"),
        completed_at: None,
        model: Some("gpt-5.4".into()),
        status,
        result: None,
        progress: None,
        wake: CompletionWake::Always,
        delivery: DeliveryState::Pending,
        generation: 0,
    }
}

fn transfer(source: &str, target: &str) -> Transfer {
    Transfer {
        native_source: None,
        instance: None,
        target_run: None,
        delivery: None,
        id: ContextTransferId::new(format!("{source}-{target}")).unwrap(),
        kind: TransferKind::MergeBack,
        source: id(source),
        target: id(target),
        boundary: 1,
        history: HistoricalContext {
            messages: vec![],
            context: String::new(),
            omitted_items: 0,
            omitted_item_ids: vec![],
        },
        superseded: false,
    }
}

fn immediate_ids(graph: &RelationshipGraph, thread: &str) -> Vec<String> {
    immediate_thread_relationships(graph, &id(thread))
        .into_iter()
        .map(|row| row.thread.to_string())
        .collect()
}

fn first_row_status(graph: &RelationshipGraph, thread: &str) -> Option<RelationshipStatus> {
    relationship_row_status(
        graph,
        &immediate_thread_relationships(graph, &id(thread))[0],
    )
}

#[rstest]
#[case(RunStatus::Running, RunStatus::Completed)]
#[case(RunStatus::Completed, RunStatus::Running)]
#[case(RunStatus::Running, RunStatus::Running)]
#[case(RunStatus::Completed, RunStatus::Completed)]
#[case(RunStatus::Waiting, RunStatus::Failed)]
#[case(RunStatus::Interrupted, RunStatus::Queued)]
fn keeps_parent_status_independent_of_child_status(
    #[case] parent_status: RunStatus,
    #[case] child_status: RunStatus,
) {
    let rows = [
        with_shell(shell("parent"), |row| {
            row.activity_run_status = Some(parent_status);
            row.status = Some(RunStatus::Cancelled);
        }),
        with_shell(child_of("child", "parent", true), |row| {
            row.activity_run_status = Some(child_status);
        }),
    ];
    let graph = relationship_graph(&rows, None);

    assert_eq!(
        first_row_status(&graph, "child"),
        Some(parent_status.into())
    );
    assert_eq!(
        first_row_status(&graph, "parent"),
        Some(child_status.into())
    );
}

#[test]
fn does_not_label_a_missing_parent_with_its_childs_running_status() {
    let rows = [with_shell(
        child_of("child", "missing-parent", false),
        |row| {
            row.status = Some(RunStatus::Running);
        },
    )];
    let graph = relationship_graph(&rows, None);

    assert_eq!(first_row_status(&graph, "child"), None);
}

#[test]
fn keeps_an_older_activity_run_visible_over_a_newer_cancelled_run() {
    let rows = [with_shell(
        child_of("thread-child", "thread-parent", false),
        |row| {
            row.title = "Active child".into();
            row.activity_run_status = Some(RunStatus::Running);
            row.status = Some(RunStatus::Cancelled);
        },
    )];
    let graph = relationship_graph(&rows, None);

    assert_eq!(graph.edges.len(), 1);
    assert_eq!(graph.edges[0].target, id("thread-child"));
    assert_eq!(graph.edges[0].status, Some(RelationshipStatus::Running));
}

#[test]
fn keeps_missing_parents_and_cycles_navigable_without_recursive_traversal() {
    let rows = [
        child_of("thread-root", "thread-child", true),
        child_of("thread-child", "thread-missing", true),
    ];
    let graph = relationship_graph(&rows, None);
    let (root, child, missing) = (id("thread-root"), id("thread-child"), id("thread-missing"));

    assert!(graph.missing(&missing));
    assert_eq!(
        related_thread_ids(&graph, &root),
        std::slice::from_ref(&child)
    );
    assert_eq!(
        related_thread_ids(&graph, &child),
        [root.clone(), missing.clone()]
    );
    assert_eq!(
        walk_thread_relationships(&graph, &root)
            .into_iter()
            .map(|row| (row.thread, row.depth))
            .collect::<Vec<_>>(),
        [(child, 1), (missing, 2)]
    );
    assert_eq!(immediate_ids(&graph, "thread-root"), ["thread-child"]);
}

#[test]
fn combines_subagent_and_transfer_edges_with_archived_shell_state() {
    let rows = [
        shell("thread-parent"),
        with_shell(child_of("thread-child", "thread-parent", false), |row| {
            row.archived_at = Some(at("2026-06-24T00:00:00.000Z"));
        }),
    ];
    let mut state = projection("thread-parent");
    state.tasks = vec![task("agent", "thread-child", ItemStatus::Completed)];
    state.transfers = vec![transfer("thread-child", "thread-transfer")];
    let graph = relationship_graph(&rows, Some(&state));

    assert!(
        graph
            .thread(&id("thread-child"))
            .unwrap()
            .archived_at
            .is_some()
    );
    assert!(graph.missing(&id("thread-transfer")));
    let edges: Vec<_> = graph
        .edges
        .iter()
        .map(|edge| (edge.source.as_str(), edge.target.as_str(), edge.kind))
        .collect();
    assert!(edges.contains(&("thread-parent", "thread-child", RelationshipKind::Subagent)));
    assert!(edges.contains(&(
        "thread-child",
        "thread-transfer",
        RelationshipKind::Transfer
    )));
}

#[rstest]
#[case(Some(RunStatus::Running), RelationshipStatus::Running)]
#[case(None, RelationshipStatus::Completed)]
fn shows_a_subagent_by_its_child_activity_after_its_delegated_task_settled(
    #[case] child_activity: Option<RunStatus>,
    #[case] expected: RelationshipStatus,
) {
    let rows = [
        with_shell(shell("thread-parent"), |row| {
            row.status = Some(RunStatus::Completed)
        }),
        with_shell(child_of("thread-child", "thread-parent", false), |row| {
            row.status = Some(child_activity.unwrap_or(RunStatus::Completed));
            row.activity_run_status = child_activity;
        }),
    ];
    let mut state = projection("thread-parent");
    state.tasks = vec![task("agent", "thread-child", ItemStatus::Completed)];
    let graph = relationship_graph(&rows, Some(&state));

    assert_eq!(first_row_status(&graph, "thread-parent"), Some(expected));
}

#[test]
fn keeps_the_live_shell_when_an_archived_snapshot_contains_the_same_thread_id() {
    let live = with_shell(child_of("thread-child", "thread-parent", true), |row| {
        row.title = "Live child".into();
        row.status = Some(RunStatus::Running);
    });
    let stale = with_shell(
        child_of("thread-child", "thread-stale-parent", true),
        |row| {
            row.title = "Stale archived child".into();
            row.status = Some(RunStatus::Completed);
            row.archived_at = Some(at("2026-06-24T00:00:00.000Z"));
        },
    );
    let rows = [live, stale];
    let graph = relationship_graph(&rows, None);

    let child = graph.thread(&id("thread-child")).unwrap();
    assert_eq!(child.title, "Live child");
    assert_eq!(child.status, Some(RunStatus::Running));
    assert_eq!(child.archived_at, None);
    assert_eq!(graph.edges.len(), 1);
    assert_eq!(graph.edges[0].source, id("thread-parent"));
    assert_eq!(graph.edges[0].target, id("thread-child"));
    assert!(!graph.nodes.contains_key(&id("thread-stale-parent")));
}

#[test]
fn resolves_merge_back_only_for_forks() {
    let mut fork = projection("thread-fork");
    let thread = fork.thread.as_mut().unwrap();
    thread.parent = Some(id("thread-parent"));
    thread.fork_boundary = Some(1);
    assert_eq!(merge_back_target(&fork), Some(&id("thread-parent")));

    fork.thread.as_mut().unwrap().fork_boundary = None;
    assert_eq!(merge_back_target(&fork), None);
}

const CURRENT: &str = "thread-current";

fn fork_shell(thread: &str, parent: Option<&str>, created_at: &str) -> ThreadShell {
    let mut row = match parent {
        Some(parent) => child_of(thread, parent, true),
        None => shell(thread),
    };
    row.created_at = at(created_at);
    row
}

fn ordered_lineage_ids(
    rows: &[ThreadShell],
    merge_target: Option<&str>,
    projection: Option<&State>,
) -> Vec<String> {
    let graph = relationship_graph(rows, projection);
    let current = id(CURRENT);
    order_lineage_rows(
        &graph,
        immediate_thread_relationships(&graph, &current),
        &current,
        merge_target.map(id).as_ref(),
    )
    .into_iter()
    .map(|row| row.thread.to_string())
    .collect()
}

#[test]
fn orders_sibling_forks_newest_created_first() {
    assert_eq!(
        ordered_lineage_ids(
            &[
                fork_shell(CURRENT, None, "2026-06-01T00:00:00.000Z"),
                fork_shell(
                    "thread-fork-oldest",
                    Some(CURRENT),
                    "2026-06-02T00:00:00.000Z"
                ),
                fork_shell(
                    "thread-fork-middle",
                    Some(CURRENT),
                    "2026-06-03T00:00:00.000Z"
                ),
                fork_shell(
                    "thread-fork-newest",
                    Some(CURRENT),
                    "2026-06-04T00:00:00.000Z"
                ),
            ],
            None,
            None,
        ),
        [
            "thread-fork-newest",
            "thread-fork-middle",
            "thread-fork-oldest"
        ]
    );
}

#[test]
fn does_not_reorder_when_related_thread_activity_arrives() {
    let before = ordered_lineage_ids(
        &[
            fork_shell(CURRENT, None, "2026-06-01T00:00:00.000Z"),
            fork_shell("thread-fork-a", Some(CURRENT), "2026-06-02T00:00:00.000Z"),
            fork_shell("thread-fork-b", Some(CURRENT), "2026-06-03T00:00:00.000Z"),
            fork_shell("thread-fork-c", Some(CURRENT), "2026-06-04T00:00:00.000Z"),
        ],
        None,
        None,
    );
    // Rows arrive ordered by their update time, which moves while the panel
    // is open.
    let after = ordered_lineage_ids(
        &[
            with_shell(
                fork_shell("thread-fork-b", Some(CURRENT), "2026-06-03T00:00:00.000Z"),
                |row| {
                    row.updated_at = at("2026-07-30T00:00:00.000Z");
                    row.status = Some(RunStatus::Running);
                },
            ),
            fork_shell("thread-fork-c", Some(CURRENT), "2026-06-04T00:00:00.000Z"),
            fork_shell(CURRENT, None, "2026-06-01T00:00:00.000Z"),
            with_shell(
                fork_shell("thread-fork-a", Some(CURRENT), "2026-06-02T00:00:00.000Z"),
                |row| {
                    row.updated_at = at("2026-07-31T00:00:00.000Z");
                    row.status = Some(RunStatus::Failed);
                },
            ),
        ],
        None,
        None,
    );

    assert_eq!(before, ["thread-fork-c", "thread-fork-b", "thread-fork-a"]);
    assert_eq!(after, before);
}

#[test]
fn pins_the_parent_first_and_a_distinct_merge_back_target_second() {
    assert_eq!(
        ordered_lineage_ids(
            &[
                fork_shell("thread-parent", None, "2026-06-01T00:00:00.000Z"),
                fork_shell(CURRENT, Some("thread-parent"), "2026-06-02T00:00:00.000Z"),
                fork_shell(
                    "thread-merge-target",
                    Some(CURRENT),
                    "2026-06-03T00:00:00.000Z"
                ),
                fork_shell(
                    "thread-newest-fork",
                    Some(CURRENT),
                    "2026-06-09T00:00:00.000Z"
                ),
            ],
            Some("thread-merge-target"),
            None,
        ),
        ["thread-parent", "thread-merge-target", "thread-newest-fork"]
    );
}

#[test]
fn sinks_missing_nodes() {
    let mut state = projection(CURRENT);
    state.transfers = vec![transfer(CURRENT, "thread-missing-transfer")];
    assert_eq!(
        ordered_lineage_ids(
            &[
                fork_shell(CURRENT, None, "2026-06-01T00:00:00.000Z"),
                fork_shell(
                    "thread-dated-fork",
                    Some(CURRENT),
                    "2026-06-02T00:00:00.000Z"
                ),
            ],
            None,
            Some(&state),
        ),
        ["thread-dated-fork", "thread-missing-transfer"]
    );
}

#[test]
fn breaks_equal_creation_times_by_thread_id() {
    assert_eq!(
        ordered_lineage_ids(
            &[
                fork_shell(CURRENT, None, "2026-06-01T00:00:00.000Z"),
                fork_shell("thread-fork-c", Some(CURRENT), "2026-06-02T00:00:00.000Z"),
                fork_shell("thread-fork-a", Some(CURRENT), "2026-06-02T00:00:00.000Z"),
                fork_shell("thread-fork-b", Some(CURRENT), "2026-06-02T00:00:00.000Z"),
            ],
            None,
            None,
        ),
        ["thread-fork-a", "thread-fork-b", "thread-fork-c"]
    );
}

#[test]
fn shows_six_rows_before_the_first_expansion() {
    let window = lineage_window(20, None);
    assert_eq!(window.visible_count, 6);
    assert_eq!(window.hidden_count, 14);
}

#[test]
fn offers_one_page_at_a_time() {
    let first = lineage_window(20, None);
    assert_eq!(first.show_more_label.as_deref(), Some("Show 12 more"));
    let second = lineage_window(20, Some(first.next_visible_count));
    assert_eq!(second.show_more_label.as_deref(), Some("Show 2 more"));
}

#[test]
fn omits_the_expansion_affordance_when_everything_fits() {
    assert_eq!(lineage_window(20, Some(20)).show_more_label, None);
    assert_eq!(lineage_window(6, Some(6)).hidden_count, 0);
}

fn project(id: &str, name: &str, root: &str) -> Project {
    Project {
        id: id.into(),
        name: name.into(),
        roots: vec![ProjectRoot { path: root.into() }],
        ..Default::default()
    }
}

fn snapshot(rows: Vec<ThreadShell>, states: Vec<State>, projects: Vec<Project>) -> Snapshot {
    let mut cache = ShellCache::default();
    cache.snapshot = Some(ShellSnapshot {
        snapshot_sequence: 1,
        projects,
        threads: rows,
    });
    let threads = states
        .into_iter()
        .map(|state| {
            let mut sync = ThreadSync::default();
            let thread = state.thread.as_ref().unwrap().id.clone();
            sync.state = Some(Arc::new(state));
            (thread, Arc::new(sync))
        })
        .collect();
    Snapshot {
        shell: Arc::new(cache),
        threads,
        ..Default::default()
    }
}

fn rows(panel: &LineagePanel) -> Vec<&LineageRow> {
    panel.groups.iter().flat_map(|group| &group.rows).collect()
}

fn checker(status: ItemStatus) -> Task {
    Task {
        title: Some("Checker".into()),
        progress: Some("Running checks".into()),
        ..task("agent-1", "child-1", status)
    }
}

const NOW: &str = "2026-09-16T12:05:00Z";

fn parent_panel(tasks: Vec<Task>) -> LineagePanel {
    let mut state = projection("parent");
    state.tasks = tasks;
    lineage_panel(
        &snapshot(vec![], vec![state], vec![]),
        &id("parent"),
        at(NOW).millis(),
    )
    .unwrap()
}

#[test]
fn shows_the_matching_child_agent_details_and_refreshes_them_when_the_agent_settles() {
    let worker = Task {
        title: Some("Worker".into()),
        model: Some("gpt-5.3".into()),
        ..task("agent-2", "child-2", ItemStatus::Running)
    };
    let panel = parent_panel(vec![checker(ItemStatus::Running), worker]);
    assert_eq!(panel.title, "Lineage · 2 running");
    assert_eq!(panel.groups.len(), 1);
    assert_eq!(panel.groups[0].kind, LineageGroupKind::Active);
    assert_eq!(panel.groups[0].label, None);
    let checker_row = rows(&panel)
        .into_iter()
        .find(|row| row.title == "Checker")
        .unwrap();
    assert_eq!(checker_row.status_label, "Running");

    let settled = Task {
        progress: None,
        result: Some("All checks passed".into()),
        completed_at: Some(at("2026-09-16T12:02:15Z")),
        ..checker(ItemStatus::Completed)
    };
    let panel = parent_panel(vec![settled]);
    assert_eq!(panel.title, "Lineage");
    let previous = &panel.groups[0];
    assert_eq!(previous.kind, LineageGroupKind::Previous);
    assert!(!previous.expanded_by_default);
    assert_eq!(
        lineage_group_title(previous.label.as_deref().unwrap(), 1, false),
        "Previous agents (1)"
    );
    assert_eq!(
        lineage_group_title("Previous agents", 1, true),
        "Previous agents"
    );
    let row = &previous.rows[0];
    assert_eq!(row.title, "Checker");
    assert_eq!(
        row.agent.as_ref().unwrap().elapsed.as_deref(),
        Some("2m 15s")
    );
    assert_eq!(row.status_label, "Done");

    let running: Vec<_> = (0..8)
        .map(|index| {
            task(
                &format!("running-agent-{index}"),
                &format!("running-child-{index}"),
                ItemStatus::Running,
            )
        })
        .collect();
    assert_eq!(parent_panel(running).title, "Lineage · 8 running");

    let old: Vec<_> = (0..8)
        .map(|index| Task {
            title: Some(format!("Old agent {index}")),
            result: Some(
                if index == 7 {
                    "Earlier build failed"
                } else {
                    "Done"
                }
                .into(),
            ),
            completed_at: Some(at("2026-09-16T12:02:15Z")),
            ..task(
                &format!("old-agent-{index}"),
                &format!("old-child-{index}"),
                if index == 7 {
                    ItemStatus::Failed
                } else {
                    ItemStatus::Completed
                },
            )
        })
        .collect();
    let panel = parent_panel(old);
    let previous = &panel.groups[0];
    assert_eq!(previous.failed_label.as_deref(), Some("1 failed"));
    let window = lineage_window(previous.rows.len() as u32, None);
    let visible = &previous.rows[..window.visible_count as usize];
    assert!(!visible.iter().any(|row| row.title == "Old agent 7"));
    let window = lineage_window(previous.rows.len() as u32, Some(window.next_visible_count));
    let visible = &previous.rows[..window.visible_count as usize];
    assert!(visible.iter().any(|row| row.title == "Old agent 7"));
}

fn tooltip_snapshot(task: Task, child: Option<ThreadShell>, models: Vec<Model>) -> Snapshot {
    let mut state = projection("parent");
    state.thread.as_mut().unwrap().project = "main".into();
    state.tasks = vec![task];
    let mut snapshot = snapshot(
        child.into_iter().collect(),
        vec![state],
        vec![
            project("main", "Main", "/main"),
            project("other", "Other project", "/other"),
        ],
    );
    snapshot.models = models;
    snapshot
}

fn worker_child() -> ThreadShell {
    with_shell(child_of("child", "parent", false), |row| {
        row.project = "main".into();
        row.title = "Worker".into();
    })
}

fn worker(model: Option<&str>) -> Task {
    Task {
        title: Some("Worker".into()),
        model: model.map(Into::into),
        ..task("agent", "child", ItemStatus::Pending)
    }
}

fn catalog() -> Vec<Model> {
    vec![Model {
        id: "gpt-5.4".into(),
        model: ModelRef {
            provider: ProviderKind::Codex,
            id: "gpt-5.4".into(),
        },
        display_name: "My GPT model".into(),
        default_reasoning_effort: String::new(),
        supported_reasoning_efforts: vec![],
        service_tiers: None,
        default_service_tier: None,
        is_default: None,
    }]
}

fn tooltip(snapshot: &Snapshot) -> LineageAgent {
    let panel = lineage_panel(snapshot, &id("parent"), at(NOW).millis()).unwrap();
    rows(&panel)[0].agent.clone().unwrap()
}

#[test]
fn shows_readable_models_and_only_differing_workspace_details_in_agent_tooltips() {
    let agent = tooltip(&tooltip_snapshot(
        worker(Some("gpt-5.4")),
        Some(worker_child()),
        catalog(),
    ));
    assert!(agent.model_label.contains("My GPT"));
    assert_eq!(agent.workspace, vec![]);

    for (model, expected) in [
        (None, "Not reported"),
        (Some(""), "Not reported"),
        (Some("   "), "Not reported"),
        (Some("gpt-5.5"), "GPT-5.5"),
        (Some("custom/model-v1"), "custom/model-v1"),
    ] {
        let agent = tooltip(&tooltip_snapshot(
            worker(model),
            Some(worker_child()),
            catalog(),
        ));
        assert_eq!(agent.model_label, expected);
    }

    let agent = tooltip(&tooltip_snapshot(
        Task {
            progress: Some("Checking the latest changes".into()),
            result: Some("Old intermediate result".into()),
            ..worker(None)
        },
        Some(worker_child()),
        catalog(),
    ));
    assert_eq!(
        agent.preview.as_deref(),
        Some("Checking the latest changes")
    );

    let result = format!(
        "Final checks passed. {}Hidden tail",
        "More detail. ".repeat(50)
    );
    let agent = tooltip(&tooltip_snapshot(
        Task {
            status: ItemStatus::Failed,
            progress: Some("Stale progress".into()),
            result: Some(result.clone()),
            ..worker(None)
        },
        Some(worker_child()),
        catalog(),
    ));
    let preview = agent.preview.unwrap();
    assert!(preview.starts_with("Final checks passed."));
    assert!(!preview.contains("Stale progress"));
    assert!(!preview.contains("Hidden tail"));
    assert_ne!(preview, result);

    let workspace = |change: fn(&mut ThreadShell)| {
        tooltip(&tooltip_snapshot(
            worker(None),
            Some(with_shell(worker_child(), change)),
            catalog(),
        ))
        .workspace
        .into_iter()
        .map(|entry| (entry.label, entry.value))
        .collect::<Vec<_>>()
    };
    assert_eq!(
        workspace(|row| {
            row.workspace = Some(Workspace {
                cwd: "/main".into(),
                worktree_path: Some("/main/worktrees/checker".into()),
                branch: None,
            })
        }),
        [("Worktree".to_string(), "checker".to_string())]
    );
    assert_eq!(
        workspace(|row| {
            row.workspace = Some(Workspace {
                cwd: "/main".into(),
                worktree_path: Some("/main/worktrees/checker".into()),
                branch: Some("fix/checker".into()),
            })
        }),
        [("Branch".to_string(), "fix/checker".to_string())]
    );
    assert_eq!(
        workspace(|row| row.project = "other".into()),
        [
            ("Project".to_string(), "Other project".to_string()),
            ("Workspace".to_string(), "other".to_string()),
        ]
    );

    let agent = tooltip(&tooltip_snapshot(worker(Some("gpt-5.4")), None, vec![]));
    assert_eq!(agent.model_label, "GPT-5.4");
    assert_eq!(agent.workspace, vec![]);
}

#[test]
fn shows_the_parents_own_visible_status_as_parent_and_child_activity_change() {
    let parent = with_shell(shell("parent"), |row| {
        row.title = "Parent conversation".into();
        row.status = Some(RunStatus::Completed);
        row.activity_run_status = Some(RunStatus::Running);
    });
    let child = with_shell(child_of("child", "parent", true), |row| {
        row.title = "Current fork".into();
        row.status = Some(RunStatus::Completed);
    });
    let mut state = projection("child");
    let thread = state.thread.as_mut().unwrap();
    thread.parent = Some(id("parent"));
    thread.fork_boundary = Some(1);
    let panel = |parent: ThreadShell, child: ThreadShell| {
        lineage_panel(
            &snapshot(vec![parent, child], vec![state.clone()], vec![]),
            &id("child"),
            0,
        )
        .unwrap()
    };

    let before = panel(parent.clone(), child.clone());
    let row = rows(&before)[0];
    assert_eq!(row.title, "Parent conversation");
    assert_eq!(row.status_label, "Running");

    let after = panel(
        with_shell(parent, |row| row.activity_run_status = None),
        with_shell(child, |row| row.status = Some(RunStatus::Running)),
    );
    let labels: Vec<_> = rows(&after)
        .iter()
        .map(|row| row.status_label.as_str())
        .collect();
    assert_eq!(labels, ["Done"]);
}

#[rstest]
#[case(None, false, "Queued")]
#[case(Some(ContextDeliveryStatus::Pending), false, "Resolved (portable)")]
#[case(Some(ContextDeliveryStatus::Injected), false, "Consumed")]
#[case(Some(ContextDeliveryStatus::Inline), false, "Consumed")]
#[case(Some(ContextDeliveryStatus::NativeFork), false, "Consumed")]
#[case(None, true, "Superseded")]
fn shows_transfer_lifecycle_states_when_viewing_the_target_thread(
    #[case] delivery: Option<ContextDeliveryStatus>,
    #[case] superseded: bool,
    #[case] label: &str,
) {
    let threads = ["source", "target"].map(|thread| {
        with_shell(shell(thread), |row| {
            row.title = format!("{thread} conversation");
            row.status = Some(RunStatus::Running);
            row.activity_run_status = Some(RunStatus::Running);
        })
    });
    let mut state = projection("target");
    state.transfers = vec![Transfer {
        delivery: delivery.map(|status| ContextDelivery {
            attempt: RunAttemptId::new("attempt").unwrap(),
            run: RunId::new("run").unwrap(),
            native_thread: None,
            status,
            item_ids: vec![],
            omitted_item_ids: vec![],
        }),
        superseded,
        ..transfer("source", "target")
    }];
    let panel = lineage_panel(
        &snapshot(threads.to_vec(), vec![state], vec![]),
        &id("target"),
        0,
    )
    .unwrap();
    let row = rows(&panel)[0];

    assert_eq!(row.title, "source conversation");
    assert_eq!(row.status_label, label);
}

#[test]
fn offers_merge_back_into_the_fork_source_once_a_run_finished() {
    let source = with_shell(shell("source"), |row| row.title = "Source".into());
    let fork = child_of("fork", "source", true);
    let mut state = projection("fork");
    let thread = state.thread.as_mut().unwrap();
    thread.parent = Some(id("source"));
    thread.fork_boundary = Some(1);
    let merge = |state: &State| {
        let panel = lineage_panel(
            &snapshot(
                vec![source.clone(), fork.clone()],
                vec![state.clone()],
                vec![],
            ),
            &id("fork"),
            0,
        )
        .unwrap();
        rows(&panel)[0].merge_back.clone().unwrap()
    };

    let waiting = merge(&state);
    assert!(!waiting.enabled);
    assert_eq!(waiting.label, "Merge back to Source");
    assert_eq!(
        waiting.tooltip,
        "Complete a run in this fork before merging it back"
    );

    state.runs = vec![run("run-1", 1, RunStatus::Completed)];
    let ready = merge(&state);
    assert!(ready.enabled);
    assert_eq!(ready.tooltip, "Merge this conversation back into Source");
    assert_eq!(
        merge_back_action(&state),
        Some(MergeBackAction {
            target_thread_id: "source".into(),
            run_id: "run-1".into(),
        })
    );
}

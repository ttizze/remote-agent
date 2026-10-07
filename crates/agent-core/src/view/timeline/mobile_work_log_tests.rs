use super::*;
use crate::view::timeline::mobile::{
    FeedInput, build_thread_feed, derive_thread_feed_presentation,
};
use crate::view::work_log::fixtures::*;
use agent_domain::{RunStatus, Task, Timestamp};
use serde_json::json;

fn ts(value: &str) -> Timestamp {
    Timestamp::parse(value).unwrap()
}

fn activities_of(state: &State) -> Vec<FeedActivity> {
    build_thread_feed(state)
        .iter()
        .flat_map(|row| row.activities().to_vec())
        .collect()
}

fn thread(items: Vec<Item>) -> State {
    State {
        runs: vec![run("run-1", 1, RunStatus::Completed)],
        ..state(items)
    }
}

fn activity_row(row: WorkLogRow) -> WorkActivityRow {
    match row {
        WorkLogRow::Activity(row) => *row,
        row => panic!("expected an activity row: {row:?}"),
    }
}

mod thread_activity_row_presentation {
    use super::*;

    #[test]
    fn shows_the_provider_and_model_as_compact_metadata() {
        assert_eq!(
            thread_activity_metadata(Some(Driver::Codex), Some("codex"), Some("gpt-5.4")),
            "Codex · gpt-5.4"
        );
    }

    #[test]
    fn falls_back_to_the_provider_instance_and_removes_duplicate_metadata() {
        assert_eq!(
            thread_activity_metadata(None, Some("custom-agent"), Some("custom-agent")),
            "custom-agent"
        );
    }

    #[test]
    fn maps_lifecycle_status_to_dot_tone_and_an_accessible_label() {
        let status = |label: &str, tone| ActivityStatus {
            label: label.into(),
            tone,
        };
        assert_eq!(
            thread_activity_status(ItemStatus::Running),
            status("Running", ActivityStatusTone::Active)
        );
        assert_eq!(
            thread_activity_status(ItemStatus::Completed),
            status("Completed", ActivityStatusTone::Success)
        );
        assert_eq!(
            thread_activity_status(ItemStatus::Failed),
            status("Failed", ActivityStatusTone::Danger)
        );
        assert_eq!(
            thread_activity_status(ItemStatus::Interrupted),
            status("Interrupted", ActivityStatusTone::Warning)
        );
    }
}

mod thread_content_presentation {
    use super::*;

    #[test]
    fn renders_cached_detail_while_its_environment_reconnects() {
        assert_eq!(
            thread_content_presentation(true, None, false, ContentConnectionPhase::Reconnecting),
            ThreadContentPresentation::Ready
        );
    }

    #[test]
    fn loads_missing_detail_inside_the_thread_screen_when_connected() {
        assert_eq!(
            thread_content_presentation(false, None, false, ContentConnectionPhase::Connected),
            ThreadContentPresentation::Loading
        );
    }

    #[test]
    fn explains_uncached_detail_while_disconnected_instead_of_loading_forever() {
        assert_eq!(
            thread_content_presentation(false, None, false, ContentConnectionPhase::Error),
            ThreadContentPresentation::Unavailable {
                title: "Messages not cached".into(),
                detail: "Reconnect this environment to load the conversation.".into(),
            }
        );
    }

    #[test]
    fn surfaces_detail_errors_before_presenting_a_loading_state() {
        assert_eq!(
            thread_content_presentation(
                false,
                Some("The thread stream failed."),
                false,
                ContentConnectionPhase::Connected
            ),
            ThreadContentPresentation::Unavailable {
                title: "Could not load conversation".into(),
                detail: "The thread stream failed.".into(),
            }
        );
    }
}

#[test]
fn keeps_inherited_activity_file_links_on_the_currently_selected_thread() {
    let current = ThreadId::new("current-thread").unwrap();
    assert_eq!(
        activity_file_target(&current, "apps/mobile/src/index.ts", Some(12.0)),
        ActivityFileTarget {
            thread: current.clone(),
            path: vec![
                "apps".into(),
                "mobile".into(),
                "src".into(),
                "index.ts".into()
            ],
            line: Some("12".into()),
        }
    );
}

mod subagent_card {
    use super::*;

    fn done() -> SubagentTiming {
        SubagentTiming {
            status: ItemStatus::Completed,
            started_at: Some(ts("2026-09-21T12:00:00Z")),
            completed_at: Some(ts("2026-09-21T12:01:00Z")),
        }
    }

    fn later() -> i64 {
        ts("2026-09-21T12:02:00Z").millis()
    }

    #[test]
    fn freezes_settled_durations_and_spans_the_groups_wall_time() {
        assert_eq!(
            subagent_card_elapsed(&[done()], later()).as_deref(),
            Some("1m")
        );
        let second = SubagentTiming {
            started_at: Some(ts("2026-09-21T12:01:00Z")),
            completed_at: Some(ts("2026-09-21T12:02:00Z")),
            ..done()
        };
        assert_eq!(
            subagent_card_elapsed(&[done(), second], later()).as_deref(),
            Some("2m")
        );
    }

    #[test]
    fn counts_live_work_but_never_uses_a_settled_agents_age_as_its_duration() {
        let unfinished = SubagentTiming {
            completed_at: None,
            ..done()
        };
        assert_eq!(
            subagent_card_elapsed(std::slice::from_ref(&unfinished), later()),
            None
        );
        assert_eq!(
            subagent_card_elapsed(
                &[SubagentTiming {
                    status: ItemStatus::Running,
                    ..unfinished
                }],
                later()
            )
            .as_deref(),
            Some("2m")
        );
        assert_eq!(
            subagent_card_elapsed(
                &[SubagentTiming {
                    started_at: None,
                    ..done()
                }],
                later()
            ),
            None
        );
    }

    #[test]
    fn summarizes_a_group_of_subagents_and_lists_members_when_expanded() {
        let item = |id: &str| subagent(id, id).run("run-1");
        let task = |id: &str, status| Task {
            status,
            title: Some(format!("Agent {id}")),
            ..task(id, &format!("child-{id}"), "Task")
        };
        let state = State {
            tasks: vec![
                task("a", ItemStatus::Running),
                task("b", ItemStatus::Completed),
                task("c", ItemStatus::Failed),
            ],
            ..thread(vec![item("a"), item("b"), item("c")])
        };
        let activities = activities_of(&state);
        let card = subagent_group_card(&state, &activities, false, 0);
        assert!(card.grouped);
        assert_eq!(card.label, "3 subagents");
        assert_eq!(card.summary, "1 working · 1 done · 1 failed");
        assert_eq!(
            card.accessibility_label,
            "3 subagents, 1 working · 1 done · 1 failed"
        );
        assert_eq!(card.tone, SubagentGroupTone::Active);
        assert!(!card.shows_members);
        let card = subagent_group_card(&state, &activities, true, 0);
        assert!(card.shows_members);
        assert_eq!(
            card.members
                .iter()
                .map(|member| member.link.title.as_str())
                .collect::<Vec<_>>(),
            ["Agent a", "Agent b", "Agent c"]
        );
        assert_eq!(
            card.members[0].accessibility_hint,
            "Opens this agent's thread"
        );
    }
}

#[test]
fn loads_withheld_output_only_when_an_expanded_row_needs_it() {
    let item = command("item-command", "vp check")
        .run("run-1")
        .output_omitted();
    let state = thread(vec![item.clone()]);
    let activity = activities_of(&state).remove(0);
    let collapsed = activity_row(work_log_row(&state, &activity, false, None));
    assert!(!collapsed.load_detail);
    assert_eq!(collapsed.detail, None);
    assert_eq!(
        collapsed.accessibility_hint,
        "Double tap to show full details. Long press to copy."
    );

    let loading = activity_row(work_log_row(
        &state,
        &activity,
        true,
        Some(&Detail::Loading),
    ));
    assert!(loading.load_detail);
    let detail = loading.detail.unwrap();
    assert_eq!(detail.call.unwrap().command.as_deref(), Some("vp check"));
    assert_eq!(detail.output.as_deref(), Some("Loading output…"));

    let failed = activity_row(work_log_row(
        &state,
        &activity,
        true,
        Some(&Detail::Failed("offline".into())),
    ));
    assert_eq!(
        failed.detail.unwrap().output.as_deref(),
        Some("Couldn't load output: offline")
    );

    let loaded = Item {
        output_omitted: false,
        ..item.text("all checks passed").exit_code(Some(2))
    };
    let shown = activity_row(work_log_row(
        &state,
        &activity,
        true,
        Some(&Detail::Loaded(Box::new(loaded))),
    ));
    let detail = shown.detail.unwrap();
    assert_eq!(detail.output.as_deref(), Some("all checks passed"));
    assert_eq!(detail.failed_exit_code, Some(2));
}

#[test]
fn keeps_read_paths_as_the_detail_of_an_expanded_read() {
    let item = dynamic_tool("read", "Read", json!({ "path": "src/env.ts" }), None)
        .run("run-1")
        .output_omitted();
    let state = thread(vec![item]);
    let activity = activities_of(&state).remove(0);
    let row = activity_row(work_log_row(&state, &activity, true, None));
    let detail = row.detail.unwrap();
    assert_eq!(detail.call, None);
    assert_eq!(detail.full_detail.as_deref(), Some("src/env.ts"));
    assert_eq!(detail.output.as_deref(), Some("Loading output…"));
}

#[test]
fn draws_a_failed_turn_as_a_provider_failure_with_its_message() {
    let mut item = error("failure", "The provider stopped this turn.")
        .run("run-1")
        .status(ItemStatus::Failed)
        .completed(Some("2026-06-20T00:00:00Z"));
    if let ItemKind::Error { class, .. } = &mut item.kind {
        *class = Some("usage_limit".into());
    }
    let state = thread(vec![item]);
    let activity = activities_of(&state).remove(0);
    let WorkLogRow::ProviderFailure(failure) = work_log_row(&state, &activity, false, None) else {
        panic!("expected a provider failure");
    };
    assert!(failure.warning);
    assert_eq!(
        failure.label(Some("Oct 7, 9:00 AM")),
        "Usage limit reached. Retry after Oct 7, 9:00 AM."
    );
    assert_eq!(failure.label(None), "Usage limit reached.");
    assert_eq!(failure.retry_preparation, None);
}

#[test]
fn describes_a_tool_toggle_for_assistive_technologies() {
    let state = thread(vec![
        command("one", "vp check").run("run-1").ordinal(1),
        command("two", "vp test").run("run-1").ordinal(2),
    ]);
    let rows = derive_thread_feed_presentation(
        &build_thread_feed(&state),
        &FeedInput {
            latest_run: Some(crate::view::timeline::mobile::FeedLatestRun::from(
                &state.runs[0],
            )),
            expanded_runs: [agent_domain::RunId::new("run-1").unwrap()].into(),
            ..FeedInput::default()
        },
    );
    let toggle = rows
        .iter()
        .find_map(|row| match row {
            crate::view::timeline::mobile::FeedRow::WorkToggle(toggle) => Some(toggle),
            _ => None,
        })
        .unwrap();
    let presentation = work_toggle_presentation(toggle);
    assert_eq!(presentation.accessibility_label, "Ran 2 commands");
    assert_eq!(
        presentation.accessibility_hint,
        "Double tap to show 2 tool calls."
    );
    assert_eq!(
        presentation.icon,
        WorkToggleIcon::Summary(ToolGroupSummaryKind::Action(ToolGroupAction::Command))
    );
}

#[test]
fn chooses_the_renderer_of_a_group() {
    let state = thread(vec![
        reasoning("thought", "Checking").run("run-1"),
        subagent("agent", "agent").run("run-1").ordinal(2),
    ]);
    let activities = activities_of(&state);
    assert_eq!(work_log_layout(&[]), WorkLogLayout::Empty);
    assert_eq!(work_log_layout(&activities[..1]), WorkLogLayout::Rows);
    assert_eq!(work_log_layout(&activities[1..]), WorkLogLayout::Subagents);
    let grouped = FeedActivity {
        grouped_tool_detail: true,
        ..activities[0].clone()
    };
    assert_eq!(
        work_log_layout(std::slice::from_ref(&grouped)),
        WorkLogLayout::Reasoning
    );
}

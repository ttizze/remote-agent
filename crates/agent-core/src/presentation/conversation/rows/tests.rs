use super::*;
use crate::{
    localization::UiLanguage,
    models::{Item, Thread, Turn},
    presentation::conversation::project_conversation,
    state::DraftKey,
};
use agent_protocol::{
    execution::TurnStatus,
    requests::{Request, RequestBody, RequestTarget},
    session::RequestDelivery,
};
use serde_json::json;

#[test]
fn hidden_search_results_keep_visible_identity_and_reject_another_storage_scope() {
    let mut source = settled_source(true);
    items(&mut source).insert(
        0,
        Arc::new(
            serde_json::from_value(json!({
                "id":"comment", "status":"completed", "body":{"inline":{"body":{
                    "assistantText":{"text":"Hidden commentary", "phase":"commentary"}
                }}}
            }))
            .unwrap(),
        ),
    );
    let raw = render(source, None);
    let collapsed = project(raw.clone(), None);
    let hidden = collapsed
        .searchable_rows()
        .into_iter()
        .find(|row| {
            matches!(&row.data.content, ConversationRowContent::Activity { item, .. }
            if item.data.body.as_deref() == Some("Hidden commentary"))
        })
        .unwrap();
    assert!(!collapsed.rows.iter().any(|row| row.id == hidden.id));
    let change = collapsed
        .reveal_message(hidden.id.clone(), collapsed.expansion())
        .unwrap();
    let opened = expanded(raw.clone(), change.expansion, None);
    assert!(opened.rows.iter().any(|row| row.id == hidden.id));
    let foreign = ConversationRows::project(
        crate::presentation::conversation::TimelineStyle::NATIVE,
        raw,
        PanelScope {
            storage: "new-account".into(),
            ..scope()
        },
        None,
        HashMap::new(),
        None,
    );
    assert!(
        foreign
            .reveal_message(hidden.id.clone(), HashMap::new())
            .is_none()
    );
}

#[test]
fn hidden_search_disclosures_distinguish_repeated_provider_turn_and_item_ids() {
    let mut source = settled_source(true);
    items(&mut source).insert(
        0,
        Arc::new(
            serde_json::from_value(json!({
                "id":"comment", "status":"completed", "body":{"inline":{"body":{
                    "assistantText":{"text":"Commentary", "phase":"commentary"}
                }}}
            }))
            .unwrap(),
        ),
    );
    let duplicate = source.turns.as_ref().unwrap()[0].clone();
    Arc::make_mut(&mut source)
        .turns
        .as_mut()
        .unwrap()
        .push(duplicate);
    let raw = render(source, None);
    let collapsed = project(raw.clone(), None);
    let comments: Vec<_> = collapsed
        .searchable_rows()
        .into_iter()
        .filter(|row| {
            matches!(&row.data.content, ConversationRowContent::Activity { item, .. }
            if item.data.body.as_deref() == Some("Commentary"))
        })
        .collect();
    assert_eq!(comments.len(), 2);
    assert_ne!(comments[0].id, comments[1].id);
    let change = collapsed
        .reveal_message(comments[0].id.clone(), HashMap::new())
        .unwrap();
    let opened = expanded(raw, change.expansion, None);
    assert!(opened.rows.iter().any(|row| row.id == comments[0].id));
    assert!(!opened.rows.iter().any(|row| row.id == comments[1].id));
}

fn footer_queue(thread: &Thread) -> Arc<crate::presentation::queue::QueuePresentation> {
    use crate::presentation::queue::{QueuePresentation, QueuedMessage};
    let mut queue = QueuePresentation::empty(UiLanguage::English);
    let value = Arc::make_mut(&mut queue);
    value.session = thread.id.clone();
    value.can_change = true;
    value.messages = ["a", "b", "c", "d", "e"]
        .into_iter()
        .map(|id| QueuedMessage {
            id: id.into(),
            text: id.into(),
            attachments: 0,
            pending: false,
            fresh_context: false,
        })
        .collect();
    queue
}

#[test]
fn the_queue_is_the_final_scoped_footer_and_never_counts_as_history() {
    let mut thread = settled_source(true);
    Arc::make_mut(&mut thread)
        .background
        .push(agent_protocol::execution::BackgroundTask {
            id: "shell".into(),
            kind: agent_protocol::execution::BackgroundTaskKind::Command,
            description: Some("watch".into()),
        });
    let queue = footer_queue(&thread);
    let raw = render(thread.clone(), None);
    let without = project(raw.clone(), None);
    let rows = ConversationRows::project(
        crate::presentation::conversation::TimelineStyle::NATIVE,
        raw.clone(),
        scope(),
        Some(queue.clone()),
        HashMap::new(),
        None,
    );
    let footer = rows.session_rows();
    assert_eq!(footer.len(), 2);
    assert!(matches!(
        footer[0].data.content,
        ConversationRowContent::Background { .. }
    ));
    assert!(!footer[0].last);
    assert!(matches!(
        footer[1].data.content,
        ConversationRowContent::Queue { .. }
    ));
    assert!(footer[1].last);
    assert_eq!(rows.rows.last().unwrap().id, footer[1].id);
    assert_eq!(rows.gaps().last(), Some(&16.));
    assert_eq!(rows.source_item_count(), 2);
    assert_eq!(rows.source_item_count(), without.source_item_count());
    assert!(
        rows.queued_rows().is_empty(),
        "delivery recovery remains a separate history section"
    );
    assert!(Arc::ptr_eq(
        &rows,
        &ConversationRows::project(
            crate::presentation::conversation::TimelineStyle::NATIVE,
            raw.clone(),
            scope(),
            Some(queue.clone()),
            HashMap::new(),
            Some(rows.clone())
        )
    ));

    let mut changed = queue.clone();
    Arc::make_mut(&mut changed).busy = true;
    let next = ConversationRows::project(
        crate::presentation::conversation::TimelineStyle::NATIVE,
        raw,
        scope(),
        Some(changed),
        HashMap::new(),
        Some(rows.clone()),
    );
    assert!(
        !Arc::ptr_eq(&rows, &next),
        "composer-only changes must refresh queue controls"
    );
    assert!(
        Arc::ptr_eq(&rows.rows[0], &next.rows[0]),
        "unaffected transcript cells remain reusable"
    );
    let ConversationRowContent::Queue { queue: current } = &next.rows.last().unwrap().data.content
    else {
        panic!("queue")
    };
    assert!(
        current
            .action("a".into(), crate::presentation::queue::QueueControl::Edit)
            .is_none()
    );
    assert!(
        queue
            .action("a".into(), crate::presentation::queue::QueueControl::Edit)
            .is_some()
    );
}

#[test]
fn queue_disclosure_survives_delivery_but_an_empty_queue_retires_its_choice() {
    let thread = settled_source(false);
    let mut queue = footer_queue(&thread);
    let raw = render(thread, None);
    let closed = ConversationRows::project(
        crate::presentation::conversation::TimelineStyle::NATIVE,
        raw.clone(),
        scope(),
        Some(queue.clone()),
        HashMap::new(),
        None,
    );
    let id = closed.rows.last().unwrap().id();
    let open = closed
        .toggle_disclosure(id.clone(), HashMap::new())
        .unwrap()
        .expansion;
    assert!(
        closed
            .toggle_disclosure(id.clone(), open.clone())
            .unwrap()
            .expansion
            .is_empty(),
        "a second click uses the owner's current choice before its next render"
    );
    let opened = ConversationRows::project(
        crate::presentation::conversation::TimelineStyle::NATIVE,
        raw.clone(),
        scope(),
        Some(queue.clone()),
        open.clone(),
        Some(closed.clone()),
    );
    assert!(opened.rows.last().unwrap().expanded());
    assert!(!closed.rows.last().unwrap().expanded());
    Arc::make_mut(&mut queue).messages.remove(0);
    let delivered = ConversationRows::project(
        crate::presentation::conversation::TimelineStyle::NATIVE,
        raw.clone(),
        scope(),
        Some(queue.clone()),
        open.clone(),
        Some(opened),
    );
    assert_eq!(delivered.rows.last().unwrap().id(), id);
    assert!(delivered.rows.last().unwrap().expanded());
    let mut choices = open;
    choices.insert("another-pane-choice".into(), "open".into());
    let empty = ConversationRows::project(
        crate::presentation::conversation::TimelineStyle::NATIVE,
        raw.clone(),
        scope(),
        None,
        choices,
        Some(delivered),
    );
    assert!(!empty.expansion().contains_key(&id));
    assert!(empty.expansion().contains_key("another-pane-choice"));
    assert!(
        empty
            .toggle_disclosure(id.clone(), HashMap::new())
            .is_none()
    );
    let restarted = ConversationRows::project(
        crate::presentation::conversation::TimelineStyle::NATIVE,
        raw,
        scope(),
        Some(queue),
        empty.expansion(),
        Some(empty),
    );
    assert_ne!(restarted.rows.last().unwrap().id(), id);
    assert!(!restarted.rows.last().unwrap().expanded());
}

#[test]
fn editing_a_hidden_message_opens_its_controls_without_overruling_a_later_collapse() {
    let thread = settled_source(false);
    let mut queue = footer_queue(&thread);
    let raw = render(thread, None);
    let closed = ConversationRows::project(
        crate::presentation::conversation::TimelineStyle::NATIVE,
        raw.clone(),
        scope(),
        Some(queue.clone()),
        HashMap::new(),
        None,
    );
    let id = closed.rows.last().unwrap().id();
    Arc::make_mut(&mut queue).editing = Some("c".into());
    let editing = ConversationRows::project(
        crate::presentation::conversation::TimelineStyle::NATIVE,
        raw.clone(),
        scope(),
        Some(queue.clone()),
        HashMap::new(),
        Some(closed),
    );
    assert!(
        editing.rows.last().unwrap().expanded(),
        "T3 editLatest exposes the tail's Cancel button"
    );
    assert_eq!(
        editing.expansion().get(&id).map(String::as_str),
        Some("open")
    );
    assert_eq!(
        queue
            .messages(editing.rows.last().unwrap().expanded())
            .last()
            .unwrap()
            .id
            .as_str(),
        "e"
    );
    let collapsed = editing
        .toggle_disclosure(id.clone(), editing.expansion())
        .unwrap()
        .expansion;
    let fewer = ConversationRows::project(
        crate::presentation::conversation::TimelineStyle::NATIVE,
        raw.clone(),
        scope(),
        Some(queue.clone()),
        collapsed,
        Some(editing.clone()),
    );
    assert!(
        !fewer.rows.last().unwrap().expanded(),
        "Show fewer remains an explicit user choice"
    );

    Arc::make_mut(&mut queue).editing = None;
    let cancelled = ConversationRows::project(
        crate::presentation::conversation::TimelineStyle::NATIVE,
        raw.clone(),
        scope(),
        Some(queue.clone()),
        editing.expansion(),
        Some(editing),
    );
    assert!(
        cancelled.rows.last().unwrap().expanded(),
        "Cancel editing does not reset Show all"
    );
    Arc::make_mut(&mut queue).editing = Some("a".into());
    let first = ConversationRows::project(
        crate::presentation::conversation::TimelineStyle::NATIVE,
        raw,
        scope(),
        Some(queue),
        HashMap::new(),
        Some(fewer),
    );
    assert!(
        !first.rows.last().unwrap().expanded(),
        "a visible row needs no extra expansion"
    );
}

#[test]
fn a_queue_from_another_conversation_or_without_an_owner_is_never_mounted() {
    let thread = settled_source(false);
    let mut queue = footer_queue(&thread);
    Arc::make_mut(&mut queue).session.as_mut().unwrap().id = "another".into();
    let raw = render(thread.clone(), None);
    let wrong = ConversationRows::project(
        crate::presentation::conversation::TimelineStyle::NATIVE,
        raw.clone(),
        scope(),
        Some(queue.clone()),
        HashMap::new(),
        None,
    );
    assert!(wrong.session_rows().is_empty());
    Arc::make_mut(&mut queue).session = None;
    let mut unowned_thread = thread;
    Arc::make_mut(&mut unowned_thread).id = None;
    let unowned = ConversationRows::project(
        crate::presentation::conversation::TimelineStyle::NATIVE,
        render(unowned_thread, None),
        scope(),
        Some(queue),
        HashMap::new(),
        None,
    );
    assert!(unowned.session_rows().is_empty());
}

#[test]
fn background_status_updates_in_the_scoped_footer_without_becoming_a_transcript_item() {
    use agent_protocol::execution::{BackgroundTask, BackgroundTaskKind};
    let mut thread = settled_source(false);
    Arc::make_mut(&mut thread).background = vec![
        BackgroundTask {
            id: "command".into(),
            kind: BackgroundTaskKind::Command,
            description: Some("npm run dev".into()),
        },
        BackgroundTask {
            id: "review".into(),
            kind: BackgroundTaskKind::Subagent,
            description: Some("Review the change".into()),
        },
    ];
    let raw = render(thread.clone(), None);
    let rows = project(raw.clone(), None);
    let footer = rows.session_rows();
    assert_eq!(footer.len(), 1);
    let ConversationRowContent::Background {
        title,
        description,
        waiting,
        turn_id,
    } = &footer[0].data.content
    else {
        panic!("the current background work belongs to the timeline footer");
    };
    assert_eq!(title, "サブエージェント 1件、コマンド 1件を待機中");
    assert_eq!(
        description.as_deref(),
        Some("Review the change、npm run dev")
    );
    assert!(*waiting);
    assert_eq!(turn_id.as_deref(), Some("turn"));
    assert_eq!(rows.rows.last().unwrap().id, footer[0].id);
    assert_eq!(rows.gaps().last(), Some(&8.));
    assert_eq!(
        rows.source_item_count(),
        1,
        "a status line is not a transcript item"
    );

    Arc::make_mut(&mut thread).background.remove(0);
    let next_raw = render(thread.clone(), Some(raw.clone()));
    let next = project(next_raw.clone(), Some(rows));
    let remaining = next.session_rows();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, footer[0].id);
    assert!(
        matches!(&remaining[0].data.content, ConversationRowContent::Background { title, description: None, .. }
        if title == "サブエージェント「Review the change」を待機中")
    );
    assert!(Arc::ptr_eq(&raw.turns[0], &next_raw.turns[0]));
    assert!(
        matches!(
            &footer[0].data.content,
            ConversationRowContent::Background {
                description: Some(_),
                ..
            }
        ),
        "the earlier immutable frame must retain its own background state"
    );

    let mut foreground = thread.clone();
    turn(&mut foreground).status = TurnStatus::Running;
    let active = project(
        render(foreground, Some(next_raw.clone())),
        Some(next.clone()),
    );
    assert!(
        active.session_rows().is_empty(),
        "foreground work replaces the background-only status"
    );

    Arc::make_mut(&mut thread).background.clear();
    let finished = project(render(thread, Some(next_raw)), Some(next));
    assert!(finished.session_rows().is_empty());
    assert_eq!(finished.source_item_count(), 1);
}

#[rstest::rstest]
fn provider_plan_documents_are_full_assistant_messages_without_special_rows(
    #[values(TurnStatus::Running, TurnStatus::Completed, TurnStatus::Interrupted)]
    status: TurnStatus,
) {
    let markdown =
        "# Full document\n\n## Summary\n\n".to_owned() + &"Required step.\n\n".repeat(25);
    let mut thread = source(false);
    turn(&mut thread).status = status;
    *items(&mut thread) = vec![Arc::new(
        serde_json::from_value(json!({
            "id":"native-plan", "body":{"inline":{"body":{"plan":{"text":markdown}}}}
        }))
        .unwrap(),
    )];
    let rows = project(render(thread, None), None);
    let row = rows.rows.iter().find(|row| matches!(&row.data.content,
        ConversationRowContent::Response { item, .. } if item.data.native_id.as_deref() == Some("native-plan")))
        .expect("the complete document must be visible as an ordinary assistant message");
    let ConversationRowContent::Response { item, .. } = &row.data.content else {
        unreachable!()
    };
    assert_eq!(item.data.kind, "agent");
    assert!(!item.data.collapsible);
    assert_eq!(item.message_body().as_deref(), Some(markdown.as_str()));
    assert!(
        crate::state::Snapshot::default()
            .activity_decorations(item.clone(), 0.0)
            .is_empty()
    );
    {
        assert!(matches!(
            row.timeline(),
            Some(TurnRow::Row(ConversationRow {
                content: ConversationRowContent::Response { .. },
                ..
            }))
        ));
    }
}

#[test]
fn native_timeline_keeps_live_work_visible_and_completed_work_opens_with_the_same_fold_choice() {
    let mut thread = source(false);
    let running = project(render(thread.clone(), None), None);
    assert!(
        running
            .rows
            .iter()
            .any(|row| matches!(row.timeline(), Some(TurnRow::Working))
                && row.progress_label(UiLanguage::English, 3.0).is_some())
    );
    assert!(
        running
            .rows
            .iter()
            .any(|row| matches!(row.timeline(), Some(TurnRow::Live { .. })))
    );
    turn(&mut thread).status = TurnStatus::Completed;
    for item in items(&mut thread) {
        Arc::make_mut(item).status = crate::models::ItemStatus::Completed;
    }
    let source = render(thread, None);
    let closed = project(source.clone(), None);
    let fold = closed
        .rows
        .iter()
        .find(|row| matches!(row.timeline(), Some(TurnRow::Fold { open: false, .. })))
        .unwrap();
    assert!(
        !closed
            .rows
            .iter()
            .any(|row| matches!(row.timeline(), Some(TurnRow::Work { .. })))
    );
    let expansion = closed
        .toggle_disclosure(fold.id(), HashMap::new())
        .unwrap()
        .expansion;
    let opened = ConversationRows::project(
        crate::presentation::conversation::TimelineStyle::NATIVE,
        source,
        scope(),
        None,
        expansion,
        Some(closed.clone()),
    );
    assert!(
        opened
            .rows
            .iter()
            .any(|row| matches!(row.timeline(), Some(TurnRow::Fold { open: true, .. })))
    );
    assert!(
        opened
            .rows
            .iter()
            .any(|row| matches!(row.timeline(), Some(TurnRow::Work { .. })))
    );
    assert_eq!(closed.source_item_count(), opened.source_item_count());
}

#[test]
fn live_work_opens_the_whole_current_group_and_closing_releases_its_members() {
    // T3 LiveWorkTimelineRow toggles groupedEntries, including the earlier work,
    // rather than just the detail of the last tool. Use the real scoped controls.
    let mut thread = source(false);
    let mut prior = items(&mut thread)[0].as_ref().clone();
    prior.id = "prior".into();
    prior.status = crate::models::ItemStatus::Completed;
    items(&mut thread).insert(0, Arc::new(prior));
    let raw = render(thread, None);
    let closed = project(raw.clone(), None);
    let live = closed
        .rows
        .iter()
        .find(|row| matches!(row.timeline(), Some(TurnRow::Live { .. })))
        .expect("the running command is the live header");
    let id = live.id();
    let change = closed
        .toggle_disclosure(id.clone(), HashMap::new())
        .expect("a live header must open all of its work");
    assert!(change.load.is_none());
    let choices = change.expansion;
    let opened = expanded(raw.clone(), choices.clone(), Some(closed.clone()));
    let members: Vec<_> = opened
        .rows
        .iter()
        .filter_map(|row| match row.timeline() {
            Some(TurnRow::Work { row, .. }) => match row.content {
                ConversationRowContent::Activity { item, .. } => item.data.native_id.clone(),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(members, ["prior", "command"].map(Into::into));
    let groups = opened.work_groups();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].end - groups[0].start, 3);
    let change = opened.toggle_disclosure(id.clone(), choices).unwrap();
    let reclosed = expanded(raw, change.expansion, Some(opened));
    assert!(
        reclosed
            .rows
            .iter()
            .all(|row| !matches!(row.timeline(), Some(TurnRow::Work { .. })))
    );
    assert_eq!(reclosed.rows.iter().filter(|row| row.id() == id).count(), 1);
    assert_eq!(reclosed.rows.len(), closed.rows.len());
}

#[test]
fn pending_approval_keeps_running_work_live_without_animating_completed_work() {
    // MessagesTimeline.logic.ts chooses inProgress work before a pending approval;
    // completed tools stay available in an ordinary group and there is no Thinking row.
    for status in [
        crate::models::ItemStatus::Running,
        crate::models::ItemStatus::Completed,
    ] {
        for delivery in [
            RequestDelivery::Awaiting,
            RequestDelivery::Sending,
            RequestDelivery::Sent,
            RequestDelivery::Unknown,
        ] {
            let mut thread = source(false);
            Arc::make_mut(&mut items(&mut thread)[0]).status = status;
            let request = Request {
                id: "approval".into(),
                target: RequestTarget::Turn {
                    turn_id: "turn".into(),
                    item_id: Some("command".into()),
                },
                delivery,
                body: super::super::approval_tests::body(false, "Allow this command?", "pwd"),
            };
            Arc::make_mut(&mut thread)
                .requests
                .insert(request.id.clone(), Arc::new(request));
            let projected = project(render(thread, None), None);
            let live = projected
                .rows
                .iter()
                .position(|row| matches!(row.timeline(), Some(TurnRow::Live { .. })));
            assert_eq!(
                live.is_some(),
                status == crate::models::ItemStatus::Running,
                "approval delivery {delivery:?}, tool status {status:?}"
            );
            assert!(
                projected
                    .rows
                    .iter()
                    .all(|row| !matches!(row.timeline(), Some(TurnRow::Thinking)))
            );
            let approval = projected
                .rows
                .iter()
                .position(|row| REQUEST(&row.data.content))
                .unwrap();
            if let Some(live) = live {
                assert!(live < approval);
                assert!(
                    projected
                        .toggle_disclosure(projected.rows[live].id(), HashMap::new())
                        .is_some()
                );
            } else {
                assert!(
                    projected.rows[..approval]
                        .iter()
                        .any(|row| matches!(row.timeline(), Some(TurnRow::Work { .. })))
                );
            }
        }
    }
}

#[test]
fn native_timeline_choices_are_scoped_and_rapid_group_clicks_keep_rows_independent() {
    let mut thread = source(false);
    turn(&mut thread).status = TurnStatus::Completed;
    let command = items(&mut thread)[0].clone();
    items(&mut thread).push(command);
    for item in items(&mut thread) {
        Arc::make_mut(item).status = crate::models::ItemStatus::Completed;
    }
    let repeated = thread.turns.as_ref().unwrap()[0].clone();
    Arc::make_mut(&mut thread)
        .turns
        .as_mut()
        .unwrap()
        .push(repeated);
    let raw = render(thread, None);
    let closed = project(raw.clone(), None);
    let folds: Vec<_> = closed
        .rows
        .iter()
        .filter(|row| matches!(row.timeline(), Some(TurnRow::Fold { .. })))
        .collect();
    assert_eq!(folds.len(), 2);
    let choices = closed
        .toggle_disclosure(folds[0].id(), HashMap::new())
        .unwrap()
        .expansion;
    let open = ConversationRows::project(
        crate::presentation::conversation::TimelineStyle::NATIVE,
        raw.clone(),
        scope(),
        None,
        choices.clone(),
        Some(closed.clone()),
    );
    let unchanged = open.rows.iter().find(|row| row.id == folds[1].id).unwrap();
    assert!(Arc::ptr_eq(folds[1], unchanged));
    assert!(!unchanged.expanded);
    let group = open
        .rows
        .iter()
        .find(|row| matches!(row.timeline(), Some(TurnRow::Group { .. })))
        .unwrap();
    let choices = open
        .toggle_disclosure(group.id(), choices)
        .unwrap()
        .expansion;
    let canceled = open
        .toggle_disclosure(group.id(), choices.clone())
        .unwrap()
        .expansion;
    assert!(!canceled.contains_key(&group.id()));
    let expanded = ConversationRows::project(
        crate::presentation::conversation::TimelineStyle::NATIVE,
        raw.clone(),
        scope(),
        None,
        choices.clone(),
        Some(open),
    );
    assert_eq!(
        expanded
            .rows
            .iter()
            .filter(|row| matches!(row.timeline(), Some(TurnRow::Work { .. })))
            .count(),
        2
    );
    let ids: std::collections::HashSet<_> = expanded.rows.iter().map(|row| row.id()).collect();
    assert_eq!(ids.len(), expanded.rows.len());
    let other_scope = ConversationRows::project(
        crate::presentation::conversation::TimelineStyle::NATIVE,
        raw,
        PanelScope {
            host: "other".into(),
            ..scope()
        },
        None,
        choices,
        Some(expanded),
    );
    assert!(!other_scope.rows.iter().any(|row| row.expanded));
}

#[test]
fn opening_deferred_attempt_returns_typed_load_and_rapid_close_does_not_read() {
    let thread = superseded_source(true);
    let expected_thread = thread.id.clone().unwrap();
    let expected_attempt = thread.turns.as_ref().unwrap()[0].attempts[0].id;
    let rows = project(render(thread, None), None);
    let group = rows
        .rows
        .iter()
        .find(|row| {
            matches!(
                row.timeline(),
                Some(TurnRow::Group {
                    attempt: Some(_),
                    ..
                })
            )
        })
        .unwrap();
    assert!(matches!(rows.activity_load(group.id()),
        Some(crate::state::Intent::ReadAttempt { thread_id, attempt })
            if thread_id == expected_thread && attempt == expected_attempt));
    let foreign = ConversationRows::project(
        crate::presentation::conversation::TimelineStyle::NATIVE,
        rows.source(),
        PanelScope {
            host: "foreign".into(),
            ..scope()
        },
        None,
        HashMap::new(),
        None,
    );
    assert!(foreign.activity_load(group.id()).is_none());
    assert!(rows.activity_load("missing-control".into()).is_none());
    let open = rows.toggle_disclosure(group.id(), HashMap::new()).unwrap();
    assert!(
        matches!(open.load, Some(crate::state::Intent::ReadAttempt { thread_id, attempt }) if thread_id == expected_thread && attempt == expected_attempt)
    );
    assert_eq!(
        open.expansion.get(&group.id()).map(String::as_str),
        Some("open")
    );
    let close = rows.toggle_disclosure(group.id(), open.expansion).unwrap();
    assert!(close.expansion.is_empty());
    assert!(close.load.is_none());
    assert!(!group.expanded);
}

#[test]
fn hydrated_attempts_do_not_request_load_and_retired_groups_reject_stale_clicks() {
    let mut thread = superseded_source(false);
    let rows = project(render(thread.clone(), None), None);
    let group = rows
        .rows
        .iter()
        .find(|row| {
            matches!(
                row.timeline(),
                Some(TurnRow::Group {
                    attempt: Some(_),
                    ..
                })
            )
        })
        .unwrap();
    let change = rows.toggle_disclosure(group.id(), HashMap::new()).unwrap();
    assert!(change.load.is_none());
    assert!(rows.activity_load(group.id()).is_none());
    turn(&mut thread).attempts.clear();
    let fresh = project(render(thread, None), Some(rows.clone()));
    assert!(fresh.activity_load(group.id()).is_none());
    assert!(
        fresh
            .toggle_disclosure(group.id(), change.expansion)
            .is_none()
    );
}

fn superseded_source(deferred: bool) -> Arc<Thread> {
    use agent_protocol::models::{AttemptState, ExecutionAttempt};
    let mut thread = source(false);
    let id = uuid::Uuid::from_u128(1);
    turn(&mut thread).attempts.push(ExecutionAttempt {
        id,
        state: AttemptState::Superseded { by_steer: true },
    });
    let item = Arc::make_mut(&mut items(&mut thread)[0]);
    item.attempt = Some(id);
    item.status = crate::models::ItemStatus::Interrupted;
    if deferred {
        item.defer();
    }
    thread
}

#[test]
fn native_timeline_keeps_each_pending_request_row_identifiable() {
    let mut thread = source(false);
    for id in ["first", "second"] {
        Arc::make_mut(&mut thread).requests.insert(
            id.into(),
            awaiting(
                id,
                RequestTarget::Turn {
                    turn_id: "turn".into(),
                    item_id: None,
                },
            ),
        );
    }
    let rows = project(render(thread, None), None);
    let requests: Vec<_> = rows
        .rows
        .iter()
        .filter(|row| matches!(row.timeline(), Some(TurnRow::Requested { .. })))
        .collect();
    assert_eq!(requests.len(), 2);
    assert_ne!(requests[0].id, requests[1].id);
}

#[test]
fn approvals_stay_in_the_timeline_across_turn_status_delivery_and_session_placement() {
    for status in [
        TurnStatus::Running,
        TurnStatus::Completed,
        TurnStatus::Failed,
        TurnStatus::Interrupted,
    ] {
        for delivery in [
            RequestDelivery::Awaiting,
            RequestDelivery::Sending,
            RequestDelivery::Sent,
            RequestDelivery::Unknown,
        ] {
            for target in [
                RequestTarget::Session,
                RequestTarget::Turn {
                    turn_id: "turn".into(),
                    item_id: None,
                },
            ] {
                for permission in [false, true] {
                    let mut thread = settled_source(false);
                    turn(&mut thread).status = status;
                    let request = Request {
                        id: "approval".into(),
                        target: target.clone(),
                        delivery,
                        body: super::super::approval_tests::body(
                            permission,
                            "reason",
                            "echo fixture",
                        ),
                    };
                    Arc::make_mut(&mut thread)
                        .requests
                        .insert(request.id.clone(), Arc::new(request));
                    assert!(super::super::docked_request(&thread, false).is_none());
                    let projected = project(render(thread, None), None);
                    assert_eq!(
                        rows(&projected, REQUEST).count(),
                        1,
                        "approval cannot disappear inside completed work or a composer panel"
                    );
                    let row = first(&projected, REQUEST);
                    assert!(!row.expanded);
                    assert_eq!(
                        projected.gaps()[projected
                            .rows
                            .iter()
                            .position(|value| value.id == row.id)
                            .unwrap()],
                        8.
                    );
                }
            }
        }
    }
}

#[test]
fn approval_keeps_preceding_tools_and_is_outside_their_bounded_group() {
    let mut thread = settled_source(false);
    turn(&mut thread).status = TurnStatus::Running;
    let command = items(&mut thread)[0].clone();
    items(&mut thread).push(command);
    let request = Request {
        id: "approval".into(),
        target: RequestTarget::Turn {
            turn_id: "turn".into(),
            item_id: None,
        },
        delivery: RequestDelivery::Awaiting,
        body: super::super::approval_tests::body(false, "reason", "echo fixture"),
    };
    Arc::make_mut(&mut thread)
        .requests
        .insert(request.id.clone(), Arc::new(request));
    let raw = render(thread, None);
    let closed = project(raw.clone(), None);
    let group = closed
        .rows
        .iter()
        .find(|row| matches!(row.timeline(), Some(TurnRow::Group { .. })))
        .expect("the tools before an approval must stay available");
    let choices = closed
        .toggle_disclosure(group.id(), HashMap::new())
        .unwrap()
        .expansion;
    let opened = expanded(raw, choices, Some(closed));
    assert_eq!(rows(&opened, ACTIVITY).count(), 2);
    let approval = opened
        .rows
        .iter()
        .position(|row| REQUEST(&row.data.content))
        .unwrap();
    let groups = opened.work_groups();
    assert_eq!(groups.len(), 1);
    assert_eq!(
        groups[0].end, approval as u32,
        "approval must not be clipped or scrolled inside tool output"
    );
}

#[test]
fn native_timeline_action_footer_has_its_own_identity_and_keeps_the_response_after_later_work() {
    let mut thread = source(true);
    turn(&mut thread).status = TurnStatus::Failed;
    items(&mut thread).rotate_right(1);
    for item in items(&mut thread) {
        Arc::make_mut(item).status = crate::models::ItemStatus::Completed;
    }
    let source = render(thread, None);
    let closed = project(source.clone(), None);
    let fold = closed
        .rows
        .iter()
        .find(|row| matches!(row.timeline(), Some(TurnRow::Fold { .. })))
        .unwrap();
    let expansion = closed
        .toggle_disclosure(fold.id(), HashMap::new())
        .unwrap()
        .expansion;
    let rows = ConversationRows::project(
        crate::presentation::conversation::TimelineStyle::NATIVE,
        source,
        scope(),
        None,
        expansion,
        Some(closed),
    )
    .rows();
    let ids: std::collections::HashSet<_> = rows.iter().map(|row| row.id()).collect();
    assert_eq!(ids.len(), rows.len());
    let answer = rows
        .iter()
        .position(|row| {
            matches!(
                row.timeline(),
                Some(TurnRow::Row(ConversationRow {
                    content: ConversationRowContent::Response {
                        show_actions: false,
                        ..
                    },
                    ..
                }))
            )
        })
        .unwrap();
    let work = rows
        .iter()
        .position(|row| matches!(row.timeline(), Some(TurnRow::Work { .. })))
        .unwrap();
    let actions = rows
        .iter()
        .position(|row| matches!(row.timeline(), Some(TurnRow::Actions(_))))
        .unwrap();
    assert!(answer < work && work < actions);
}

#[test]
fn worktree_progress_stays_after_the_initial_user_when_native_activity_arrives() {
    let mut thread = source(true);
    items(&mut thread).insert(0, Arc::new(serde_json::from_value(json!({
        "id":"user", "status":"completed", "body":{"inline":{"body":{"userMessage":{"text":"first input", "content":[]}}}}
    })).unwrap()));
    Arc::make_mut(&mut thread).workspace_setup = Some(
        agent_protocol::workspace::WorkspaceSetup::new(Some("origin/main".into()), 1000),
    );
    let projection = ConversationRows::project(
        crate::presentation::conversation::TimelineStyle::NATIVE,
        render(thread, None),
        scope(),
        None,
        HashMap::new(),
        None,
    );
    let rows = projection.rows();
    assert!(matches!(
        rows[0].data.content,
        ConversationRowContent::User { .. }
    ));
    assert!(matches!(
        rows[1].data.content,
        ConversationRowContent::Preparation { .. }
    ));
    assert!(
        rows[2..]
            .iter()
            .any(|row| matches!(row.data.content, ConversationRowContent::InProgress { .. }))
    );
    assert_eq!(
        projection
            .turn_rows()
            .iter()
            .filter(|row| matches!(row.data.content, ConversationRowContent::Preparation { .. }))
            .count(),
        1
    );
    assert!(projection.queued_rows().is_empty());
    assert!(projection.session_rows().is_empty());
}

fn scope() -> PanelScope {
    PanelScope {
        host: "host".into(),
        storage: "account".into(),
        draft: DraftKey::from("draft"),
    }
}

/// A running turn with a running `pwd` command and, optionally, an answer after it.
fn source(answer: bool) -> Arc<Thread> {
    let mut items = vec![
        json!({"id":"command", "status":"running", "body":{"inline":{"body":{
            "commandExecution":{"command":"pwd", "cwd":null, "output":"first output", "exitCode":null}
        }}}}),
    ];
    if answer {
        items.push(json!({"id":"answer", "body":{"inline":{"body":{
            "assistantText":{"text":"answer", "phase":"unknown"}
        }}}}));
    }
    Arc::new(
        serde_json::from_value(json!({
            "id":{"provider":"codex", "id":"thread"}, "capabilities":{"fork":true},
            "turns":[{"id":"turn", "status":"running", "startedAtMs":1000, "items":items}]
        }))
        .unwrap(),
    )
}

fn settled_source(answer: bool) -> Arc<Thread> {
    let mut thread = source(answer);
    turn(&mut thread).status = TurnStatus::Completed;
    for item in items(&mut thread) {
        Arc::make_mut(item).status = crate::models::ItemStatus::Completed;
    }
    thread
}

fn turn(thread: &mut Arc<Thread>) -> &mut Turn {
    Arc::make_mut(&mut Arc::make_mut(thread).turns.as_mut().unwrap()[0])
}

fn items(thread: &mut Arc<Thread>) -> &mut Vec<Arc<Item>> {
    turn(thread).items.as_mut().unwrap()
}

fn awaiting(id: &str, target: RequestTarget) -> Arc<Request> {
    Arc::new(Request {
        id: id.into(),
        target,
        delivery: RequestDelivery::Awaiting,
        body: RequestBody::ToolExecution {
            tool: "tool".into(),
            namespace: None,
            arguments: json!({}),
        },
    })
}

fn render(
    source: Arc<Thread>,
    previous: Option<Arc<RenderedConversation>>,
) -> Arc<RenderedConversation> {
    project_conversation(
        UiLanguage::Japanese,
        &scope().draft,
        &Arc::default(),
        source,
        &previous,
    )
}

/// Rows with every activity collapsed.
fn project(
    source: Arc<RenderedConversation>,
    previous: Option<Arc<ConversationRows>>,
) -> Arc<ConversationRows> {
    ConversationRows::project(
        crate::presentation::conversation::TimelineStyle::NATIVE,
        source,
        scope(),
        None,
        HashMap::new(),
        previous,
    )
}

/// Timeline rows with the chosen activities open.
fn expanded(
    source: Arc<RenderedConversation>,
    expansion: HashMap<String, String>,
    previous: Option<Arc<ConversationRows>>,
) -> Arc<ConversationRows> {
    ConversationRows::project(
        crate::presentation::conversation::TimelineStyle::NATIVE,
        source,
        scope(),
        None,
        expansion,
        previous,
    )
}

type Kind = fn(&ConversationRowContent) -> bool;
const HEADER: Kind = |content| matches!(content, ConversationRowContent::ActivityHeader { .. });
const ACTIVITY: Kind = |content| matches!(content, ConversationRowContent::Activity { .. });
const PROGRESS: Kind = |content| matches!(content, ConversationRowContent::InProgress { .. });
const REQUEST: Kind = |content| matches!(content, ConversationRowContent::PendingRequest { .. });
const RESPONSE: Kind = |content| matches!(content, ConversationRowContent::Response { .. });
const USER: Kind = |content| matches!(content, ConversationRowContent::User { .. });

fn rows(rows: &ConversationRows, kind: Kind) -> impl Iterator<Item = &Arc<VisibleConversationRow>> {
    rows.rows.iter().filter(move |row| kind(&row.data.content))
}

fn first(conversation: &ConversationRows, kind: Kind) -> &Arc<VisibleConversationRow> {
    rows(conversation, kind).next().unwrap()
}

fn header(conversation: &ConversationRows) -> &Arc<VisibleConversationRow> {
    first(conversation, HEADER)
}

/// The progress label three seconds into the fixture's turn, which started at one second.
fn progress_label(row: &VisibleConversationRow) -> String {
    row.progress
        .as_ref()
        .unwrap()
        .label(UiLanguage::Japanese, 3.0)
}

fn expand(row: &VisibleConversationRow) -> HashMap<String, String> {
    let ConversationRowContent::ActivityHeader { activity } = &row.data.content else {
        panic!("activity header")
    };
    [(row.id.clone(), activity.status.clone())].into()
}

#[test]
fn turn_input_is_named_in_the_timeline_and_session_input_stays_docked_without_losing_source_counts()
{
    let mut thread = source(false);
    for request in [
        awaiting(
            "turn-request",
            RequestTarget::Turn {
                turn_id: "turn".into(),
                item_id: None,
            },
        ),
        awaiting("session-request", RequestTarget::Session),
    ] {
        Arc::make_mut(&mut thread)
            .requests
            .insert(request.id.clone(), request);
    }
    let raw = render(thread, None);
    let timeline = project(raw, None);
    assert_eq!(rows(&timeline, REQUEST).count(), 0);
    assert_eq!(rows(&timeline, PROGRESS).count(), 1);
    assert_eq!(progress_label(first(&timeline, PROGRESS)), "2秒 作業中…");
    let requests: Vec<_> = timeline
        .rows
        .iter()
        .filter_map(|row| match row.timeline() {
            Some(TurnRow::Requested { label, .. }) => Some(label),
            _ => None,
        })
        .collect();
    assert_eq!(requests, ["入力を求めています"]);
    assert!(timeline.session_rows().is_empty());
    assert_eq!(
        timeline.source_item_count(),
        3,
        "two requests plus the hidden command"
    );
}

#[test]
fn completion_refreshes_actions_without_replacing_the_body_and_stable_rows_reuse_arcs() {
    let mut thread = source(true);
    // A response alone keeps its last-row position across completion, isolating the actions.
    items(&mut thread).remove(0);
    let raw = render(thread.clone(), None);
    let live = project(raw.clone(), None);
    let repeated = project(raw.clone(), Some(live.clone()));
    assert!(Arc::ptr_eq(&live, &repeated));
    let before = first(&live, RESPONSE);
    assert!(
        !before.last,
        "the live thinking row follows the streaming answer"
    );
    assert!(matches!(
        live.rows.last().unwrap().timeline(),
        Some(TurnRow::Thinking)
    ));
    let ConversationRowContent::Response {
        item: before_item,
        show_actions: false,
        ..
    } = &before.data.content
    else {
        panic!("live response")
    };
    turn(&mut thread).status = TurnStatus::Completed;
    let done = project(render(thread.clone(), Some(raw)), Some(live.clone()));
    let after = first(&done, RESPONSE);
    let ConversationRowContent::Response {
        item: after_item,
        show_actions: true,
        fork_turn_id: Some(id),
    } = &after.data.content
    else {
        panic!("completed response actions")
    };
    assert_eq!(id.as_str(), "turn");
    assert!(Arc::ptr_eq(before_item, after_item));
    assert!(after.last);
    assert!(!Arc::ptr_eq(before, after));
    assert!(
        before != after,
        "native equality must observe changed action flags"
    );
    let repeated = project(done.source.clone(), Some(done.clone()));
    assert!(Arc::ptr_eq(&done, &repeated));
    Arc::make_mut(&mut thread)
        .capabilities
        .as_mut()
        .unwrap()
        .fork = false;
    let no_fork = project(
        render(thread, Some(done.source.clone())),
        Some(done.clone()),
    );
    let response = first(&no_fork, RESPONSE);
    let ConversationRowContent::Response {
        item,
        show_actions: true,
        fork_turn_id: None,
    } = &response.data.content
    else {
        panic!("completed copy action without fork support")
    };
    assert!(Arc::ptr_eq(after_item, item));
    assert!(
        after != response,
        "removing fork support must update the row even with copy still available"
    );
}

#[test]
fn repeated_turn_and_item_ids_have_independent_expansion_and_reuse() {
    let mut thread = settled_source(true);
    let repeated = thread.turns.as_ref().unwrap()[0].clone();
    Arc::make_mut(&mut thread)
        .turns
        .as_mut()
        .unwrap()
        .push(repeated);
    let raw = render(thread, None);
    assert_ne!(raw.turns[0].key(), raw.turns[1].key());
    let collapsed = project(raw.clone(), None);
    let headers: Vec<_> = rows(&collapsed, HEADER).collect();
    assert_eq!(headers.len(), 2);
    assert_ne!(headers[0].id, headers[1].id);
    let opened = expanded(raw, expand(headers[0]), Some(collapsed.clone()));
    assert_eq!(rows(&opened, ACTIVITY).count(), 1);
    let second = opened
        .rows
        .iter()
        .find(|row| row.id == headers[1].id)
        .unwrap();
    assert!(!second.expanded);
    assert!(
        headers[1].as_ref() == second.as_ref(),
        "native wrappers share the same row identity"
    );
    for before in collapsed
        .rows
        .iter()
        .filter(|row| row.origin == headers[1].origin)
    {
        let after = opened.rows.iter().find(|row| row.id == before.id).unwrap();
        assert!(
            Arc::ptr_eq(before, after),
            "unaffected rows retain their render values"
        );
    }
    let unique: std::collections::HashSet<_> = opened.rows.iter().map(|row| &row.id).collect();
    assert_eq!(unique.len(), opened.rows.len());
}

#[test]
fn scopes_and_panes_do_not_share_expansion_choices_or_mutate_previous_results() {
    let raw = render(settled_source(false), None);
    let collapsed = project(raw.clone(), None);
    let choices = expand(header(&collapsed));
    let opened = expanded(raw.clone(), choices.clone(), Some(collapsed.clone()));
    assert!(header(&opened).expanded);
    assert!(!header(&collapsed).expanded);
    let other_pane = project(raw.clone(), Some(opened.clone()));
    assert!(!header(&other_pane).expanded);
    assert!(header(&opened).expanded);
    for key in [
        PanelScope {
            host: "other".into(),
            ..scope()
        },
        PanelScope {
            storage: "other".into(),
            ..scope()
        },
        PanelScope {
            draft: DraftKey::from("other"),
            ..scope()
        },
    ] {
        let other = ConversationRows::project(
            crate::presentation::conversation::TimelineStyle::NATIVE,
            raw.clone(),
            key,
            None,
            choices.clone(),
            Some(opened.clone()),
        );
        assert!(!header(&other).expanded);
        assert_ne!(header(&opened).id, header(&other).id);
    }
}

#[test]
fn collapsed_body_updates_are_visible_on_reopen_and_resuming_retires_the_fold() {
    let mut thread = settled_source(false);
    let raw = render(thread.clone(), None);
    let collapsed = project(raw.clone(), None);
    let choices = expand(header(&collapsed));
    Arc::make_mut(&mut items(&mut thread)[0]).body =
        serde_json::from_value(json!({"inline":{"body":{"commandExecution":{
            "command":"pwd", "cwd":null, "output":"latest hidden output", "exitCode":null
        }}}}))
        .unwrap();
    let updated_raw = render(thread.clone(), Some(raw));
    let updated = project(updated_raw.clone(), Some(collapsed.clone()));
    assert!(
        Arc::ptr_eq(header(&collapsed), header(&updated)),
        "unchanged summary and progress stay cached"
    );
    let reopened = expanded(updated_raw.clone(), choices.clone(), Some(updated));
    let ConversationRowContent::Activity { item: activity, .. } =
        &first(&reopened, ACTIVITY).data.content
    else {
        panic!("activity")
    };
    assert!(activity.expanded_body().contains("latest hidden output"));
    let old_fold = header(&reopened).id.clone();
    turn(&mut thread).status = TurnStatus::Running;
    Arc::make_mut(&mut items(&mut thread)[0]).status = crate::models::ItemStatus::Running;
    let resumed = expanded(render(thread, Some(updated_raw)), choices, Some(reopened));
    assert!(rows(&resumed, HEADER).next().is_none());
    assert_eq!(rows(&resumed, PROGRESS).count(), 1);
    assert!(
        resumed
            .toggle_disclosure(old_fold, resumed.expansion.clone())
            .is_none()
    );
    let ConversationRowContent::Activity { item, .. } = &first(&resumed, ACTIVITY).data.content
    else {
        panic!("live work must remain visible after resuming")
    };
    assert!(item.expanded_body().contains("latest hidden output"));
}

#[test]
fn timestamps_last_row_and_progress_are_part_of_render_identity() {
    let mut thread = source(false);
    items(&mut thread).insert(
        0,
        Arc::new(serde_json::from_value(json!({
            "id":"user", "body":{"inline":{"body":{"userMessage":{"text":"question", "content":[]}}}}
        })).unwrap()),
    );
    let raw = render(thread.clone(), None);
    let initial = project(raw.clone(), None);
    let before = first(&initial, USER).clone();
    assert_eq!(before.data.timestamp_ms, Some(1000));
    let initial_work = first(&initial, ACTIVITY);
    assert!(initial_work.last);

    let mut timed = thread.clone();
    turn(&mut timed).started_at_ms = Some(2000);
    let retimed = project(render(timed, Some(raw.clone())), Some(initial.clone()));
    let after = first(&retimed, USER).clone();
    assert_eq!(after.data.timestamp_ms, Some(2000));
    let (
        ConversationRowContent::User { item: old_item },
        ConversationRowContent::User { item: new_item },
    ) = (&before.data.content, &after.data.content)
    else {
        panic!("user rows")
    };
    assert!(Arc::ptr_eq(old_item, new_item), "only row timing changed");
    assert!(before != after);
    assert_eq!(progress_label(first(&retimed, PROGRESS)), "1秒 作業中…");
    assert!(!Arc::ptr_eq(
        first(&initial, PROGRESS),
        first(&retimed, PROGRESS)
    ));

    let mut answered = thread.clone();
    let answer = items(&mut source(true))[1].clone();
    items(&mut answered).push(answer);
    let moved = project(render(answered, Some(raw.clone())), Some(initial.clone()));
    let moved_work = first(&moved, ACTIVITY);
    assert!(!moved_work.last);
    assert!(
        !Arc::ptr_eq(initial_work, moved_work),
        "a new final response changes the preceding row padding"
    );

    turn(&mut thread).started_at = Some(7.0);
    let legacy_timed = project(render(thread, Some(raw)), Some(initial));
    let fallback = first(&legacy_timed, USER).clone();
    assert_eq!(fallback.data.timestamp_ms, before.data.timestamp_ms);
    assert!(
        before != fallback,
        "native timestamp fallbacks cannot retain stale turn timing"
    );
}

#[test]
fn toggling_returns_choices_without_mutating_the_frame_or_accepting_stale_ids() {
    let mut thread = source(true);
    let completed = turn(&mut thread);
    completed.status = TurnStatus::Completed;
    let raw = render(thread, None);
    let collapsed = project(raw.clone(), None);
    let id = header(&collapsed).id.clone();
    let change = collapsed
        .toggle_disclosure(id.clone(), HashMap::new())
        .unwrap()
        .expansion;
    assert!(collapsed.expansion.is_empty());
    assert!(!header(&collapsed).expanded);
    let opened = expanded(raw.clone(), change, Some(collapsed.clone()));
    assert!(header(&opened).expanded);
    let close = opened
        .toggle_disclosure(id.clone(), opened.expansion.clone())
        .unwrap()
        .expansion;
    assert!(!close.contains_key(&id));
    assert!(
        opened
            .toggle_disclosure(
                first(&opened, RESPONSE).id.clone(),
                opened.expansion.clone()
            )
            .is_none()
    );
    assert!(
        opened
            .toggle_disclosure("missing".into(), HashMap::new())
            .is_none()
    );
    let other = ConversationRows::project(
        crate::presentation::conversation::TimelineStyle::NATIVE,
        raw,
        PanelScope {
            host: "new host".into(),
            ..scope()
        },
        None,
        HashMap::new(),
        Some(opened),
    );
    assert!(
        other.toggle_disclosure(id, HashMap::new()).is_none(),
        "retired host/window actions cannot load into another scope"
    );
}

#[test]
fn queued_inputs_keep_their_footer_between_turns_and_session_rows_without_inflating_source_counts()
{
    use crate::state::{Draft, PendingSubmission};
    let mut thread = source(false);
    Arc::make_mut(&mut thread).requests.insert(
        "session".into(),
        awaiting("session", RequestTarget::Session),
    );
    let mut pending: std::collections::BTreeMap<_, _> = [(
        "pending".into(),
        Arc::new(PendingSubmission {
            new_draft_slot: None,
            cursor: None,
            sequence: 0,
            submitted_at_ms: Some(12_000),
            draft_key: thread.id.as_ref().unwrap().into(),
            draft: Arc::new(Draft {
                text: "unsent footer".into(),
                ..Default::default()
            }),
            turn_id: Some("unloaded".into()),
            after_item_id: None,
            accepted: false,
            delivery_unknown: true,
            queued: false,
        }),
    )]
    .into_iter()
    .collect();
    let with_pending = |pending: &std::collections::BTreeMap<_, _>, previous| {
        project_conversation(
            UiLanguage::Japanese,
            &scope().draft,
            pending,
            thread.clone(),
            &previous,
        )
    };
    let raw = with_pending(&pending, None);
    let rows = project(raw.clone(), None);
    let queue = rows.queued_rows();
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].data.timestamp_ms, Some(12_000));
    assert!(queue[0].last);
    let ConversationRowContent::User { item } = &queue[0].data.content else {
        panic!("queued input")
    };
    assert_eq!(item.data.body.as_deref(), Some("unsent footer"));
    assert_eq!(item.unknown_submission_id().as_deref(), Some("pending"));
    assert_eq!(
        rows.source_item_count(),
        2,
        "the source command and session request exclude queued local input"
    );
    let grouped: Vec<_> = rows
        .turn_rows()
        .into_iter()
        .chain(rows.queued_rows())
        .chain(rows.session_rows())
        .map(|row| row.id.clone())
        .collect();
    assert_eq!(
        grouped,
        rows.rows
            .iter()
            .map(|row| row.id.clone())
            .collect::<Vec<_>>()
    );
    Arc::make_mut(pending.get_mut("pending").unwrap()).submitted_at_ms = Some(13_000);
    let updated = project(with_pending(&pending, Some(raw)), Some(rows));
    assert_eq!(updated.queued_rows()[0].data.timestamp_ms, Some(13_000));
    assert!(!Arc::ptr_eq(&queue[0], &updated.queued_rows()[0]));
}

#[test]
fn scope_components_are_unambiguous_even_when_they_contain_key_delimiters() {
    let raw = render(settled_source(false), None);
    let before_scope = PanelScope {
        host: "a:b".into(),
        storage: "c".into(),
        draft: "draft:[]".into(),
    };
    let first = ConversationRows::project(
        crate::presentation::conversation::TimelineStyle::NATIVE,
        raw.clone(),
        before_scope,
        None,
        HashMap::new(),
        None,
    );
    let choices = expand(header(&first));
    let other_scope = PanelScope {
        host: "a".into(),
        storage: "b:c".into(),
        draft: "draft:[]".into(),
    };
    let other = ConversationRows::project(
        crate::presentation::conversation::TimelineStyle::NATIVE,
        raw,
        other_scope,
        None,
        choices,
        Some(first.clone()),
    );
    assert_ne!(header(&first).id, header(&other).id);
    assert!(!header(&other).expanded);
}

#[test]
fn rapid_activity_clicks_use_the_owners_pending_choices() {
    let mut thread = source(true);
    let completed = turn(&mut thread);
    completed.status = TurnStatus::Completed;
    let raw = render(thread, None);
    let collapsed = project(raw, None);
    let id = header(&collapsed).id.clone();
    let first = collapsed
        .toggle_disclosure(id.clone(), HashMap::new())
        .unwrap()
        .expansion;
    let second = collapsed
        .toggle_disclosure(id.clone(), first)
        .unwrap()
        .expansion;
    assert!(
        second.is_empty(),
        "a second click closes before the frame is published"
    );
    let third = collapsed
        .toggle_disclosure(id.clone(), [(id.clone(), "running".into())].into())
        .unwrap()
        .expansion;
    assert_eq!(
        third[&id], "completed",
        "a stale running choice does not expand completed work"
    );
}

#[test]
fn long_collapsed_activity_does_not_retain_hidden_native_render_values() {
    let mut thread = settled_source(true);
    let command = items(&mut thread)[0].clone();
    for index in 1..2000 {
        let mut next = command.as_ref().clone();
        next.id = format!("command-{index}").into();
        items(&mut thread).insert(index, Arc::new(next));
    }
    let raw = render(thread, None);
    let hidden: Vec<_> = raw.turns[0]
        .rows
        .iter()
        .filter_map(|row| match &row.content {
            ConversationRowContent::Activity { item, .. } => Some(item),
            _ => None,
        })
        .collect();
    assert_eq!(hidden.len(), 2000);
    let references: Vec<_> = hidden.iter().map(|item| Arc::strong_count(item)).collect();
    let collapsed = project(raw.clone(), None);
    assert_eq!(collapsed.source_item_count(), 2001);
    assert_eq!(
        collapsed.rows.len(),
        2,
        "only the activity header and response are native rows"
    );
    // Retained references measure hidden render allocation deterministically,
    // without a device-dependent wall-clock performance threshold.
    assert_eq!(
        hidden
            .iter()
            .map(|item| Arc::strong_count(item))
            .collect::<Vec<_>>(),
        references
    );
    let fold_open = expanded(raw.clone(), expand(header(&collapsed)), Some(collapsed));
    let group = fold_open
        .rows
        .iter()
        .find(|row| matches!(row.timeline(), Some(TurnRow::Group { .. })))
        .unwrap();
    let choices = fold_open
        .toggle_disclosure(group.id(), fold_open.expansion.clone())
        .unwrap()
        .expansion;
    let opened = expanded(raw.clone(), choices.clone(), Some(fold_open.clone()));
    assert_eq!(rows(&opened, ACTIVITY).count(), 2000);
    let closed = project(raw.clone(), Some(opened.clone()));
    assert_eq!(closed.rows.len(), 2);
    drop(opened);
    drop(fold_open);
    assert_eq!(
        hidden
            .iter()
            .map(|item| Arc::strong_count(item))
            .collect::<Vec<_>>(),
        references,
        "closing releases native activity rows instead of caching hidden content"
    );
    let reopened = expanded(raw.clone(), choices, Some(closed));
    assert_eq!(rows(&reopened, ACTIVITY).count(), 2000);
}

#[test]
fn presentation_styles_keep_terminal_work_contracts_and_invalidate_the_frame_cache() {
    let mut thread = settled_source(false);
    turn(&mut thread).status = TurnStatus::Interrupted;
    let raw = render(thread, None);
    let native = ConversationRows::project(
        super::super::TimelineStyle::NATIVE,
        raw.clone(),
        scope(),
        None,
        HashMap::new(),
        None,
    );
    assert!(
        native
            .rows
            .iter()
            .any(|row| matches!(row.timeline(), Some(TurnRow::Fold { open: false, .. })))
    );
    let t3 = ConversationRows::project(
        super::super::TimelineStyle::T3,
        raw,
        scope(),
        None,
        HashMap::new(),
        Some(native.clone()),
    );
    assert!(!Arc::ptr_eq(&native, &t3));
    assert!(
        !t3.rows
            .iter()
            .any(|row| matches!(row.timeline(), Some(TurnRow::Fold { .. })))
    );
    assert!(t3.rows.iter().any(|row| matches!(
        row.timeline(),
        Some(TurnRow::Work { .. } | TurnRow::Group { .. })
    )));
}

#[test]
fn native_terminal_disclosure_does_not_force_plans_into_plain_messages() {
    use super::super::{PlanPresentation, TimelineStyle};
    let mut thread = settled_source(false);
    turn(&mut thread).status = TurnStatus::Interrupted;
    items(&mut thread).push(Arc::new(serde_json::from_value(json!({
        "id":"plan", "body":{"inline":{"body":{"plan":{"text":"# Proposed change\n\nKeep the full document."}}}}
    })).unwrap()));
    let raw = render(thread, None);
    let cards = ConversationRows::project(
        TimelineStyle {
            plans: PlanPresentation::Card,
            background_footer: false,
            ..TimelineStyle::NATIVE
        },
        raw.clone(),
        scope(),
        None,
        HashMap::new(),
        None,
    );
    assert!(
        cards
            .rows
            .iter()
            .any(|row| matches!(row.timeline(), Some(TurnRow::Fold { open: false, .. })))
    );
    assert!(cards.rows.iter().any(|row| matches!(row.timeline(), Some(TurnRow::Plan { text, .. }) if text.contains("Keep the full document."))));
    let messages = ConversationRows::project(
        TimelineStyle::NATIVE,
        raw,
        scope(),
        None,
        HashMap::new(),
        Some(cards),
    );
    assert!(
        !messages
            .rows
            .iter()
            .any(|row| matches!(row.timeline(), Some(TurnRow::Plan { .. })))
    );
    assert!(messages.rows.iter().any(|row| matches!(&row.data.content, ConversationRowContent::Response { item, .. } if item.data.native_id.as_deref() == Some("plan") && item.message_body().is_some_and(|text| text.contains("Keep the full document.")))));
}

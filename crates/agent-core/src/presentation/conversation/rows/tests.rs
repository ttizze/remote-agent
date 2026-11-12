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
fn worktree_progress_stays_after_the_initial_user_when_native_activity_arrives() {
    let mut thread = source(true);
    items(&mut thread).insert(0, Arc::new(serde_json::from_value(json!({
        "id":"user", "status":"completed", "body":{"inline":{"body":{"userMessage":{"text":"first input", "content":[]}}}}
    })).unwrap()));
    Arc::make_mut(&mut thread).workspace_setup = Some(
        agent_protocol::workspace::WorkspaceSetup::new(Some("origin/main".into()), 1000),
    );
    let projection = ConversationRows::project(
        render(thread, None),
        scope(),
        HashMap::new(),
        ConversationRowPolicy {
            requests: true,
            progress: ProgressRows::Always,
        },
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
    assert!(rows[2..].iter().any(|row| matches!(
        row.data.content,
        ConversationRowContent::ActivityHeader { .. }
    )));
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
    policy: ConversationRowPolicy,
    previous: Option<Arc<ConversationRows>>,
) -> Arc<ConversationRows> {
    ConversationRows::project(source, scope(), HashMap::new(), policy, previous)
}

/// Timeline rows with the chosen activities open.
fn expanded(
    source: Arc<RenderedConversation>,
    expansion: HashMap<String, String>,
    previous: Option<Arc<ConversationRows>>,
) -> Arc<ConversationRows> {
    ConversationRows::project(source, scope(), expansion, TIMELINE, previous)
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

const TIMELINE: ConversationRowPolicy = ConversationRowPolicy {
    requests: false,
    progress: ProgressRows::UnlessHeader,
};
const INLINE: ConversationRowPolicy = ConversationRowPolicy {
    requests: true,
    progress: ProgressRows::Always,
};
const DOCKED: ConversationRowPolicy = ConversationRowPolicy {
    requests: false,
    progress: ProgressRows::Hidden,
};

#[test]
fn request_and_progress_placement_is_explicit_and_source_counts_ignore_visibility() {
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
    let inline = project(raw.clone(), INLINE, None);
    let docked = project(raw.clone(), DOCKED, None);
    let timeline = project(raw.clone(), TIMELINE, None);
    assert_eq!(progress_label(header(&timeline)), "2秒 作業中 · pwd");
    assert_eq!(rows(&inline, REQUEST).count(), 2);
    assert_eq!(rows(&docked, REQUEST).count(), 0);
    assert_eq!(rows(&timeline, REQUEST).count(), 0);
    assert_eq!(rows(&inline, PROGRESS).count(), 1);
    assert_eq!(rows(&docked, PROGRESS).count(), 0);
    assert_eq!(
        rows(&timeline, PROGRESS).count(),
        0,
        "hidden activity and requests leave the working header adjacent"
    );
    assert_eq!(
        inline.source_item_count(),
        3,
        "two requests plus the hidden command"
    );
    assert_eq!(docked.source_item_count(), inline.source_item_count());
    assert_eq!(timeline.source_item_count(), inline.source_item_count());
    let opened = expanded(raw, expand(header(&timeline)), Some(timeline.clone()));
    assert_eq!(
        rows(&opened, PROGRESS).count(),
        1,
        "a displayed activity separates the working header from progress"
    );
    assert_eq!(opened.source_item_count(), timeline.source_item_count());
    assert_eq!(progress_label(header(&opened)), "2秒 作業中…");
    assert_eq!(progress_label(first(&opened, PROGRESS)), "2秒 作業中…");
    assert!(rows(&opened, ACTIVITY).next().is_some());
    assert!(rows(&timeline, ACTIVITY).next().is_none());
}

#[test]
fn completion_refreshes_actions_without_replacing_the_body_and_stable_rows_reuse_arcs() {
    let mut thread = source(true);
    // A response alone keeps its last-row position across completion, isolating the actions.
    items(&mut thread).remove(0);
    let raw = render(thread.clone(), None);
    let live = project(raw.clone(), DOCKED, None);
    let repeated = project(raw.clone(), DOCKED, Some(live.clone()));
    assert!(Arc::ptr_eq(&live, &repeated));
    let before = first(&live, RESPONSE);
    assert!(before.last);
    let ConversationRowContent::Response {
        item: before_item,
        show_actions: false,
        ..
    } = &before.data.content
    else {
        panic!("live response")
    };
    turn(&mut thread).status = TurnStatus::Completed;
    let done = project(
        render(thread.clone(), Some(raw)),
        DOCKED,
        Some(live.clone()),
    );
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
    let repeated = project(done.source.clone(), DOCKED, Some(done.clone()));
    assert!(Arc::ptr_eq(&done, &repeated));
    Arc::make_mut(&mut thread)
        .capabilities
        .as_mut()
        .unwrap()
        .fork = false;
    let no_fork = project(
        render(thread, Some(done.source.clone())),
        DOCKED,
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
    let mut thread = source(true);
    let repeated = thread.turns.as_ref().unwrap()[0].clone();
    Arc::make_mut(&mut thread)
        .turns
        .as_mut()
        .unwrap()
        .push(repeated);
    let raw = render(thread, None);
    assert_ne!(raw.turns[0].key(), raw.turns[1].key());
    let collapsed = project(raw.clone(), TIMELINE, None);
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
    let raw = render(source(false), None);
    let collapsed = project(raw.clone(), TIMELINE, None);
    let choices = expand(header(&collapsed));
    let opened = expanded(raw.clone(), choices.clone(), Some(collapsed.clone()));
    assert!(header(&opened).expanded);
    assert!(!header(&collapsed).expanded);
    let other_pane = project(raw.clone(), TIMELINE, Some(opened.clone()));
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
            raw.clone(),
            key,
            choices.clone(),
            TIMELINE,
            Some(opened.clone()),
        );
        assert!(!header(&other).expanded);
        assert_ne!(header(&opened).id, header(&other).id);
    }
}

#[test]
fn collapsed_body_updates_are_visible_on_reopen_and_status_changes_close_activity() {
    let mut thread = source(false);
    let raw = render(thread.clone(), None);
    let collapsed = project(raw.clone(), TIMELINE, None);
    let choices = expand(header(&collapsed));
    Arc::make_mut(&mut items(&mut thread)[0]).body =
        serde_json::from_value(json!({"inline":{"body":{"commandExecution":{
            "command":"pwd", "cwd":null, "output":"latest hidden output", "exitCode":null
        }}}}))
        .unwrap();
    let updated_raw = render(thread.clone(), Some(raw));
    let updated = project(updated_raw.clone(), TIMELINE, Some(collapsed.clone()));
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
    turn(&mut thread).status = TurnStatus::Completed;
    let completed = expanded(render(thread, Some(updated_raw)), choices, Some(reopened));
    assert!(!header(&completed).expanded);
    assert!(rows(&completed, ACTIVITY).next().is_none());
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
    let initial = project(raw.clone(), TIMELINE, None);
    let before = first(&initial, USER).clone();
    assert_eq!(before.data.timestamp_ms, Some(1000));
    assert!(header(&initial).last);

    let mut timed = thread.clone();
    turn(&mut timed).started_at_ms = Some(2000);
    let retimed = project(
        render(timed, Some(raw.clone())),
        TIMELINE,
        Some(initial.clone()),
    );
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
    assert_eq!(progress_label(header(&retimed)), "1秒 作業中 · pwd");
    assert!(!Arc::ptr_eq(header(&initial), header(&retimed)));

    let mut answered = thread.clone();
    let answer = items(&mut source(true))[1].clone();
    items(&mut answered).push(answer);
    let moved = project(
        render(answered, Some(raw.clone())),
        TIMELINE,
        Some(initial.clone()),
    );
    assert!(!header(&moved).last);
    assert!(
        !Arc::ptr_eq(header(&initial), header(&moved)),
        "a new final response changes the preceding row padding"
    );

    turn(&mut thread).started_at = Some(7.0);
    let legacy_timed = project(render(thread, Some(raw)), TIMELINE, Some(initial));
    let fallback = first(&legacy_timed, USER).clone();
    assert_eq!(fallback.data.timestamp_ms, before.data.timestamp_ms);
    assert!(
        before != fallback,
        "native timestamp fallbacks cannot retain stale turn timing"
    );
}

#[test]
fn toggling_returns_choices_and_loads_without_mutating_the_frame_or_accepting_stale_ids() {
    let mut thread = source(true);
    let completed = turn(&mut thread);
    completed.items.as_mut().unwrap().remove(0);
    completed.items_summary = true;
    completed.status = TurnStatus::Completed;
    let raw = render(thread, None);
    let collapsed = project(raw.clone(), TIMELINE, None);
    let id = header(&collapsed).id.clone();
    let change = collapsed
        .toggle_activity(id.clone(), HashMap::new())
        .unwrap();
    let load = change.load_items.unwrap();
    assert_eq!(load.thread_id.id, "thread");
    assert_eq!(load.turn_id.as_str(), "turn");
    assert!(collapsed.expansion.is_empty());
    assert!(!header(&collapsed).expanded);
    let opened = expanded(raw.clone(), change.expansion, Some(collapsed.clone()));
    assert!(header(&opened).expanded);
    let close = opened
        .toggle_activity(id.clone(), opened.expansion.clone())
        .unwrap();
    assert!(!close.expansion.contains_key(&id));
    assert!(close.load_items.is_none(), "closing never loads details");
    assert!(
        opened
            .toggle_activity(
                first(&opened, RESPONSE).id.clone(),
                opened.expansion.clone()
            )
            .is_none()
    );
    assert!(
        opened
            .toggle_activity("missing".into(), HashMap::new())
            .is_none()
    );
    let other = ConversationRows::project(
        raw,
        PanelScope {
            host: "new host".into(),
            ..scope()
        },
        HashMap::new(),
        TIMELINE,
        Some(opened),
    );
    assert!(
        other.toggle_activity(id, HashMap::new()).is_none(),
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
    let rows = project(raw.clone(), INLINE, None);
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
    let updated = project(with_pending(&pending, Some(raw)), INLINE, Some(rows));
    assert_eq!(updated.queued_rows()[0].data.timestamp_ms, Some(13_000));
    assert!(!Arc::ptr_eq(&queue[0], &updated.queued_rows()[0]));
}

#[test]
fn scope_components_are_unambiguous_even_when_they_contain_key_delimiters() {
    let raw = render(source(false), None);
    let before_scope = PanelScope {
        host: "a:b".into(),
        storage: "c".into(),
        draft: "draft:[]".into(),
    };
    let first =
        ConversationRows::project(raw.clone(), before_scope, HashMap::new(), TIMELINE, None);
    let choices = expand(header(&first));
    let other_scope = PanelScope {
        host: "a".into(),
        storage: "b:c".into(),
        draft: "draft:[]".into(),
    };
    let other = ConversationRows::project(raw, other_scope, choices, TIMELINE, Some(first.clone()));
    assert_ne!(header(&first).id, header(&other).id);
    assert!(!header(&other).expanded);
}

#[test]
fn rapid_activity_clicks_use_the_owners_pending_choices() {
    let mut thread = source(true);
    let completed = turn(&mut thread);
    completed.items_summary = true;
    completed.status = TurnStatus::Completed;
    let raw = render(thread, None);
    let collapsed = project(raw, DOCKED, None);
    let id = header(&collapsed).id.clone();
    let first = collapsed
        .toggle_activity(id.clone(), HashMap::new())
        .unwrap();
    assert!(first.load_items.is_some());
    let second = collapsed
        .toggle_activity(id.clone(), first.expansion)
        .unwrap();
    assert!(
        second.expansion.is_empty(),
        "a second click closes before the frame is published"
    );
    assert!(
        second.load_items.is_none(),
        "closing must not start another history read"
    );
    let third = collapsed
        .toggle_activity(id.clone(), [(id, "running".into())].into())
        .unwrap();
    assert!(
        third.load_items.is_some(),
        "a stale running choice does not expand completed work"
    );
}

#[test]
fn long_collapsed_activity_does_not_retain_hidden_native_render_values() {
    let mut thread = source(true);
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
    let collapsed = project(raw.clone(), DOCKED, None);
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
    let opened = expanded(raw.clone(), expand(header(&collapsed)), Some(collapsed));
    assert_eq!(rows(&opened, ACTIVITY).count(), 2000);
    let closed = project(raw.clone(), DOCKED, Some(opened.clone()));
    assert_eq!(closed.rows.len(), 2);
    drop(opened);
    assert_eq!(
        hidden
            .iter()
            .map(|item| Arc::strong_count(item))
            .collect::<Vec<_>>(),
        references,
        "closing releases native activity rows instead of caching hidden content"
    );
    let reopened = expanded(raw.clone(), expand(header(&closed)), Some(closed));
    assert_eq!(rows(&reopened, ACTIVITY).count(), 2000);
}

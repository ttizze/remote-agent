//! The same reducer runs on the Host and every native client.
use crate::contracts::*;

#[derive(Debug, Clone, Copy, Default)]
pub struct ProjectionOptions {
    pub partial_timeline: bool,
    pub latest_local_turn_ordinal: Option<u64>,
}

pub fn apply(
    projection: Option<&ThreadProjection>,
    event: &DomainEvent,
    options: ProjectionOptions,
) -> Option<ThreadProjection> {
    let mut next = match projection {
        Some(value) if value.thread.id != event.thread_id => return Some(value.clone()),
        Some(value) => value.clone(),
        None => match &event.payload {
            EventPayload::ThreadCreated(thread) if thread.id == event.thread_id => {
                return Some(ThreadProjection::empty(thread.clone()));
            }
            _ => return None,
        },
    };
    if let EventPayload::TurnItemUpdated(item) = &event.payload
        && options.partial_timeline
        && !next.turn_items.iter().any(|old| old.id == item.id)
        && (options
            .latest_local_turn_ordinal
            .is_some_and(|ordinal| item.ordinal <= ordinal)
            || next
                .visible_turn_items
                .iter()
                .filter(|row| row.visibility == Visibility::Local)
                .map(|row| row.item.ordinal)
                .min()
                .is_some_and(|oldest| item.ordinal < oldest))
    {
        return Some(next);
    }
    if !matches!(
        event.payload,
        EventPayload::ThreadVisited(_) | EventPayload::ThreadMarkedUnread(_)
    ) {
        next.updated_at = event.occurred_at.clone();
    }
    macro_rules! upsert {
        ($field:ident, $value:expr) => {{
            let value = $value;
            if let Some(old) = next.$field.iter_mut().find(|old| old.id == value.id) {
                *old = value.clone();
            } else {
                next.$field.push(value.clone());
            }
        }};
    }
    use EventPayload::*;
    match &event.payload {
        ThreadCreated(value)
        | ThreadArchived(value)
        | ThreadUnarchived(value)
        | ThreadDeleted(value)
        | ThreadSettled(value)
        | ThreadUnsettled(value)
        | ThreadSnoozed(value)
        | ThreadUnsnoozed(value)
        | ThreadPinned(value)
        | ThreadUnpinned(value)
        | ThreadAutoSettleSet(value)
        | ThreadPinReordered(value)
        | ThreadActiveReordered(value)
        | ThreadVisited(value)
        | ThreadMarkedUnread(value)
        | ThreadMetadataUpdated(value)
        | ThreadRuntimeModeUpdated(value)
        | ThreadInteractionModeUpdated(value)
        | ThreadModelSelectionUpdated(value)
        | ThreadProviderSwitched(value) => next.thread = value.clone(),
        RunCreated(value) | RunUpdated(value) => upsert!(runs, value),
        RunAttemptCreated(value) | RunAttemptUpdated(value) => upsert!(attempts, value),
        NodeUpdated(value) => upsert!(nodes, value),
        SubagentUpdated(value) => upsert!(subagents, value),
        ProviderSessionAttached(value) | ProviderSessionUpdated(value) => {
            upsert!(provider_sessions, value)
        }
        ProviderSessionDetached(id) => next.provider_sessions.retain(|value| value.id != *id),
        ProviderThreadUpdated(value) => {
            if let Some(thread) = updated_thread_for_provider(&next.thread, value) {
                next.thread = thread;
            }
            upsert!(provider_threads, value);
        }
        ProviderTurnUpdated(value) => {
            let value = updated_provider_turn(
                next.provider_turns.iter().find(|old| old.id == value.id),
                value,
            );
            upsert!(provider_turns, &value);
        }
        RuntimeRequestUpdated(value) => upsert!(runtime_requests, value),
        MessageUpdated(value) => upsert!(messages, value),
        PlanUpdated(value) => upsert!(plans, value),
        TurnItemUpdated(value) => upsert!(turn_items, value),
        TurnItemTextDelta(delta) => {
            if let Some(item) = next
                .turn_items
                .iter_mut()
                .find(|item| item.id == delta.item_id && item.run_id == delta.run_id)
                && append_text_delta(
                    item,
                    &mut next.messages,
                    &mut next.plans,
                    delta,
                    &event.occurred_at,
                )
                && let Some(row) = next.visible_turn_items.iter_mut().find(|row| {
                    row.visibility == Visibility::Local && row.source_item_id == item.id
                })
            {
                row.item = item.clone();
            }
        }
        CheckpointScopeCreated(value) => upsert!(checkpoint_scopes, value),
        CheckpointCaptured(value) => upsert!(checkpoints, value),
        CheckpointRollbackRequested(_) => {}
        ContextHandoffUpdated(value) => upsert!(context_handoffs, value),
        ContextTransferCreated(value) | ContextTransferUpdated(value) => {
            upsert!(context_transfers, value)
        }
    }
    if matches!(
        event.payload,
        RunCreated(_)
            | RunUpdated(_)
            | RunAttemptCreated(_)
            | RunAttemptUpdated(_)
            | TurnItemUpdated(_)
    ) {
        next.visible_turn_items = visible_items(&next).into();
    }
    Some(next)
}

/// Offsets make replayed chunks harmless and reject gaps without corrupting Unicode.
pub fn append_text_delta(
    item: &mut TurnItem,
    messages: &mut [ConversationMessage],
    plans: &mut [PlanArtifact],
    delta: &TurnItemTextDelta,
    now: &Timestamp,
) -> bool {
    if item.status != ItemStatus::Running || item.run_id != delta.run_id {
        return false;
    }
    let plan_id = match &item.body {
        TurnItemBody::ProposedPlan { plan_id, .. } => Some(plan_id.clone()),
        _ => None,
    };
    let (text, message_id) = match &mut item.body {
        TurnItemBody::AssistantMessage {
            text,
            message_id,
            streaming: true,
            ..
        } => (text, Some(message_id)),
        TurnItemBody::Reasoning {
            text,
            streaming: true,
        } => (text, None),
        TurnItemBody::ProposedPlan {
            markdown,
            streaming: true,
            ..
        } => (markdown, None),
        TurnItemBody::CommandExecution {
            output: Some(output),
            ..
        } => (output, None),
        _ => return false,
    };
    if text.len() != delta.offset {
        return false;
    }
    text.push_str(&delta.text);
    item.updated_at = now.clone();
    if let Some(id) = message_id
        && let Some(message) = messages.iter_mut().find(|message| &message.id == id)
        && message.text.len() == delta.offset
    {
        message.text.push_str(&delta.text);
        message.updated_at = now.clone();
    }
    if let Some(id) = plan_id
        && let Some(plan) = plans.iter_mut().find(|plan| plan.id == id)
        && let PlanBody::ProposedPlan { markdown } = &mut plan.body
        && markdown.len() == delta.offset
    {
        markdown.push_str(&delta.text);
    }
    true
}

/// Finalize the visible item and its streaming body together.
pub fn finished_item(item: &TurnItem, status: ItemStatus, now: &Timestamp) -> TurnItem {
    let mut item = item.clone();
    item.status = status;
    item.completed_at = Some(now.clone());
    item.updated_at = now.clone();
    match &mut item.body {
        TurnItemBody::AssistantMessage { streaming, .. }
        | TurnItemBody::Reasoning { streaming, .. }
        | TurnItemBody::ProposedPlan { streaming, .. } => *streaming = false,
        _ => {}
    }
    item
}

/// A queued placeholder does not become the active native conversation before delivery.
pub fn updated_thread_for_provider(
    thread: &AppThread,
    provider: &ProviderThread,
) -> Option<AppThread> {
    let placeholder = provider.status == ProviderThreadStatus::NotLoaded
        && provider.first_run_ordinal.is_none()
        && provider.native_thread_ref.is_none()
        && provider.provider_session_id.is_none();
    if provider.app_thread_id.as_ref() != Some(&thread.id)
        || provider.status == ProviderThreadStatus::Closed
        || placeholder
    {
        return None;
    }
    let mut thread = thread.clone();
    thread.active_provider_thread_id = Some(provider.id.clone());
    thread.updated_at = provider.updated_at.clone();
    Some(thread)
}

pub fn updated_provider_turn(
    previous: Option<&ProviderTurn>,
    updated: &ProviderTurn,
) -> ProviderTurn {
    let mut value = updated.clone();
    value.token_usage = updated
        .token_usage
        .as_ref()
        .or_else(|| previous.and_then(|turn| turn.token_usage.as_ref()))
        .cloned();
    value
}

pub fn is_visible(
    item: &TurnItem,
    runs: &[Run],
    attempts: &[RunAttempt],
    items: &[TurnItem],
) -> bool {
    let status = item
        .run_id
        .as_ref()
        .and_then(|id| runs.iter().find(|run| run.id == *id))
        .map(|run| run.status);
    if status == Some(RunStatus::RolledBack) {
        return false;
    }
    if status == Some(RunStatus::Cancelled)
        && matches!(
            item.body,
            TurnItemBody::UserMessage {
                input_intent: InputIntent::QueuedTurn,
                ..
            }
        )
    {
        return false;
    }
    if matches!(item.body, TurnItemBody::RunInterruptResult { .. })
        && let (Some(run_id), Some(node_id)) = (&item.run_id, &item.node_id)
        && attempts.iter().any(|attempt| {
            attempt.run_id == *run_id
                && attempt.root_node_id == *node_id
                && attempt.status == AttemptStatus::Superseded
        })
        && !items.iter().any(|candidate| {
            candidate.run_id.as_ref() == Some(run_id)
                && matches!(candidate.body, TurnItemBody::RunInterruptRequest { .. })
        })
    {
        return false;
    }
    true
}

pub fn visible_items(projection: &ThreadProjection) -> Vec<ProjectedTurnItem> {
    let mut rows: Vec<_> = projection
        .visible_turn_items
        .iter()
        .filter(|row| row.visibility != Visibility::Local)
        .cloned()
        .collect();
    let mut local: Vec<_> = projection
        .turn_items
        .iter()
        .filter(|item| {
            is_visible(
                item,
                &projection.runs,
                &projection.attempts,
                &projection.turn_items,
            )
        })
        .cloned()
        .collect();
    local.sort_by(|a, b| a.ordinal.cmp(&b.ordinal).then_with(|| a.id.cmp(&b.id)));
    rows.extend(local.into_iter().map(|item| ProjectedTurnItem {
        position: 0,
        visibility: Visibility::Local,
        source_thread_id: item.thread_id.clone(),
        source_item_id: item.id.clone(),
        item,
    }));
    for (position, row) in rows.iter_mut().enumerate() {
        row.position = position as u64;
    }
    rows
}

pub fn shell(projection: &ThreadProjection) -> ThreadShell {
    let latest = projection.runs.iter().max_by_key(|run| run.ordinal);
    let active = projection
        .runs
        .iter()
        .filter(|run| run.status.is_blocking())
        .max_by_key(|run| run.ordinal);
    let latest_message = projection
        .messages
        .iter()
        .filter(|message| matches!(message.role, Role::User | Role::Assistant))
        .filter(|message| {
            message.run_id.as_ref().is_none_or(|id| {
                !projection.runs.iter().any(|run| {
                    run.id == *id
                        && matches!(
                            run.status,
                            RunStatus::Queued | RunStatus::Cancelled | RunStatus::RolledBack
                        )
                })
            })
        })
        .max_by(|a, b| {
            a.updated_at
                .cmp(&b.updated_at)
                .then_with(|| a.id.cmp(&b.id))
        });
    let mut history = vec![];
    for run in &projection.runs {
        if !history.contains(&run.provider_instance_id) {
            history.push(run.provider_instance_id.clone());
        }
    }
    if !history.contains(&projection.thread.provider_instance_id) {
        history.push(projection.thread.provider_instance_id.clone());
    }
    ThreadShell {
        thread: projection.thread.clone(),
        latest_run_id: latest.map(|run| run.id.clone()),
        active_run_id: active.map(|run| run.id.clone()),
        status: active.or(latest).map(|run| run.status),
        pending_runtime_request: projection
            .runtime_requests
            .iter()
            .filter(|request| request.status == RequestStatus::Pending)
            .min_by(|a, b| a.created_at.cmp(&b.created_at))
            .map(|request| PendingRuntimeRequest {
                id: request.id.clone(),
                kind: request.kind,
                created_at: request.created_at.clone(),
            }),
        latest_visible_message: latest_message.map(|message| VisibleMessage {
            id: message.id.clone(),
            role: message.role,
            text: message.text.chars().take(400).collect(),
            updated_at: message.updated_at.clone(),
        }),
        latest_user_message_at: projection
            .messages
            .iter()
            .filter(|message| message.role == Role::User)
            .map(|message| &message.created_at)
            .max()
            .cloned(),
        latest_run_requested_at: latest.map(|run| run.requested_at.clone()),
        latest_run_started_at: latest.and_then(|run| run.started_at.clone()),
        latest_run_completed_at: projection
            .runs
            .iter()
            .filter_map(|run| run.completed_at.as_ref())
            .max()
            .cloned(),
        active_run_started_at: active.and_then(|run| run.started_at.clone()),
        has_actionable_proposed_plan: projection.plans.iter().any(|plan| {
            matches!(plan.status, PlanStatus::Draft | PlanStatus::Active)
                && matches!(plan.body, PlanBody::ProposedPlan { .. })
        }),
        pending_background_tasks: projection
            .provider_threads
            .iter()
            .flat_map(|thread| thread.pending_background_tasks.clone())
            .collect(),
        provider_instance_history: history,
        item_count: projection.turn_items.len() as u64,
        visible_item_count: projection.visible_turn_items.len() as u64,
        last_error: projection.visible_turn_items.iter().rev().find_map(|row| {
            match &row.item.body {
                TurnItemBody::Error { failure, .. } => Some(failure.clone()),
                _ => None,
            }
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use proptest::prelude::*;
    #[test]
    fn streamed_offsets_are_idempotent_and_share_unchanged_projection_collections() {
        let (mut p, _) = crate::test_support::apply(
            &projection(),
            &send("start", DispatchMode::StartImmediately),
        );
        let run = p.runs[0].id.clone();
        let mut item = p.turn_items[0].clone();
        item.id = TurnItemId::new("stream").unwrap();
        item.ordinal = 100;
        item.body = TurnItemBody::Reasoning {
            text: "日本".into(),
            streaming: true,
        };
        item.run_id = Some(run.clone());
        item.status = ItemStatus::Running;
        let event = DomainEvent {
            id: EventId::new("initial-stream").unwrap(),
            thread_id: p.thread.id.clone(),
            occurred_at: now(),
            payload: EventPayload::TurnItemUpdated(item.clone()),
        };
        p = super::apply(Some(&p), &event, Default::default()).unwrap();
        let delta = DomainEvent {
            id: EventId::new("delta").unwrap(),
            payload: EventPayload::TurnItemTextDelta(TurnItemTextDelta {
                item_id: item.id.clone(),
                run_id: Some(run),
                offset: "日本".len(),
                text: "語".into(),
            }),
            ..event
        };
        let next = super::apply(
            Some(&p),
            &delta,
            ProjectionOptions {
                partial_timeline: true,
                latest_local_turn_ordinal: Some(100),
            },
        )
        .unwrap();
        assert!(p.runs.shares_storage(&next.runs));
        assert!(p.nodes.shares_storage(&next.nodes));
        assert!(p.checkpoints.shares_storage(&next.checkpoints));
        let replay = super::apply(Some(&next), &delta, Default::default()).unwrap();
        assert_eq!(next, replay);
        assert!(
            matches!(&next.turn_items.iter().find(|i| i.id == item.id).unwrap().body, TurnItemBody::Reasoning { text, .. } if text == "日本語")
        );
        assert!(
            matches!(&next.visible_turn_items.iter().find(|i| i.item.id == item.id).unwrap().item.body, TurnItemBody::Reasoning { text, .. } if text == "日本語")
        );
        assert!(
            matches!(&p.turn_items.iter().find(|i| i.id == item.id).unwrap().body, TurnItemBody::Reasoning { text, .. } if text == "日本")
        );
    }
    #[test]
    fn plan_and_command_deltas_preserve_detail_and_ignore_replays() {
        let (p, _) = crate::test_support::apply(
            &projection(),
            &send("start", DispatchMode::StartImmediately),
        );
        let plan_id = PlanId::new("plan").unwrap();
        for body in [
            TurnItemBody::ProposedPlan {
                plan_id: plan_id.clone(),
                markdown: "日".into(),
                streaming: true,
            },
            TurnItemBody::CommandExecution {
                input: "test".into(),
                output: Some("日".into()),
                output_omitted: false,
                output_indicates_failure: false,
                exit_code: None,
            },
        ] {
            let mut item = p.turn_items[0].clone();
            item.status = ItemStatus::Running;
            item.body = body;
            let mut plans = vec![PlanArtifact {
                id: plan_id.clone(),
                thread_id: p.thread.id.clone(),
                run_id: item.run_id.clone(),
                node_id: item.node_id.clone().unwrap(),
                status: PlanStatus::Draft,
                detail_in_turn_item: true,
                body: PlanBody::ProposedPlan {
                    markdown: "日".into(),
                },
            }];
            let delta = TurnItemTextDelta {
                item_id: item.id.clone(),
                run_id: item.run_id.clone(),
                offset: "日".len(),
                text: "本語".into(),
            };
            assert!(append_text_delta(
                &mut item,
                &mut [],
                &mut plans,
                &delta,
                &now()
            ));
            assert!(!append_text_delta(
                &mut item,
                &mut [],
                &mut plans,
                &delta,
                &now()
            ));
            match &item.body {
                TurnItemBody::ProposedPlan { markdown, .. } => {
                    assert_eq!(markdown, "日本語");
                    assert!(
                        matches!(&plans[0].body, PlanBody::ProposedPlan { markdown } if markdown == "日本語")
                    );
                }
                TurnItemBody::CommandExecution { output, .. } => {
                    assert_eq!(output.as_deref(), Some("日本語"))
                }
                _ => unreachable!(),
            }
        }
    }
    #[test]
    fn native_delivery_activates_provider_thread_but_queued_placeholder_does_not() {
        let (projection, _) = crate::test_support::apply(
            &projection(),
            &send("start", DispatchMode::StartImmediately),
        );
        let mut provider = projection.provider_threads[0].clone();
        assert!(updated_thread_for_provider(&projection.thread, &provider).is_none());
        provider.native_thread_ref = Some(ProviderRef {
            driver: Driver::Codex,
            native_id: Some("native".into()),
            strength: Strength::Strong,
            fingerprint: None,
            ordinal: None,
        });
        let event = DomainEvent {
            id: EventId::new("native-delivery").unwrap(),
            thread_id: projection.thread.id.clone(),
            occurred_at: now(),
            payload: EventPayload::ProviderThreadUpdated(provider.clone()),
        };
        let next = super::apply(Some(&projection), &event, Default::default()).unwrap();
        assert_eq!(next.thread.active_provider_thread_id, Some(provider.id));
    }
    #[test]
    fn visit_updates_read_state_without_changing_activity() {
        let projection = projection();
        let event = DomainEvent {
            id: EventId::new("visit-event").unwrap(),
            thread_id: projection.thread.id.clone(),
            occurred_at: Timestamp::parse("2026-10-06T00:00:00Z").unwrap(),
            payload: EventPayload::ThreadVisited(projection.thread.clone()),
        };
        let next = super::apply(Some(&projection), &event, ProjectionOptions::default()).unwrap();
        assert_eq!(next.updated_at, projection.updated_at);
    }
    #[test]
    fn bounded_timeline_ignores_old_missing_live_updates() {
        let (projection, _) = crate::test_support::apply(
            &projection(),
            &send("start", DispatchMode::StartImmediately),
        );
        let mut bounded = projection.clone();
        bounded.turn_items.clear();
        bounded.visible_turn_items.clear();
        let item = projection.turn_items[0].clone();
        let event = DomainEvent {
            id: EventId::new("update").unwrap(),
            thread_id: bounded.thread.id.clone(),
            occurred_at: now(),
            payload: EventPayload::TurnItemUpdated(item.clone()),
        };
        let next = super::apply(
            Some(&bounded),
            &event,
            ProjectionOptions {
                partial_timeline: true,
                latest_local_turn_ordinal: Some(item.ordinal),
            },
        )
        .unwrap();
        assert_eq!(next, bounded);
    }
    #[test]
    fn provider_turn_updates_retain_usage_when_omitted() {
        let mut projection = running();
        projection.provider_turns[0].token_usage = Some(TokenUsage {
            used_tokens: 10,
            max_tokens: None,
            input_tokens: None,
            cached_input_tokens: None,
            output_tokens: None,
            reasoning_output_tokens: None,
            updated_at: now(),
        });
        let mut turn = projection.provider_turns[0].clone();
        turn.token_usage = None;
        let event = DomainEvent {
            id: EventId::new("turn-event").unwrap(),
            thread_id: projection.thread.id.clone(),
            occurred_at: now(),
            payload: EventPayload::ProviderTurnUpdated(turn),
        };
        assert_eq!(
            super::apply(Some(&projection), &event, ProjectionOptions::default())
                .unwrap()
                .provider_turns[0]
                .token_usage,
            projection.provider_turns[0].token_usage
        );
    }
    proptest! {
        #[test]
        fn upserts_are_idempotent_sorted_and_unique(ordinals in proptest::collection::vec(0u64..100, 1..50)) {
            let (mut projection, _) = crate::test_support::apply(&projection(), &send("start", DispatchMode::StartImmediately));
            let template = projection.turn_items[0].clone();
            for (i, ordinal) in ordinals.into_iter().enumerate() {
                let mut item = template.clone(); item.id = TurnItemId::new(format!("item-{i}")).unwrap(); item.ordinal = ordinal;
                let event = DomainEvent { id: EventId::new(format!("event-{i}")).unwrap(), thread_id: projection.thread.id.clone(), occurred_at: now(), payload: EventPayload::TurnItemUpdated(item) };
                let next = super::apply(Some(&projection), &event, ProjectionOptions::default()).unwrap();
                prop_assert_eq!(super::apply(Some(&next), &event, ProjectionOptions::default()).unwrap(), next.clone()); projection = next;
            }
            prop_assert!(projection.visible_turn_items.windows(2).all(|rows| (rows[0].item.ordinal, &rows[0].item.id) <= (rows[1].item.ordinal, &rows[1].item.id)));
            for (index, row) in projection.visible_turn_items.iter().enumerate() {prop_assert_eq!(row.position, index as u64);}
        }
    }
}

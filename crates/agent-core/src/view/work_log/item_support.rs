//! The entities around one item: its run, attempts, request, checkpoint, task
//! and transfer.
use agent_domain::{
    Attempt, Checkpoint, Item, ItemKind, Request, Run, State, Task, Transfer, TransferKind,
    TurnItemId,
};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ItemSupport {
    pub item: Option<Item>,
    pub run: Option<Run>,
    pub attempts: Vec<Attempt>,
    /// The approval or question the item asks.
    pub request: Option<Request>,
    /// The checkpoint of the item's run.
    pub checkpoint: Option<Checkpoint>,
    /// The delegated task a subagent item stands for.
    pub task: Option<Task>,
    /// The context a fork item or a subagent's result brought into the thread.
    pub transfer: Option<Transfer>,
}

/// The latest transfer that matches, preferring one that is not superseded.
fn latest_transfer(state: &State, matches: impl Fn(&Transfer) -> bool) -> Option<Transfer> {
    state
        .transfers
        .iter()
        .filter(|transfer| matches(transfer))
        .max_by_key(|transfer| !transfer.superseded)
        .cloned()
}

/// Resolves the entities that enrich one item. The caller passes the item's
/// source-thread state, including for inherited items.
pub fn resolve_item_support(state: &State, item_id: &TurnItemId) -> ItemSupport {
    let Some(item) = state
        .items
        .iter()
        .chain(&state.inherited_items)
        .find(|candidate| &candidate.id == item_id)
    else {
        return ItemSupport::default();
    };

    let run = item
        .run
        .as_ref()
        .and_then(|id| state.runs.iter().find(|candidate| &candidate.id == id));
    let attempts = item.run.as_ref().map_or_else(Vec::new, |id| {
        state
            .attempts
            .iter()
            .filter(|candidate| &candidate.run == id)
            .cloned()
            .collect()
    });
    let request = match &item.kind {
        ItemKind::ApprovalRequest { request } | ItemKind::UserInputRequest { request } => state
            .requests
            .iter()
            .find(|candidate| &candidate.id == request)
            .cloned(),
        _ => None,
    };
    let checkpoint = run.and_then(|run| run.checkpoint.as_ref()).and_then(|id| {
        state
            .checkpoints
            .iter()
            .find(|candidate| &candidate.id == id)
            .cloned()
    });
    let task = match &item.kind {
        ItemKind::Subagent { task } => state
            .tasks
            .iter()
            .find(|candidate| &candidate.id == task)
            .cloned(),
        _ => None,
    };
    let transfer = match &item.kind {
        ItemKind::Fork { parent, boundary } => latest_transfer(state, |transfer| {
            transfer.kind == TransferKind::Fork
                && &transfer.source == parent
                && transfer.boundary == *boundary
        }),
        ItemKind::Subagent { .. } => task.as_ref().and_then(|task| {
            latest_transfer(state, |transfer| {
                transfer.kind == TransferKind::SubagentResult
                    && transfer.source == task.child_thread
            })
        }),
        _ => None,
    };

    ItemSupport {
        item: Some(item.clone()),
        run: run.cloned(),
        attempts,
        request,
        checkpoint,
        task,
        transfer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_domain::{
        AttemptStatus, CheckpointId, CheckpointStatus, CompletionWake, ContextTransferId,
        DeliveryState, Driver, HistoricalContext, ItemStatus, MessageId, ModelSelection, NodeId,
        RequestBody, RequestStatus, ResponseCapability, RunAttemptId, RunId, RunStatus,
        RuntimeRequestId, ThreadId, Timestamp,
    };

    fn now() -> Timestamp {
        Timestamp::parse("2026-06-20T00:00:00.000Z").unwrap()
    }
    fn run_id() -> RunId {
        RunId::new("run-1").unwrap()
    }
    fn item_id() -> TurnItemId {
        TurnItemId::new("item-1").unwrap()
    }

    fn item(kind: ItemKind) -> Item {
        Item {
            id: item_id(),
            run: Some(run_id()),
            attempt: Some(RunAttemptId::new("attempt-2").unwrap()),
            native_key: String::new(),
            ordinal: 0,
            kind,
            status: ItemStatus::Running,
            text: String::new(),
            started_at: now(),
            completed_at: None,
            output_omitted: false,
            output_indicates_failure: false,
        }
    }

    fn command_item() -> Item {
        item(ItemKind::CommandExecution {
            command: "vp check".into(),
            cwd: None,
            exit_code: None,
            title: None,
        })
    }

    fn run(id: RunId, checkpoint: Option<CheckpointId>) -> Run {
        Run {
            restart_of: None,
            restart_cancelled_work: vec![],
            checkpoint_scope: None,
            native_baseline_heads: Default::default(),
            id,
            ordinal: 1,
            message: MessageId::new("message-1").unwrap(),
            selection: ModelSelection {
                instance: "codex".into(),
                driver: Driver::Codex,
                model: "gpt-5.4".into(),
                options: Default::default(),
            },
            status: RunStatus::Running,
            attempt: Some(RunAttemptId::new("attempt-2").unwrap()),
            queue_position: None,
            queue_held: false,
            requested_at: now(),
            started_at: Some(now()),
            completed_at: None,
            source_plan: None,
            checkpoint,
            continuation: false,
        }
    }

    fn attempt(id: &str, run: RunId, ordinal: u64, status: AttemptStatus) -> Attempt {
        Attempt {
            id: RunAttemptId::new(id).unwrap(),
            run,
            ordinal,
            status,
            native_thread: None,
            native_turn: None,
            native_head: None,
            accepted: true,
            usage: None,
            context_usage: None,
            turn_usage: None,
            usage_accumulator: None,
            usage_observed: false,
            rejected_limits: Default::default(),
            started_at: now(),
            completed_at: None,
        }
    }

    fn request(id: &str) -> Request {
        Request {
            owner_path: vec![],
            id: RuntimeRequestId::new(id).unwrap(),
            attempt: RunAttemptId::new("attempt-2").unwrap(),
            native_key: id.into(),
            body: RequestBody::Questions { questions: vec![] },
            capability: ResponseCapability::Live,
            status: RequestStatus::Pending,
            decision: None,
            answers: None,
            attachments: Default::default(),
            created_at: now(),
            resolved_at: None,
        }
    }

    fn transfer(id: &str, kind: TransferKind, source: &str, superseded: bool) -> Transfer {
        Transfer {
            native_source: None,
            instance: None,
            target_run: None,
            delivery: None,
            id: ContextTransferId::new(id).unwrap(),
            kind,
            source: ThreadId::new(source).unwrap(),
            target: ThreadId::new("thread-1").unwrap(),
            boundary: 3,
            history: HistoricalContext {
                messages: vec![],
                context: String::new(),
                omitted_items: 0,
                omitted_item_ids: vec![],
            },
            superseded,
        }
    }

    #[test]
    fn retains_identity_while_linking_a_turn_item_to_its_execution_and_provider_entities() {
        let attempts = vec![
            attempt("attempt-1", run_id(), 1, AttemptStatus::Superseded),
            attempt("attempt-2", run_id(), 2, AttemptStatus::Running),
        ];
        let state = State {
            runs: vec![run(run_id(), None)],
            attempts: [
                attempts.clone(),
                vec![attempt(
                    "other-attempt",
                    RunId::new("run-2").unwrap(),
                    1,
                    AttemptStatus::Running,
                )],
            ]
            .concat(),
            items: vec![command_item()],
            ..State::default()
        };

        let support = resolve_item_support(&state, &item_id());
        assert_eq!(support.item, Some(command_item()));
        assert_eq!(support.run, Some(run(run_id(), None)));
        assert_eq!(support.attempts, attempts);
        assert_eq!(support.request, None);
    }

    #[test]
    fn resolves_synthetic_items_from_the_authoritative_visible_sequence() {
        let state = State {
            inherited_items: vec![command_item()],
            ..State::default()
        };
        assert_eq!(
            resolve_item_support(&state, &item_id()).item,
            Some(command_item())
        );
    }

    #[test]
    fn returns_the_stable_empty_support_for_unknown_items() {
        assert_eq!(
            resolve_item_support(&State::default(), &item_id()),
            ItemSupport::default()
        );
    }

    #[test]
    fn compares_support_structurally_while_retaining_entity_identity_semantics() {
        assert_eq!(ItemSupport::default(), ItemSupport::default().clone());
        assert_ne!(
            ItemSupport::default(),
            ItemSupport {
                attempts: vec![attempt("attempt", run_id(), 1, AttemptStatus::Running)],
                ..ItemSupport::default()
            }
        );
    }

    #[test]
    fn links_a_request_item_to_its_request() {
        let asked = item(ItemKind::UserInputRequest {
            request: RuntimeRequestId::new("request-1").unwrap(),
        });
        let state = State {
            items: vec![asked],
            requests: vec![request("request-0"), request("request-1")],
            ..State::default()
        };
        assert_eq!(
            resolve_item_support(&state, &item_id()).request,
            Some(request("request-1"))
        );
    }

    #[test]
    fn links_an_item_to_its_run_checkpoint() {
        let checkpoint = Checkpoint {
            status: CheckpointStatus::Ready,
            scope: None,
            id: CheckpointId::new("checkpoint-1").unwrap(),
            run: Some(run_id()),
            run_ordinal: 1,
            native_heads: Default::default(),
            file_ref: "ref".into(),
            files: vec![],
        };
        let state = State {
            runs: vec![run(run_id(), Some(checkpoint.id.clone()))],
            checkpoints: vec![checkpoint.clone()],
            items: vec![command_item()],
            ..State::default()
        };
        assert_eq!(
            resolve_item_support(&state, &item_id()).checkpoint,
            Some(checkpoint)
        );
    }

    #[test]
    fn links_a_subagent_item_to_its_task_and_result_transfer() {
        let task = Task {
            original_message: None,
            native_task: None,
            background: false,
            id: NodeId::new("task-1").unwrap(),
            native_key: "task-1".into(),
            run: Some(run_id()),
            attempt: RunAttemptId::new("attempt-2").unwrap(),
            child_thread: ThreadId::new("child-1").unwrap(),
            parent_task: None,
            prompt: "Check".into(),
            title: None,
            started_at: now(),
            completed_at: None,
            model: None,
            status: ItemStatus::Completed,
            result: Some("done".into()),
            progress: None,
            wake: CompletionWake::Always,
            delivery: DeliveryState::Delivered,
            generation: 0,
        };
        let result = transfer("result-new", TransferKind::SubagentResult, "child-1", false);
        let state = State {
            items: vec![item(ItemKind::Subagent {
                task: task.id.clone(),
            })],
            tasks: vec![task.clone()],
            transfers: vec![
                transfer("result-old", TransferKind::SubagentResult, "child-1", true),
                result.clone(),
                transfer("other", TransferKind::SubagentResult, "child-2", false),
            ],
            ..State::default()
        };
        let support = resolve_item_support(&state, &item_id());
        assert_eq!(support.task, Some(task));
        assert_eq!(support.transfer, Some(result));
    }

    #[test]
    fn links_a_fork_item_to_its_fork_transfer() {
        let fork = transfer("fork", TransferKind::Fork, "parent-1", false);
        let state = State {
            items: vec![Item {
                run: None,
                ..item(ItemKind::Fork {
                    parent: ThreadId::new("parent-1").unwrap(),
                    boundary: 3,
                })
            }],
            transfers: vec![
                transfer("merge", TransferKind::MergeBack, "parent-1", false),
                fork.clone(),
            ],
            ..State::default()
        };
        let support = resolve_item_support(&state, &item_id());
        assert_eq!(support.run, None);
        assert!(support.attempts.is_empty());
        assert_eq!(support.transfer, Some(fork));
    }
}

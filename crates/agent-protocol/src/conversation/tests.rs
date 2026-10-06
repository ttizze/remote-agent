use super::*;
use crate::protocol::{Call, MAX_FRAME_BYTES, Response, decode, encode};
use agent_domain::{
    AttachmentKind, DispatchMode, Driver, Input, InputEnvelope, ItemStatus, Json, MessageAuthor,
    ProviderEvent, ProviderItem, RunStatus, SendMessage, ThreadMachine, ToolPresentation, fold,
};
use proptest::prelude::*;
use std::collections::BTreeMap;
use std::fmt::Debug;

fn round_trip<T>(value: &T)
where
    T: Serialize + serde::de::DeserializeOwned + PartialEq + Debug,
{
    let json = serde_json::to_string(value).unwrap();
    assert_eq!(&serde_json::from_str::<T>(&json).unwrap(), value);
    assert_eq!(&decode::<T>(&encode(value).unwrap()).unwrap(), value);
}

/// The Host sends `Response<T>` first and bare items after it on the same stream.
fn response_round_trip<T>(value: T)
where
    T: Serialize + serde::de::DeserializeOwned + PartialEq + Debug + Clone,
{
    round_trip(&value);
    let bytes = encode(Response::<T>::Success {
        result: value.clone(),
    })
    .unwrap();
    let Response::Success { result } = decode::<Response<T>>(&bytes).unwrap() else {
        panic!("success expected");
    };
    assert_eq!(result, value);
}

fn call_round_trip(call: Call, method: &str) {
    assert_eq!(call.method(), method);
    let bytes = encode(&call).unwrap();
    assert_eq!(encode(decode::<Call>(&bytes).unwrap()).unwrap(), bytes);
}

fn id<T: serde::de::DeserializeOwned>(value: &str) -> T {
    serde_json::from_value(serde_json::json!(value)).unwrap()
}
fn at() -> Timestamp {
    Timestamp::parse("2026-10-06T00:00:00Z").unwrap()
}
fn selection() -> ModelSelection {
    ModelSelection {
        instance: "codex".into(),
        driver: Driver::Codex,
        model: "gpt-6-luna".into(),
        options: BTreeMap::from([("effort".into(), "high".into())]),
    }
}
fn attachment() -> Attachment {
    Attachment {
        kind: AttachmentKind::File,
        source: None,
        id: "notes".into(),
        name: "notes.md".into(),
        mime_type: "text/markdown".into(),
        path: "/tmp/notes.md".into(),
        size: 12,
    }
}
fn project(id: &str) -> Project {
    Project {
        id: id.into(),
        name: id.into(),
        roots: vec![crate::models::ProjectRoot {
            path: format!("/work/{id}"),
        }],
    }
}

/// A thread driven through the domain: every step's facts with their sequences.
struct Thread {
    state: State,
    facts: Vec<Vec<SequencedFact>>,
}
impl Thread {
    fn step(&mut self, key: &str, input: Input) -> Reply {
        let step = ThreadMachine::step(
            &self.state,
            &InputEnvelope {
                at: at(),
                key: key.into(),
                input,
            },
        );
        self.state = fold(&self.state, &step.facts).unwrap();
        let base = self.facts.iter().map(Vec::len).sum::<usize>() as u64;
        self.facts.push(
            step.facts
                .into_iter()
                .enumerate()
                .map(|(index, fact)| SequencedFact {
                    sequence: 100 + base + index as u64,
                    thread_sequence: base + index as u64 + 1,
                    fact,
                })
                .collect(),
        );
        step.reply
    }
    fn command(&mut self, key: &str, command: Command) -> Reply {
        self.step(
            key,
            Input::Command {
                id: id(key),
                command: Box::new(command),
                receipt: None,
            },
        )
    }
    fn provider(&mut self, key: &str, event: ProviderEvent) {
        let attempt = self.state.runs[0].attempt.clone().unwrap();
        self.step(
            key,
            Input::Provider {
                attempt,
                event: Box::new(event),
            },
        );
    }
    fn sequence(&self) -> u64 {
        self.facts
            .last()
            .and_then(|facts| facts.last())
            .unwrap()
            .sequence
    }
}

fn send() -> Command {
    Command::Send(SendMessage {
        created_by: MessageAuthor::User,
        creation_source: "client".into(),
        id: id("first"),
        text: "Summarize the notes".into(),
        attachments: vec![attachment()],
        selection: None,
        mode: DispatchMode::StartImmediately,
        intent: None,
        source_plan: None,
        resolved_plan: None,
        continuation: None,
        title_seed: Some("Notes".into()),
    })
}

/// A completed turn whose answer streamed as one coalesced 64 KiB delta, plus a tool.
fn thread() -> Thread {
    let mut thread = Thread {
        state: State::default(),
        facts: vec![],
    };
    thread.command(
        "create",
        Command::Create {
            thread: id("thread"),
            project: "project".into(),
            title: "Thread".into(),
            selection: selection(),
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Default,
            workspace: None,
        },
    );
    assert!(matches!(thread.command("send", send()), Reply::Run(_)));
    for (key, event) in [
        (
            "ready",
            ProviderEvent::SessionReady {
                native_thread: "native".into(),
            },
        ),
        (
            "turn",
            ProviderEvent::TurnStarted {
                native_turn: Some("turn".into()),
            },
        ),
        (
            "tool",
            ProviderEvent::ItemFinished {
                key: "tool".into(),
                kind: ProviderItem::Tool {
                    presentation: ToolPresentation {
                        title: Some("Read".into()),
                        source: None,
                        surface: None,
                        icon: None,
                    },
                    name: "read".into(),
                    input: Json(serde_json::json!({"path": "notes.md"})),
                    output: Some(Json(serde_json::json!([{"text": "notes"}]))),
                },
                text: Some("notes".into()),
                status: ItemStatus::Completed,
            },
        ),
        (
            "answer",
            ProviderEvent::TextDelta {
                key: "answer".into(),
                kind: ProviderItem::Text,
                text: "あ".repeat(64 * 1024 / 3),
            },
        ),
        (
            "answered",
            ProviderEvent::ItemFinished {
                key: "answer".into(),
                kind: ProviderItem::Text,
                text: None,
                status: ItemStatus::Completed,
            },
        ),
        (
            "done",
            ProviderEvent::TurnFinished {
                status: RunStatus::Completed,
                native_head: Some("turn".into()),
            },
        ),
    ] {
        thread.provider(key, event);
    }
    thread
}

fn history_row(state: &State) -> HistoryRow {
    let item = state.visible_items().pop().unwrap().clone();
    let message = match &item.kind {
        agent_domain::ItemKind::AssistantMessage { message } => state.message(message).cloned(),
        _ => None,
    };
    HistoryRow {
        position: 3,
        source: id("thread"),
        inherited: false,
        item,
        message,
        plan: None,
    }
}

#[test]
fn requests_round_trip_and_join_the_call_table() {
    let dispatch = Dispatch {
        thread_id: id("thread"),
        command_id: id("send"),
        command: send(),
    };
    round_trip(&dispatch);
    call_round_trip(Call::Dispatch(Box::new(dispatch)), "conversation/dispatch");

    let mut launch = Launch {
        command_id: id("launch"),
        thread_id: Some(id("thread")),
        project_id: "project".into(),
        title: "New thread".into(),
        selection: selection(),
        runtime_mode: RuntimeMode::ApprovalRequired,
        interaction_mode: InteractionMode::Plan,
        workspace: WorkspaceStrategy::Root { branch: None },
        message: Some(LaunchMessage {
            id: Some(id("message")),
            text: "Start".into(),
            attachments: vec![attachment()],
            creation_source: "client".into(),
            title_seed: Some("Start".into()),
        }),
    };
    for workspace in [
        WorkspaceStrategy::Root {
            branch: Some("main".into()),
        },
        WorkspaceStrategy::ExistingWorktree {
            worktree_path: "/work/tree".into(),
            branch: None,
        },
        WorkspaceStrategy::Worktree {
            base_ref: "origin/main".into(),
            branch: Some("feature".into()),
            start_from_origin: true,
        },
    ] {
        round_trip(&workspace);
        launch.workspace = workspace;
        round_trip(&launch);
    }
    launch.thread_id = None;
    launch.message = None;
    round_trip(&launch);
    call_round_trip(Call::Launch(Box::new(launch)), "conversation/launch");

    for after_sequence in [None, Some(7)] {
        let subscribe = SubscribeThread {
            thread_id: id("thread"),
            after_sequence,
            request_completion_marker: true,
            accept_bounded_snapshot: after_sequence.is_none(),
        };
        round_trip(&subscribe);
        call_round_trip(
            Call::ThreadStream(subscribe),
            "conversation/subscribeThread",
        );
    }
    for location in [ShellLocation::Active, ShellLocation::Archived] {
        let subscribe = SubscribeShell {
            after_sequence: Some(3),
            request_completion_marker: false,
            location,
        };
        round_trip(&location);
        round_trip(&subscribe);
        call_round_trip(Call::ShellStream(subscribe), "conversation/subscribeShell");
    }
    let get = GetThread {
        thread_id: id("thread"),
        bounded: true,
    };
    round_trip(&get);
    call_round_trip(Call::GetThread(get), "conversation/getThread");
    let item = GetTurnItem {
        thread_id: id("thread"),
        item_id: id("item"),
    };
    round_trip(&item);
    call_round_trip(Call::TurnItem(item), "conversation/getTurnItem");
    for cursor in [None, Some("eyJ2IjoxfQ".into())] {
        let history = ReadHistory {
            thread_id: id("thread"),
            cursor,
        };
        round_trip(&history);
        call_round_trip(Call::ReadHistory(history), "conversation/readHistory");
    }
    for limit in [None, Some(10)] {
        let search = Search {
            query: "notes".into(),
            limit,
        };
        round_trip(&search);
        call_round_trip(Call::Search(search), "conversation/search");
    }
    let diff = GetTurnDiff {
        thread_id: id("thread"),
        from_run_ordinal: 0,
        to_run_ordinal: 2,
        ignore_whitespace: true,
    };
    round_trip(&diff);
    call_round_trip(Call::TurnDiff(diff), "conversation/turnDiff");
    round_trip(&ScanAgentSessions {});
    call_round_trip(
        Call::ScanAgentSessions(ScanAgentSessions {}),
        "conversation/agentSessions/scan",
    );
    let import = ImportAgentSessions {
        project_id: "project".into(),
        expected_root: Some("/work/project".into()),
    };
    round_trip(&import);
    call_round_trip(
        Call::ImportAgentSessions(import),
        "conversation/agentSessions/import",
    );
}

#[test]
fn replies_round_trip_inside_responses() {
    let thread = thread();
    for reply in [
        Reply::Accepted,
        Reply::Run(id("run")),
        Reply::Thread(id("thread")),
        Reply::Request(id("request")),
        Reply::Rejected {
            reason: "rollback-pending".into(),
        },
        Reply::Ignored,
    ] {
        let committed = Committed {
            reply,
            thread_sequence: 4,
            sequence: 104,
            replayed: false,
        };
        response_round_trip(committed.clone());
        response_round_trip(Launched {
            thread_id: id("thread"),
            committed,
            resumed: true,
        });
    }

    let window = SnapshotWindow {
        history_cursor: Some("cursor".into()),
        has_more_history: true,
        latest_local_ordinal: Some(9),
        payload_budget_exceeded: false,
    };
    round_trip(&window);
    for window in [None, Some(window)] {
        response_round_trip(ThreadSnapshot {
            snapshot_sequence: thread.sequence(),
            thread_sequence: 12,
            state: Arc::new(thread.state.clone()),
            window,
        });
    }

    let row = history_row(&thread.state);
    assert!(row.message.is_some());
    response_round_trip(Some(row.clone()));
    response_round_trip(None::<HistoryRow>);
    for next_cursor in [None, Some("older".into())] {
        response_round_trip(HistoryPage {
            rows: vec![row.clone()],
            has_more: next_cursor.is_some(),
            next_cursor,
        });
    }

    for source in [SearchSource::User, SearchSource::Assistant] {
        response_round_trip(vec![SearchMatch {
            thread_id: id("thread"),
            project_id: "project".into(),
            source,
            snippet: "…the notes…".into(),
            message_created_at: at(),
        }]);
    }
    response_round_trip(TurnDiff {
        thread_id: id("thread"),
        from_run_ordinal: 1,
        to_run_ordinal: 2,
        diff: "diff --git a/x b/x\n".into(),
    });

    let git = ProjectGit {
        remote_key: Some("github.com/owner/repo".into()),
        repository: Some("owner/repo".into()),
    };
    round_trip(&git);
    let candidate = SessionCandidate {
        path: "/work/project".into(),
        title: "project".into(),
        project_id: Some("project".into()),
        sources: vec![Driver::Codex, Driver::Claude],
        thread_count: 3,
        last_active_at: Some(at()),
        already_imported: false,
        git: Some(git),
    };
    round_trip(&candidate);
    response_round_trip(SessionScan {
        candidates: vec![
            candidate.clone(),
            SessionCandidate {
                project_id: None,
                last_active_at: None,
                git: None,
                ..candidate
            },
        ],
        scanned_at: at(),
        truncated: true,
    });
    response_round_trip(ImportCounts {
        imported: 2,
        skipped: 1,
    });
}

#[test]
fn stream_items_round_trip() {
    let thread = thread();
    for facts in &thread.facts {
        for fact in facts {
            round_trip(fact);
        }
    }
    let snapshot = ThreadSnapshot {
        snapshot_sequence: thread.sequence(),
        thread_sequence: 12,
        state: Arc::new(thread.state.clone()),
        window: None,
    };
    for update in [
        ThreadUpdate::Snapshot(snapshot),
        ThreadUpdate::Facts(thread.facts.concat()),
        ThreadUpdate::Synchronized,
    ] {
        response_round_trip(update);
    }

    let shell = Box::new(agent_domain::shell(&thread.state).unwrap());
    let snapshot = ShellSnapshot {
        snapshot_sequence: 40,
        projects: vec![project("project")],
        threads: vec![*shell.clone()],
    };
    round_trip(&snapshot);
    for update in [
        ShellUpdate::Snapshot(snapshot),
        ShellUpdate::Projects {
            sequence: 41,
            projects: vec![project("project"), project("other")],
        },
        ShellUpdate::ThreadUpdated {
            sequence: 42,
            thread: shell,
        },
        ShellUpdate::ThreadRemoved {
            sequence: 43,
            thread_id: id("thread"),
        },
        ShellUpdate::ProjectUpdated {
            sequence: 44,
            project: project("project"),
        },
        ShellUpdate::ProjectRemoved {
            sequence: 45,
            project_id: "other".into(),
        },
        ShellUpdate::Synchronized,
    ] {
        response_round_trip(update);
    }
}

#[test]
fn errors_keep_a_stable_code_and_delivery_through_the_wire() {
    let errors = [
        ConversationError::ResponseTooLarge,
        ConversationError::ThreadNotFound(id("thread")),
        ConversationError::CommandIdConflict(id("launch")),
        ConversationError::InternalCommand,
        ConversationError::InvalidCursor,
        ConversationError::InvalidSearch,
        ConversationError::ProjectNotFound("project".into()),
        ConversationError::ProjectChanged("project".into()),
        ConversationError::CheckpointUnavailable(2),
        ConversationError::AttachmentUnavailable("missing upload".into()),
        ConversationError::Unavailable("database is locked".into()),
    ];
    assert_eq!(
        errors
            .iter()
            .map(ConversationError::code)
            .collect::<Vec<_>>(),
        ErrorCode::ALL
    );
    for error in errors {
        let failure = RpcFailure::from(error.clone());
        round_trip(&failure);
        let bytes = encode(Response::<Committed>::Failure {
            error: failure.clone(),
        })
        .unwrap();
        let Response::Failure { error: decoded } = decode::<Response<Committed>>(&bytes).unwrap()
        else {
            panic!("failure expected");
        };
        assert_eq!(ErrorCode::of(&decoded), Some(error.code()));
        assert_eq!(decoded.message, error.to_string());
        assert_eq!(
            decoded.delivery,
            if matches!(
                error,
                ConversationError::ResponseTooLarge | ConversationError::Unavailable(_)
            ) {
                Delivery::Unknown
            } else {
                Delivery::NotSent
            }
        );
    }
    let mut codes: Vec<_> = ErrorCode::ALL.iter().map(|code| code.as_str()).collect();
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), ErrorCode::ALL.len());
    assert_eq!(
        ErrorCode::of(&RpcFailure {
            code: "method_not_found".into(),
            message: String::new(),
            delivery: Delivery::Unknown,
        }),
        None
    );
}

#[test]
fn oversized_responses_fail_with_the_conversation_code() {
    let bytes = crate::protocol::response_frame(Response::Success {
        result: "x".repeat(MAX_FRAME_BYTES).into(),
    })
    .unwrap();
    let Response::Failure { error } = decode::<Response<String>>(&bytes).unwrap() else {
        panic!("failure expected");
    };
    assert_eq!(ErrorCode::of(&error), Some(ErrorCode::ResponseTooLarge));
    assert_eq!(error.delivery, Delivery::Unknown);
}

#[test]
fn clients_cannot_dispatch_host_only_commands() {
    let thread = thread();
    let run = thread.state.runs[0].id.clone();
    let attempt = thread.state.runs[0].attempt.clone().unwrap();
    let dispatch = |command| Dispatch {
        thread_id: id("thread"),
        command_id: id("command"),
        command,
    };
    for command in [
        Command::NativeInput {
            attempt: attempt.clone(),
            event: Box::new(ProviderEvent::ContextInjected),
        },
        Command::BindNativeChild {
            native_thread: None,
            owner: attempt,
            parent: id("parent"),
            task: id("task"),
            generation: 1,
        },
        Command::TaskProgress {
            task: id("task"),
            progress: None,
            model: None,
        },
        Command::TaskResult {
            source_message: None,
            generation: None,
            context: None,
            task: id("task"),
            status: ItemStatus::Completed,
            result: "done".into(),
        },
        Command::AcceptTaskWake { task_ids: vec![] },
        Command::Import {
            thread: id("thread"),
            project: "project".into(),
            title: "Imported".into(),
            selection: selection(),
            workspace: None,
            created_at: at(),
            updated_at: at(),
            messages: vec![],
            native: agent_domain::NativeBinding {
                instance: "codex".into(),
                thread: "native".into(),
                head: None,
            },
        },
        Command::ContinueRestart {
            source: run.clone(),
            enabled: true,
        },
        Command::ReleasePrepared { run: run.clone() },
        Command::FailPrepared {
            run: run.clone(),
            message: "failed".into(),
        },
    ] {
        assert!(host_only_command(&command));
        assert_eq!(
            dispatch(command).validate(),
            Err(ConversationError::InternalCommand)
        );
    }
    for command in [
        send(),
        Command::Rename {
            title: "Renamed".into(),
        },
        Command::Interrupt {
            run: run.clone(),
            hold_queue: false,
            reason: None,
        },
        Command::RetryPrepared { run },
        Command::Delegate {
            task: id("task"),
            child: id("child"),
            prompt: "help".into(),
            title: None,
            selection: selection(),
            runtime_mode: agent_domain::RuntimeMode::FullAccess,
            interaction_mode: agent_domain::InteractionMode::Default,
            wake: agent_domain::CompletionWake::Always,
        },
        Command::Stop,
    ] {
        assert!(!host_only_command(&command));
        assert_eq!(dispatch(command).validate(), Ok(()));
    }
}

#[test]
fn a_typical_facts_batch_stays_far_below_the_frame_cap() {
    let thread = thread();
    let largest = thread
        .facts
        .iter()
        .map(|facts| {
            encode(Response::Success {
                result: ThreadUpdate::Facts(facts.clone()),
            })
            .unwrap()
            .len()
        })
        .max()
        .unwrap();
    // The coalesced 64 KiB delta dominates; a whole turn replays in well under 1 MiB.
    assert!(
        largest > 64 * 1024 && largest < MAX_FRAME_BYTES / 128,
        "{largest}"
    );
    let turn = encode(ThreadUpdate::Facts(thread.facts.concat()))
        .unwrap()
        .len();
    assert!(turn < MAX_FRAME_BYTES / 16, "{turn}");
}

fn text_fact(sequence: u64, bytes: usize) -> SequencedFact {
    SequencedFact {
        sequence,
        thread_sequence: sequence,
        fact: agent_domain::Fact {
            at: at(),
            body: agent_domain::FactBody::ThreadRenamed {
                title: "x".repeat(bytes),
            },
        },
    }
}

#[test]
fn a_step_larger_than_a_frame_is_sent_in_frames_that_fit() {
    let facts: Vec<_> = (1..=20)
        .map(|sequence| text_fact(sequence, agent_domain::MAX_FACT_TEXT))
        .collect();
    let updates = fact_updates(facts.clone(), FACTS_FRAME_BUDGET);
    assert!(updates.len() > 1);
    let mut sent = vec![];
    for update in updates {
        let frame = encode(Response::Success {
            result: update.clone(),
        })
        .unwrap();
        assert!(frame.len() <= MAX_FRAME_BYTES);
        let ThreadUpdate::Facts(batch) = update else {
            panic!("facts expected");
        };
        sent.extend(batch);
    }
    assert_eq!(sent, facts);
}

proptest! {
    #[test]
    fn fact_updates_keep_order_and_respect_the_budget(
        sizes in prop::collection::vec(0usize..600, 0..40),
        budget in 1usize..2_000,
    ) {
        let facts: Vec<_> = sizes
            .iter()
            .enumerate()
            .map(|(index, size)| text_fact(index as u64 + 1, *size))
            .collect();
        let updates = fact_updates(facts.clone(), budget);
        let mut sent = vec![];
        for update in updates {
            let ThreadUpdate::Facts(batch) = update else {
                panic!("facts expected");
            };
            prop_assert!(!batch.is_empty());
            let size: usize = batch
                .iter()
                .map(|fact| encode(fact).unwrap().len())
                .sum();
            prop_assert!(batch.len() == 1 || size <= budget);
            sent.extend(batch);
        }
        prop_assert_eq!(sent, facts);
    }
}

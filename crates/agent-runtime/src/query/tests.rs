//! History and item reads through a thread actor.
use super::*;
use crate::store::tests::{selection, temp_store};
use crate::{
    ActorContext, CommandOrigin, CommitBatch, HostProject, ShellProjector, ThreadHead,
    ThreadShellProjector, search_changes,
};
use agent_domain::{
    Command, CommandId, DispatchMode, Fact, FactBody, InputIntent, InteractionMode, MessageAuthor,
    MessageId, Role, RuntimeMode, SendMessage, apply,
};

struct Projects(Vec<&'static str>);
impl ProjectDirectory for Projects {
    fn projects(&self) -> Vec<HostProject> {
        self.0
            .iter()
            .map(|id| HostProject {
                id: (*id).into(),
                name: (*id).into(),
                root: format!("/work/{id}"),
            })
            .collect()
    }
}

fn at(minute: u32) -> Timestamp {
    Timestamp::parse(&format!("2026-09-27T00:{minute:02}:00Z")).unwrap()
}

/// Commits facts the way an actor step does, with the shell row and search index.
struct Writer {
    store: Store,
    thread: ThreadId,
    state: State,
    head: ThreadHead,
}
impl Writer {
    async fn create(store: &Store, thread: &str, project: &str) -> Self {
        let mut writer = Self {
            store: store.clone(),
            thread: ThreadId::new(thread).unwrap(),
            state: State::default(),
            head: ThreadHead::default(),
        };
        writer
            .commit(
                0,
                FactBody::ThreadCreated {
                    id: writer.thread.clone(),
                    project: project.into(),
                    title: thread.into(),
                    selection: selection(),
                    runtime_mode: RuntimeMode::FullAccess,
                    interaction_mode: InteractionMode::Default,
                    created_by: agent_domain::MessageAuthor::User,
                    creation_source: "desktop".into(),
                },
            )
            .await;
        writer
    }
    async fn commit(&mut self, minute: u32, body: FactBody) {
        let facts = vec![Fact {
            at: at(minute),
            body,
        }];
        for fact in &facts {
            apply(&mut self.state, fact).unwrap();
        }
        let batch = CommitBatch {
            thread: self.thread.clone(),
            at: at(minute),
            base_thread_seq: self.head.thread_seq,
            input_seq: self.head.input_seq + 1,
            receipt: None,
            search: search_changes(&self.state, &facts),
            facts,
            effects: vec![],
            settle: None,
            shell: ThreadShellProjector.project(&self.state),
            needs_recovery: false,
            snapshot: None,
        };
        self.head = self.store.commit(batch).await.unwrap().head;
    }
    async fn message(&mut self, id: &str, role: Role, text: &str, minute: u32, finished: bool) {
        self.commit(
            minute,
            FactBody::MessageCreated {
                context: None,
                id: MessageId::new(id).unwrap(),
                run: None,
                role,
                text: text.into(),
                attachments: vec![],
                intent: InputIntent::TurnStart,
                created_by: if role == Role::User {
                    MessageAuthor::User
                } else {
                    MessageAuthor::Agent
                },
                creation_source: "client".into(),
            },
        )
        .await;
        if role == Role::Assistant && finished {
            self.commit(
                minute,
                FactBody::MessageFinished {
                    id: MessageId::new(id).unwrap(),
                },
            )
            .await;
        }
    }
}

fn summary(matches: &[SearchMatch]) -> Vec<(&str, SearchSource, &str)> {
    matches
        .iter()
        .map(|m| (m.thread.as_str(), m.source, m.snippet.as_str()))
        .collect()
}

#[tokio::test]
async fn returns_one_finished_user_or_assistant_match_per_active_thread() {
    let (_dir, store) = temp_store();
    let project = "project:search";
    let projects = Projects(vec![project]);

    let mut both = Writer::create(&store, "thread:both", project).await;
    both.message(
        "both-assistant",
        Role::Assistant,
        "needle from the answer",
        3,
        true,
    )
    .await;
    both.message(
        "both-user-old",
        Role::User,
        "older needle question",
        1,
        true,
    )
    .await;
    both.message(
        "both-user-new",
        Role::User,
        "newer needle question",
        2,
        true,
    )
    .await;
    let mut assistant = Writer::create(&store, "thread:assistant", project).await;
    assistant
        .message(
            "assistant-only",
            Role::Assistant,
            "needle in an answer",
            1,
            true,
        )
        .await;
    assistant
        .message(
            "assistant-streaming",
            Role::Assistant,
            "needle still typing",
            1,
            false,
        )
        .await;
    assistant
        .message(
            "assistant-system",
            Role::System,
            "needle system prompt",
            1,
            true,
        )
        .await;
    let mut archived = Writer::create(&store, "thread:archived", project).await;
    archived
        .message("archived", Role::User, "needle archived", 1, true)
        .await;
    archived
        .commit(1, FactBody::ThreadArchived { archived: true })
        .await;
    let mut deleted = Writer::create(&store, "thread:deleted", project).await;
    deleted
        .message("deleted", Role::User, "needle deleted", 1, true)
        .await;
    deleted.commit(1, FactBody::ThreadDeleted).await;
    let mut orphaned = Writer::create(&store, "thread:orphaned", "project:search-deleted").await;
    orphaned
        .message("orphaned", Role::User, "needle orphaned", 1, true)
        .await;

    let result = store.search("NEEDLE", Some(20), &projects).unwrap();
    assert_eq!(
        summary(&result),
        [
            ("thread:both", SearchSource::User, "newer needle question"),
            (
                "thread:assistant",
                SearchSource::Assistant,
                "needle in an answer"
            ),
        ]
    );
    assert_eq!(result[0].message_created_at, Some(at(2)));
    assert_eq!(store.search("needle", Some(1), &projects).unwrap().len(), 1);
    // LIKE wildcards in the query match literally.
    assert_eq!(store.search("ne%le", None, &projects).unwrap(), []);
}

#[tokio::test]
async fn limits_matches_in_sql_after_leaving_out_removed_projects() {
    let (_dir, store) = temp_store();
    let project = "project:search-limit";
    let mut removed = Writer::create(&store, "thread:removed", "project:removed").await;
    removed
        .message("removed", Role::User, "needle removed", 9, true)
        .await;
    for (index, thread) in ["thread:limit-a", "thread:limit-b", "thread:limit-c"]
        .into_iter()
        .enumerate()
    {
        let mut writer = Writer::create(&store, thread, project).await;
        writer
            .message(thread, Role::User, "needle kept", index as u32 + 1, true)
            .await;
    }

    let rows = store
        .read(|c| search_rows(c, &like_pattern("needle"), &[project.to_owned()], 2))
        .unwrap();
    let threads: Vec<_> = rows.iter().map(|row| row.0.as_str()).collect();
    assert_eq!(threads, ["thread:limit-c", "thread:limit-b"]);
    let result = store
        .search("needle", Some(2), &Projects(vec![project]))
        .unwrap();
    assert_eq!(
        result.iter().map(|m| m.thread.as_str()).collect::<Vec<_>>(),
        ["thread:limit-c", "thread:limit-b"]
    );
}

// Visits don't change activity; search sorts by activity.
#[tokio::test]
async fn orders_matching_threads_by_activity_not_by_their_latest_fact() {
    let (_dir, store) = temp_store();
    let project = "project:search-activity";
    let mut older = Writer::create(&store, "thread:older-activity", project).await;
    older
        .message("older", Role::User, "needle older", 1, true)
        .await;
    let mut newer = Writer::create(&store, "thread:newer-activity", project).await;
    newer
        .message("newer", Role::User, "needle newer", 5, true)
        .await;
    older
        .commit(10, FactBody::ThreadVisited { at: at(10) })
        .await;
    assert!(
        store.thread_head(&older.thread).unwrap().global_seq
            > store.thread_head(&newer.thread).unwrap().global_seq
    );

    let result = store
        .search("needle", None, &Projects(vec![project]))
        .unwrap();
    assert_eq!(
        result.iter().map(|m| m.thread.as_str()).collect::<Vec<_>>(),
        ["thread:newer-activity", "thread:older-activity"]
    );
}

/// A corrupt timestamp stands in for a non-text payload, which a STRICT table rejects.
#[tokio::test]
async fn reports_an_unreadable_match_as_a_decode_failure() {
    let (_dir, store) = temp_store();
    let project = "project:search-corrupt";
    let mut thread = Writer::create(&store, "thread:corrupt", project).await;
    thread
        .message("corrupt", Role::User, "20260927", 1, true)
        .await;
    store
        .write(|tx| {
            tx.execute(
                "UPDATE search_messages SET created_at = 'not a time' WHERE message_id = 'corrupt'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let error = store
        .search("0260927", None, &Projects(vec![project]))
        .unwrap_err();
    assert!(matches!(
        error,
        QueryError::Search {
            operation: "decode"
        }
    ));
    assert!(!error.to_string().contains("0260927"));
}

#[test]
fn validates_the_query_and_limit() {
    let (_dir, store) = temp_store();
    let projects = Projects(vec![]);
    for (query, limit) in [
        ("a", None),
        ("  a  ", None),
        ("ok", Some(0)),
        ("ok", Some(51)),
    ] {
        assert!(matches!(
            store.search(query, limit, &projects),
            Err(QueryError::InvalidSearch)
        ));
    }
    let long = "x".repeat(SEARCH_MAX_QUERY_CHARS + 1);
    assert!(store.search(&long, None, &projects).is_err());
    assert_eq!(store.search(" ok ", Some(50), &projects).unwrap(), []);
    // The query is measured in UTF-16 units.
    assert_eq!(store.search("😀", None, &projects).unwrap(), []);
    let astral = "😀".repeat(SEARCH_MAX_QUERY_CHARS / 2 + 1);
    assert!(matches!(
        store.search(&astral, None, &projects),
        Err(QueryError::InvalidSearch)
    ));
}

#[test]
fn bounds_snippets_in_utf16_units() {
    let snippet = search_snippet(&"😀".repeat(200), "missing");
    assert_eq!(snippet, format!("{}…", "😀".repeat(118)));
    assert_eq!(snippet.encode_utf16().count(), SEARCH_SNIPPET_CHARS - 3);
}

#[test]
fn centres_long_snippets_near_the_first_match() {
    assert_eq!(search_snippet("  short \n text ", "text"), "short text");
    let text = format!("{} Needle {}", "a".repeat(300), "b".repeat(300));
    let snippet = search_snippet(&text, "needle");
    assert_eq!(snippet.chars().count(), SEARCH_SNIPPET_CHARS - 2);
    assert!(snippet.starts_with('…') && snippet.ends_with('…'));
    assert_eq!(snippet.find("Needle"), Some('…'.len_utf8() + 72));
    let tail = search_snippet(&format!("{}needle", "a".repeat(300)), "needle");
    assert!(tail.starts_with('…') && tail.ends_with("needle"));
}

#[tokio::test]
async fn reads_history_pages_and_single_items_from_the_actor() {
    let (_dir, store) = temp_store();
    let thread = ThreadId::new("thread:history").unwrap();
    let handle = ActorHandle::spawn(ActorContext::new(store), thread.clone())
        .await
        .unwrap();
    let dispatch = |id: &str, command| {
        let handle = handle.clone();
        let id = CommandId::new(id).unwrap();
        async move {
            handle
                .dispatch(id, command, CommandOrigin::Client)
                .await
                .unwrap()
        }
    };
    dispatch(
        "create",
        Command::Create {
            workspace: None,
            thread,
            project: "project".into(),
            title: "History".into(),
            selection: selection(),
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Default,
            created_by: agent_domain::MessageAuthor::User,
            creation_source: "desktop".into(),
        },
    )
    .await;
    dispatch(
        "hello",
        Command::Send(SendMessage {
            context: None,
            title_seed: None,
            created_by: MessageAuthor::User,
            creation_source: "client".into(),
            id: MessageId::new("hello").unwrap(),
            text: "hello".into(),
            attachments: vec![],
            selection: None,
            mode: DispatchMode::StartImmediately,
            intent: None,
            source_plan: None,
            resolved_plan: None,
            continuation: None,
        }),
    )
    .await;

    let page = handle.history(None).await.unwrap();
    assert!(!page.has_more);
    let row = page
        .rows
        .iter()
        .find(|row| row.message.as_ref().is_some_and(|m| m.text == "hello"))
        .unwrap();
    assert_eq!(
        handle.turn_item(&row.item.id).await.unwrap(),
        Some(TurnItemDetail {
            row: row.clone(),
            task: None
        })
    );
    assert!(
        handle
            .turn_item(&TurnItemId::new("missing").unwrap())
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        handle.history(Some("not-valid")).await,
        Err(QueryError::Cursor(_))
    ));
}

/// Returns the output the timeline withholds, bounded at 256 KiB.
#[test]
fn reads_withheld_command_output_on_demand_within_its_bound() {
    use crate::sync::history::tests::{command_row, created};
    let mut state = created();
    state.items = vec![command_row(0, "o".repeat(300 * 1024))];
    let client = crate::sync::client_state(&std::sync::Arc::new(state.clone()));
    let page = recent_history(&client, 1, PagePolicy::RECENT);
    assert!(page.rows[0].item.text.is_empty() && page.rows[0].item.output_omitted);
    let detail = turn_item(&state, &state.items[0].id).unwrap().row;
    assert_eq!(
        detail.item.text,
        format!(
            "{}\n… output truncated for transport",
            "o".repeat(256 * 1024)
        )
    );
    assert_eq!(state.items[0].text.len(), 300 * 1024);
}

/// A subagent's prompt, progress and result come with its item read, bounded
/// at 256 KiB like the rest of the withheld detail instead of the timeline's
/// 32 KiB.
#[test]
fn reads_a_subagent_task_on_demand_within_the_detail_bound() {
    use crate::sync::history::tests::{created, item};
    use agent_domain::{CompletionWake, DeliveryState, ItemStatus, NodeId, RunAttemptId, Task};
    let marker = "\n… output truncated for transport";
    let result = "r".repeat(40 * 1024);
    let progress = format!("{}😀{}", "p".repeat(256 * 1024 - 1), "x".repeat(1024));
    let mut state = created();
    let task = NodeId::new("child-agent").unwrap();
    state.items = vec![item(
        "item-task",
        1,
        ItemKind::Subagent { task: task.clone() },
        String::new(),
    )];
    state.tasks = vec![Task {
        original_message: None,
        native_task: None,
        background: false,
        id: task,
        native_key: "child-agent".into(),
        run: None,
        attempt: RunAttemptId::new("attempt").unwrap(),
        child_thread: ThreadId::new("child").unwrap(),
        parent_task: None,
        prompt: "Inspect code".into(),
        title: None,
        started_at: at(0),
        completed_at: Some(at(1)),
        model: None,
        status: ItemStatus::Completed,
        result: Some(result.clone()),
        progress: Some(progress.clone()),
        wake: CompletionWake::Always,
        delivery: DeliveryState::Pending,
        generation: 0,
    }];
    let client = crate::sync::client_state(&std::sync::Arc::new(state.clone()));
    assert!(client.tasks[0].result.as_ref().unwrap().ends_with(marker));

    let detail = turn_item(&state, &state.items[0].id).unwrap();
    assert_eq!(detail.row.item, state.items[0]);
    let read = detail.task.unwrap();
    assert_eq!(read.prompt, "Inspect code");
    assert_eq!(read.result, Some(result));
    assert_eq!(
        read.progress,
        Some(format!("{}{marker}", "p".repeat(256 * 1024 - 1)))
    );
    assert_eq!(state.tasks[0].progress, Some(progress));
}

/// ThreadForkService.ts copies subagent items with their prompt, progress and
/// result. A fork, and a fork of that fork, read an inherited subagent item's
/// task as it was at the fork, whatever the source does after it.
#[tokio::test]
async fn a_forks_inherited_subagent_item_reads_its_task_as_it_was_at_the_fork() {
    use crate::sync::history::tests::item;
    use agent_domain::{CompletionWake, ItemStatus, NodeId, RunAttemptId};
    let (_dir, store) = temp_store();
    let task = NodeId::new("child-agent").unwrap();
    let mut source = Writer::create(&store, "thread:source", "project").await;
    source
        .commit(
            1,
            FactBody::TaskStarted {
                original_message: None,
                background: false,
                id: task.clone(),
                native_key: "child-agent".into(),
                run: None,
                attempt: RunAttemptId::new("attempt").unwrap(),
                child: ThreadId::new("thread:child-agent").unwrap(),
                parent: None,
                prompt: "Inspect code".into(),
                model: None,
                wake: CompletionWake::Always,
            },
        )
        .await;
    source
        .commit(
            1,
            FactBody::TaskProgressed {
                id: task.clone(),
                progress: Some("Reading files".into()),
                model: None,
            },
        )
        .await;
    let at_fork = source.state.tasks[0].clone();
    let inherited = item(
        "item-task",
        1,
        ItemKind::Subagent { task: task.clone() },
        String::new(),
    );
    let fork = |parent: &str| FactBody::ForkAccepted {
        parent: ThreadId::new(parent).unwrap(),
        boundary: 1,
        history: vec![inherited.clone()],
        messages: vec![],
        tasks: vec![at_fork.clone()],
    };
    let mut first = Writer::create(&store, "thread:fork", "project").await;
    first.commit(2, fork("thread:source")).await;
    let mut second = Writer::create(&store, "thread:fork-of-fork", "project").await;
    second.commit(3, fork("thread:fork")).await;
    source
        .commit(
            4,
            FactBody::TaskProgressed {
                id: task.clone(),
                progress: Some("Writing the report".into()),
                model: None,
            },
        )
        .await;
    source
        .commit(
            5,
            FactBody::TaskFinished {
                id: task.clone(),
                status: ItemStatus::Completed,
                result: "Found nothing".into(),
            },
        )
        .await;
    let registry = ActorRegistry::new(ActorContext::new(store));
    let now = registry
        .state(&ThreadId::new("thread:source").unwrap())
        .await
        .unwrap();
    assert_eq!(now.tasks[0].result.as_deref(), Some("Found nothing"));
    for thread in ["thread:fork", "thread:fork-of-fork"] {
        let thread = ThreadId::new(thread).unwrap();
        let detail = registry
            .turn_item(&thread, &inherited.id)
            .await
            .unwrap()
            .unwrap();
        assert!(detail.row.inherited);
        assert_eq!(detail.task.unwrap(), at_fork, "{thread}");
        // Clients fold the same frozen tasks for the inherited cards.
        let state = registry.state(&thread).await.unwrap();
        assert_eq!(state.inherited_tasks, std::slice::from_ref(&at_fork));
        assert_eq!(
            crate::sync::client_state(&state).inherited_tasks,
            std::slice::from_ref(&at_fork)
        );
    }
}

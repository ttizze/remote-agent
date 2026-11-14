use super::*;
use agent_protocol::conversation::{
    Command, InputContent, OperationId, Thread, Workspace,
    records::{Details, Header, Replica},
};
use agent_protocol::sync::Scope;
use agent_protocol::{
    operations::{FollowUp, Input as Part},
    session::ProviderKind,
};

fn context(seed: u128) -> Context<'static> {
    Context {
        rewind_context_start: None,
        preparation: None,
        rewind_binding: None,
        imported_turns: &[],
        external_busy: false,
        now: 100,
        seed: Uuid::from_u128(seed),
        steers: true,
        steer_ready: true,
    }
}
fn address(id: &str) -> SessionRef {
    SessionRef::new(ProviderKind::Claude, id.into()).unwrap()
}
fn header() -> Header {
    Header {
        state: Thread {
            context_start: 0,
            name: "Conversation".into(),
            workspace: Workspace::Ready("/project".into()),
            provider: ProviderKind::Claude,
            queue_held: false,
        },
        details: Details::default(),
    }
}
fn command(seed: u128, command: Command) -> Event {
    Event::Command {
        id: OperationId::Client(Uuid::from_u128(seed)),
        receipt: None,
        command,
    }
}
fn submit() -> Event {
    command(
        1,
        Command::Submit {
            id: Uuid::from_u128(1).to_string().into(),
            content: InputContent {
                author: None,
                fresh_context: false,
                parts: vec![Part::Text {
                    text: "hello".into(),
                }],
                model: None,
                effort: None,
                service_tier: None,
                follow_up: FollowUp::Queue,
            },
        },
    )
}

#[tokio::test]
async fn concurrent_replays_publish_one_native_action_after_durable_admission() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Arc::new(Storage::open(&directory.path().join("state.sqlite")));
    let target = address("one");
    storage.create(&target, &header()).unwrap();
    let (owners, mut effects) = Owners::new(storage.clone());
    let mut tasks = tokio::task::JoinSet::new();
    for seed in 1..33 {
        let owners = owners.clone();
        let target = target.clone();
        tasks.spawn(async move {
            owners
                .write(&target, Write::Event(submit(), context(seed), None))
                .await
                .unwrap()
        });
    }
    while let Some(result) = tasks.join_next().await {
        assert_eq!(result.unwrap().0, Outcome::Accepted);
    }
    let effect = effects.recv().await.unwrap();
    assert_eq!(effect.target, target);
    assert!(storage.state(&target).unwrap().permits(&effect.action));
    assert!(matches!(effect.action, Action::Prepare { .. }));
    assert!(effects.try_recv().is_err());
    assert_eq!(storage.state(&target).unwrap().inputs.len(), 1);
}

#[tokio::test]
async fn subscribers_opened_between_other_conversations_commits_keep_independent_cursors() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Arc::new(Storage::open(&directory.path().join("state.sqlite")));
    let target = address("one");
    storage.create(&target, &header()).unwrap();
    let (owners, _effects) = Owners::new(storage.clone());
    let first = owners
        .subscribe(&target, 1, Uuid::new_v4(), None, None, Arc::default())
        .await
        .unwrap();
    let mut first_replica = Replica::new(Scope::Conversation(target.clone()), 1)
        .apply(1, first.initial)
        .unwrap();
    let mut first = first.subscription;
    storage.create(&address("two"), &header()).unwrap();
    let second = owners
        .subscribe(&target, 2, Uuid::new_v4(), None, None, Arc::default())
        .await
        .unwrap();
    let mut second_replica = Replica::new(Scope::Conversation(target.clone()), 1)
        .apply(1, second.initial)
        .unwrap();
    let mut second = second.subscription;
    assert_ne!(first_replica.cursor(), second_replica.cursor());
    owners
        .write(
            &target,
            Write::Event(
                command(3, Command::Rename("renamed".into())),
                context(3),
                None,
            ),
        )
        .await
        .unwrap();
    first_replica = first_replica
        .apply(1, protocol::decode(&first.recv().await.unwrap()).unwrap())
        .unwrap();
    second_replica = second_replica
        .apply(1, protocol::decode(&second.recv().await.unwrap()).unwrap())
        .unwrap();
    assert_eq!(first_replica.rows(), second_replica.rows());
    assert_eq!(first_replica.cursor(), second_replica.cursor());
    owners.disconnect(1);
    assert!(first.recv().await.is_none());
    owners
        .write(
            &target,
            Write::Event(
                command(4, Command::Rename("still connected".into())),
                context(4),
                None,
            ),
        )
        .await
        .unwrap();
    assert!(second.recv().await.is_some());
}

#[tokio::test]
async fn public_input_rows_do_not_expose_native_bindings_or_action_tokens() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Arc::new(Storage::open(&directory.path().join("state.sqlite")));
    let target = address("one");
    storage.create(&target, &header()).unwrap();
    let (owners, mut effects) = Owners::new(storage.clone());
    let mut opened = owners
        .subscribe(&target, 1, Uuid::new_v4(), None, None, Arc::default())
        .await
        .unwrap()
        .subscription;
    owners
        .write(&target, Write::Event(submit(), context(1), None))
        .await
        .unwrap();
    let Action::Prepare { id, action, .. } = effects.recv().await.unwrap().action else {
        panic!("preparation")
    };
    let secret = "private-native-source";
    owners
        .write(
            &target,
            Write::Event(
                Event::Prepared {
                    id,
                    action,
                    cwd: Some("/project".into()),
                    source: Some(secret.into()),
                },
                context(2),
                None,
            ),
        )
        .await
        .unwrap();
    for _ in 0..2 {
        let frame = opened.recv().await.unwrap();
        assert!(
            !frame
                .windows(secret.len())
                .any(|bytes| bytes == secret.as_bytes())
        );
    }
    let effect = effects.recv().await.unwrap();
    assert!(matches!(effect.action, Action::Deliver { .. }));
    assert!(storage.state(&target).unwrap().permits(&effect.action));
}

#[tokio::test]
async fn history_extensions_arrive_on_the_stream_before_later_commits() {
    use agent_protocol::models::{Thread as View, ThreadResponse, Turn, TurnStatus};
    let directory = tempfile::tempdir().unwrap();
    let storage = Arc::new(Storage::open(&directory.path().join("state.sqlite")));
    let target = address("history");
    storage
        .import(
            &target,
            &ThreadResponse {
                thread: View {
                    id: Some(target.clone()),
                    turns: Some(
                        (1..=25)
                            .map(|n| {
                                Arc::new(Turn {
                                    id: format!("t{n}").into(),
                                    status: TurnStatus::Completed,
                                    items: Some(vec![Arc::new(agent_protocol::items::Item::new(
                                        format!("user{n}").into(),
                                        agent_protocol::execution::ItemStatus::Completed,
                                        agent_protocol::items::ItemBody::UserMessage {
                                            text: Some("question".into()),
                                            content: vec![],
                                        },
                                    ))]),
                                    ..Default::default()
                                })
                            })
                            .collect(),
                    ),
                    ..Default::default()
                },
                ..Default::default()
            },
            Some(&target),
            100,
        )
        .unwrap();
    let (owners, _effects) = Owners::new(storage);
    let opened = owners
        .subscribe(&target, 1, Uuid::new_v4(), None, None, Arc::default())
        .await
        .unwrap();
    let mut replica = Replica::new(Scope::Conversation(target.clone()), 1)
        .apply(1, opened.initial)
        .unwrap();
    let mut stream = opened.subscription;
    let _reply = owners.read(&target, 1, stream.id, None).await.unwrap();
    owners
        .write(
            &target,
            Write::Event(
                command(999, Command::Rename("later".into())),
                context(999),
                None,
            ),
        )
        .await
        .unwrap();
    for _ in 0..3 {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), stream.recv())
            .await
            .unwrap()
            .unwrap();
        replica = replica.apply(1, protocol::decode(&frame).unwrap()).unwrap();
    }
    let thread = agent_protocol::conversation::records::project(&replica)
        .unwrap()
        .thread;
    assert_eq!(thread.turns.unwrap().len(), 20);
    assert_eq!(thread.name.as_deref(), Some("later"));
}

#[tokio::test]
async fn subscriptions_share_the_connection_budget_and_release_unread_bytes() {
    fn stream(bytes: Arc<AtomicUsize>) -> (Subscriber, Subscription) {
        let (messages, receiver) = mpsc::channel(QUEUED_MESSAGES);
        let (owner, _input) = mpsc::unbounded_channel();
        let id = Uuid::new_v4();
        let queued_bytes = Arc::new(AtomicUsize::new(0));
        (
            Subscriber {
                session: 1,
                cursor: Cursor {
                    store: Uuid::new_v4(),
                    sequence: 1,
                },
                window: Window { turn: 0, item: 0 },
                messages,
                bytes: bytes.clone(),
                queued_bytes: queued_bytes.clone(),
            },
            Subscription {
                id,
                owner,
                receiver,
            },
        )
    }
    let budget = Arc::new(AtomicUsize::new(0));
    let (mut first, first_read) = stream(budget.clone());
    let (mut second, mut second_read) = stream(budget.clone());
    let other = Arc::new(AtomicUsize::new(0));
    let (mut independent, _independent_read) = stream(other.clone());
    let frame = || Vec::with_capacity(MAX_QUEUED_BYTES / 2 - std::mem::size_of::<Vec<u8>>());
    first.send(frame()).unwrap();
    second.send(frame()).unwrap();
    assert_eq!(budget.load(Relaxed), MAX_QUEUED_BYTES);
    assert!(second.send(vec![1]).is_err());
    independent.send(frame()).unwrap();
    drop(first_read);
    assert_eq!(budget.load(Relaxed), MAX_QUEUED_BYTES / 2);
    assert!(second_read.recv().await.is_some());
    assert_eq!(budget.load(Relaxed), 0);
    assert_eq!(other.load(Relaxed), MAX_QUEUED_BYTES / 2);
    // A frame may still be owned by an in-flight sender after unsubscribe.
    // Its credit follows that frame until it is consumed or discarded.
    let (mut sender, mut receiver) = stream(budget.clone());
    sender.send(vec![1]).unwrap();
    let in_flight = receiver.receiver.try_recv().unwrap();
    drop(receiver);
    assert_eq!(budget.load(Relaxed), frame_cost(in_flight.0.capacity()));
    drop(in_flight);
    assert_eq!(budget.load(Relaxed), 0);
    // One stream must close at its own limit, before the shared 16 MiB cap.
    let (mut over_bytes, mut closed) = stream(budget.clone());
    over_bytes
        .send(Vec::with_capacity(
            STREAM_QUEUED_BYTES - std::mem::size_of::<Vec<u8>>(),
        ))
        .unwrap();
    assert!(over_bytes.send(vec![1]).is_err());
    drop(over_bytes);
    assert!(closed.recv().await.is_some());
    assert!(closed.recv().await.is_none());
    assert_eq!(budget.load(Relaxed), 0);
    let (mut over_frames, mut closed) = stream(budget.clone());
    for _ in 0..QUEUED_MESSAGES {
        over_frames.send(vec![1]).unwrap();
    }
    assert!(over_frames.send(vec![1]).is_err());
    drop(over_frames);
    for _ in 0..QUEUED_MESSAGES {
        assert!(closed.recv().await.is_some());
    }
    assert!(closed.recv().await.is_none());
    assert_eq!(budget.load(Relaxed), 0);
    second.send(vec![2]).unwrap();
    assert_eq!(second_read.recv().await.unwrap(), vec![2]);
    assert_eq!(budget.load(Relaxed), 0);
}

#[tokio::test]
async fn commits_outside_the_conversation_receiver_use_the_same_live_and_replay_events() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Arc::new(Storage::open(&directory.path().join("state.sqlite")));
    let target = address("outside-owner");
    storage.create(&target, &header()).unwrap();
    let (owners, _effects) = Owners::new(storage.clone());
    let opened = owners
        .subscribe(&target, 1, Uuid::new_v4(), None, None, Arc::default())
        .await
        .unwrap();
    let Message::Snapshot { cursor, window, .. } = opened.initial else {
        panic!("initial snapshot")
    };
    let mut stream = opened.subscription;
    storage
        .decide(
            &target,
            command(51, Command::Rename("external commit".into())),
            context(51),
            None,
        )
        .unwrap();
    let frame = tokio::time::timeout(std::time::Duration::from_secs(5), stream.recv())
        .await
        .unwrap()
        .unwrap();
    let live: Message = protocol::decode(&frame).unwrap();
    assert_eq!(
        live,
        storage.snapshot(&target, Some(cursor), window).unwrap()
    );
    assert!(matches!(live,Message::Events {events,..} if events.len()==1));
}

#[tokio::test]
async fn coalesced_live_commits_use_the_reconnect_snapshot_limit() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Arc::new(Storage::open(&directory.path().join("state.sqlite")));
    let target = address("coalesced");
    storage.create(&target, &header()).unwrap();
    let (owners, _effects) = Owners::new(storage.clone());
    let mut stream = owners
        .subscribe(&target, 1, Uuid::new_v4(), None, None, Arc::default())
        .await
        .unwrap()
        .subscription;
    // This current-thread runtime cannot read the wake until these commits end.
    for index in 0..129 {
        storage
            .decide(
                &target,
                command(100 + index, Command::Rename(format!("name-{index}"))),
                context(100 + index),
                None,
            )
            .unwrap();
    }
    let frame = tokio::time::timeout(std::time::Duration::from_secs(5), stream.recv())
        .await
        .unwrap()
        .unwrap();
    let live: Message = protocol::decode(&frame).unwrap();
    assert!(matches!(live, Message::Snapshot { .. }));
    assert_eq!(live, storage.snapshot(&target, None, None).unwrap());
}

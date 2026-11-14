use super::*;
use agent_protocol::conversation::{
    Command, InputContent, OperationId, Thread, Workspace,
    records::{Details, Header},
};
use agent_protocol::{
    operations::{FollowUp, Input as Part},
    session::ProviderKind,
};

fn context(seed: u128) -> Context<'static> {
    Context {
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

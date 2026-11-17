use super::*;
use crate::state::{Draft, DraftKey, WELCOME_DRAFT_SLOT};
use serde::Deserialize;

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
enum Action {
    ReleaseBorrow,
    ReplaceHost,
    RemoveHost,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
enum Slot {
    Original,
    Replacement,
    Absent,
}

#[derive(Debug, Deserialize)]
struct EditResult {
    applied: bool,
    text: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Trace {
    action: Action,
    navigate: bool,
    captured_storage: String,
    view_bound: bool,
    slot: Slot,
    first: EditResult,
    second: EditResult,
}

fn fixture() -> (Arc<Storage>, DraftKey) {
    let storage = Arc::new(Storage::default());
    let welcome = crate::session::SessionRef {
        provider: None,
        id: "welcome-id".into(),
    };
    let key = DraftKey::from(&welcome);
    let mut snapshot = Snapshot::default();
    snapshot.connection.storage_scope = "host".into();
    Arc::make_mut(&mut snapshot.input.new_drafts).insert(WELCOME_DRAFT_SLOT.into(), welcome);
    Arc::make_mut(&mut snapshot.input.drafts).insert(
        key.clone(),
        Arc::new(Draft {
            text: "before".into(),
            ..Default::default()
        }),
    );
    *storage.bytes.lock().unwrap() = crate::persistence::encode(&snapshot).unwrap();
    (storage, key)
}

fn queue_edit(
    session: &ClientSession,
    key: &DraftKey,
    storage: &str,
    bound: bool,
    text: &str,
) -> Arc<Receipt> {
    session
        .dispatch_scoped(
            DispatchScope {
                storage: storage.into(),
                draft: bound.then(|| key.clone()),
            },
            Intent::SetDraftText {
                thread_id: key.clone(),
                text: text.into(),
            },
        )
        .unwrap()
}

async fn replay(trace: Trace) {
    let (storage, key) = fixture();
    let owner = clients();
    let original = owner.open("host".into(), storage.clone(), storage.clone());
    let other_storage = Arc::new(Storage::default());
    let other = owner.open("other".into(), other_storage.clone(), other_storage);
    let borrower = clients();
    borrower.borrow("visible".into(), original.clone());
    let view = borrower.session("visible".into()).unwrap();
    let first = queue_edit(&view, &key, "host", true, "first");
    let navigation = trace.navigate.then(|| {
        view.dispatch(Intent::NewChat {
            cwd: "/elsewhere".into(),
        })
        .unwrap()
    });
    let second = queue_edit(
        &view,
        &key,
        &trace.captured_storage,
        trace.view_bound,
        "second",
    );
    borrower.release();
    drop(borrower);
    match trace.action {
        Action::ReleaseBorrow => {}
        Action::ReplaceHost => owner.adopt("host".into(), open(storage.clone())),
        Action::RemoveHost => assert!(Arc::ptr_eq(&owner.remove("host".into()).unwrap(), &view)),
    }
    let session = owner.session("host".into());
    let slot = match &session {
        None => Slot::Absent,
        Some(session) if Arc::ptr_eq(session, &view) => Slot::Original,
        Some(_) => Slot::Replacement,
    };
    assert_eq!(slot, trace.slot, "{trace:?}");
    assert_eq!(view.clone().ready().await.is_ok(), slot == Slot::Original);
    assert_eq!(first.wait().await.is_ok(), trace.first.applied, "{trace:?}");
    if let Some(navigation) = navigation {
        assert_eq!(navigation.wait().await.is_ok(), slot == Slot::Original);
    }
    assert_eq!(
        second.wait().await.is_ok(),
        trace.second.applied,
        "{trace:?}"
    );
    if let Some(session) = session {
        session.clone().ready().await.unwrap();
        assert_eq!(
            session.snapshot().input.drafts[&key].text,
            trace.second.text,
            "{trace:?}"
        );
    }
    assert!(Arc::ptr_eq(&other, &owner.session("other".into()).unwrap()));
    other.ready().await.unwrap();
    owner.shutdown().await.unwrap();
    let persisted = crate::persistence::decode(&storage.bytes.lock().unwrap()).unwrap();
    assert_eq!(
        persisted.input.drafts[&key].text, trace.second.text,
        "{trace:?}"
    );
}

#[tokio::test]
#[ignore = "requires Lean-generated client.jsonl"]
async fn lean_core_client_conformance() {
    let directory = std::env::var_os("AGENT_CORE_CASES").expect("missing AGENT_CORE_CASES");
    let corpus = std::fs::read_to_string(std::path::PathBuf::from(directory).join("client.jsonl"))
        .expect("read client.jsonl");
    assert_eq!(corpus.lines().count(), 24, "incomplete client corpus");
    for line in corpus.lines() {
        replay(serde_json::from_str(line).expect("invalid Lean client trace")).await;
    }
}

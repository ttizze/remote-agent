use super::*;
use crate::session::{ProviderKind, SessionRef};

fn pending(session: &SessionRef) -> Arc<PendingSubmission> {
    Arc::new(PendingSubmission {
        new_draft_slot: None,
        cursor: None,
        sequence: 0,
        submitted_at_ms: None,
        draft_key: session.into(),
        draft: Arc::default(),
        turn_id: None,
        after_item_id: None,
        accepted: false,
        delivery_unknown: true,
        queued: false,
    })
}
fn session(provider: ProviderKind) -> SessionRef {
    SessionRef::new(provider, "same-address".into()).unwrap()
}
fn canonical(snapshot: &mut Snapshot, id: &SessionRef, inputs: &[&str]) {
    let items = inputs
        .iter()
        .map(|id| {
            Arc::new(crate::models::Item::new(
                (*id).into(),
                Default::default(),
                crate::models::ItemBody::UserMessage {
                    text: None,
                    content: vec![],
                },
            ))
        })
        .collect();
    let replica = crate::test_conversations::projection(
        crate::models::ThreadResponse {
            thread: Thread {
                id: Some(id.clone()),
                turns: Some(vec![Arc::new(crate::models::Turn {
                    id: "turn".into(),
                    items: Some(items),
                    ..Default::default()
                })]),
                ..Default::default()
            },
            ..Default::default()
        },
        1,
    );
    Arc::make_mut(&mut snapshot.sessions.projections).insert(id.clone(), Arc::new(replica));
}

#[test]
fn canonical_inputs_settle_only_their_own_conversation_outbox() {
    let own = session(ProviderKind::Codex);
    let other = session(ProviderKind::Claude);
    let mut state = Snapshot::default();
    Arc::make_mut(&mut state.sessions.pending_submissions).extend([
        ("one".into(), pending(&own)),
        ("two".into(), pending(&own)),
        ("absent".into(), pending(&own)),
        ("foreign".into(), pending(&other)),
    ]);
    let previous = state.sessions.pending_submissions.clone();
    canonical(&mut state, &own, &["one", "two", "foreign"]);
    reconcile_pending(&mut state, &own);
    assert_eq!(state.sessions.pending_submissions.len(), 2);
    for id in ["absent", "foreign"] {
        assert!(Arc::ptr_eq(
            &state.sessions.pending_submissions[id],
            &previous[id]
        ));
    }
    assert_eq!(previous.len(), 4);
}

#[test]
fn unrelated_rows_preserve_the_shared_pending_snapshot() {
    let own = session(ProviderKind::Codex);
    let other = session(ProviderKind::Claude);
    let mut state = Snapshot::default();
    Arc::make_mut(&mut state.sessions.pending_submissions).insert("input".into(), pending(&own));
    let previous = state.sessions.pending_submissions.clone();
    canonical(&mut state, &own, &["unrelated"]);
    canonical(&mut state, &other, &["input"]);
    for id in [&own, &other] {
        reconcile_pending(&mut state, id);
    }
    assert!(Arc::ptr_eq(&previous, &state.sessions.pending_submissions));
    canonical(&mut state, &own, &["input"]);
    reconcile_pending(&mut state, &own);
    assert!(state.sessions.pending_submissions.is_empty());
    assert_eq!(previous.len(), 1);
}

#[test]
fn a_receipt_needs_its_conversations_sequence_coverage_before_retiring_an_input() {
    let own = session(ProviderKind::Codex);
    let other = session(ProviderKind::Claude);
    let mut state = Snapshot::default();
    canonical(&mut state, &own, &[]);
    canonical(&mut state, &other, &[]);
    let current = state.sessions.projections[&own].snapshot_sequence;
    let accepted = current + 1;
    let mut input = pending(&own);
    Arc::make_mut(&mut input).cursor = Some(accepted);
    Arc::make_mut(&mut state.sessions.pending_submissions)
        .extend([("owned".into(), input), ("foreign".into(), pending(&other))]);
    reconcile_pending(&mut state, &own);
    assert_eq!(state.sessions.pending_submissions.len(), 2);
    // Advancing another conversation cannot cover this conversation's receipt.
    Arc::make_mut(
        Arc::make_mut(&mut state.sessions.projections)
            .get_mut(&other)
            .unwrap(),
    )
    .snapshot_sequence = accepted + 10;
    reconcile_pending(&mut state, &own);
    assert!(state.sessions.pending_submissions.contains_key("owned"));
    Arc::make_mut(
        Arc::make_mut(&mut state.sessions.projections)
            .get_mut(&own)
            .unwrap(),
    )
    .snapshot_sequence = accepted;
    reconcile_pending(&mut state, &own);
    assert!(!state.sessions.pending_submissions.contains_key("owned"));
    assert!(state.sessions.pending_submissions.contains_key("foreign"));
}

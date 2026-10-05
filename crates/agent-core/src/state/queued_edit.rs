//! Queue editing shares the ordinary composer without replacing its draft.
use super::*;
use agent_protocol::{
    composer::{Invocation, InvocationKind},
    operations::{Input, Submission},
    queue::QueueEntry,
    session::SubmissionDelivery,
};

impl Draft {
    pub(super) fn submission(
        &self,
        thread_id: crate::session::SessionRef,
        client_user_message_id: agent_protocol::ids::ClientInputId,
    ) -> Submission {
        let mut input = Vec::new();
        if !self.text.is_empty() {
            input.push(Input::Text {
                text: self.text.clone(),
            });
        }
        input.extend(
            self.invocations
                .iter()
                .filter(|item| item.provider == thread_id.provider && item.is_in(&self.text))
                .map(Invocation::input),
        );
        input.extend(self.attachments.iter().map(|attachment| {
            if attachment.is_image {
                Input::LocalImage {
                    path: attachment.path.clone(),
                }
            } else {
                Input::Mention {
                    path: attachment.path.clone(),
                    name: attachment.name.clone(),
                }
            }
        }));
        Submission {
            thread_id,
            client_user_message_id,
            input,
            model: self.model.clone(),
            effort: self.effort.clone(),
            service_tier: self.service_tier.clone(),
        }
    }
}

fn draft_from_submission(input: &Submission) -> Draft {
    let mut draft = Draft {
        model: input.model.clone(),
        effort: input.effort.clone(),
        service_tier: input.service_tier.clone(),
        ..Default::default()
    };
    let mut texts = Vec::new();
    for part in &input.input {
        match part {
            Input::Text { text } => texts.push(text.as_str()),
            Input::Skill { name, path } => draft.invocations.push(Invocation {
                provider: input.thread_id.provider,
                kind: InvocationKind::Skill,
                name: name.clone(),
                path: path.clone(),
            }),
            Input::Mention { name, path }
                if path.starts_with("plugin://") || path.starts_with("app://") =>
            {
                draft.invocations.push(Invocation {
                    provider: input.thread_id.provider,
                    kind: InvocationKind::Plugin,
                    name: name.clone(),
                    path: path.clone(),
                })
            }
            Input::Mention { name, path } => draft.attachments.push(Attachment {
                name: name.clone(),
                path: path.clone(),
                is_image: false,
            }),
            Input::LocalImage { path } => draft.attachments.push(Attachment {
                path: path.clone(),
                name: path
                    .rsplit(['/', '\\'])
                    .next()
                    .filter(|name| !name.is_empty())
                    .unwrap_or("画像")
                    .into(),
                is_image: true,
            }),
        }
    }
    draft.text = texts.join("\n");
    draft
}

impl Snapshot {
    pub(super) fn queue_edit_matches(&self, key: &DraftKey, captured: Option<&Arc<Draft>>) -> bool {
        if !matches!(key, DraftKey::Queued { .. }) {
            return true;
        }
        captured
            .zip(self.queue_edits.get(key))
            .is_some_and(|(captured, current)| Arc::ptr_eq(captured, current))
    }
    pub fn composer_key(&self) -> &DraftKey {
        self.queue_edits.keys().find(|key| matches!(key, DraftKey::Queued { session, .. } if Some(session) == self.navigation.thread_id.as_ref())).unwrap_or(&self.navigation.draft_key)
    }
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ComposerControls {
    pub editing: bool,
    pub send_enabled: bool,
    pub queue_enabled: bool,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    pub fn composer_controls(&self, busy: bool) -> ComposerControls {
        let key = self.composer_key();
        let busy =
            busy || self.operation_running(op::OperationKey::Submission {
                draft_key: key.clone(),
            }) || self.operation_running(op::OperationKey::Dictation {
                draft_key: key.clone(),
            }) || self.operation_running(op::OperationKey::Attachment {
                draft_key: key.clone(),
            });
        let editing = matches!(key, DraftKey::Queued { .. });
        let has_content = self
            .drafts
            .get(key)
            .is_some_and(|draft| !draft.text.trim().is_empty() || !draft.attachments.is_empty());
        let available = self.connected && !busy && has_content;
        let input_available = self
            .navigation
            .thread_id
            .as_ref()
            .is_none_or(|session| self.conversations.contains_key(session));
        ComposerControls {
            editing,
            send_enabled: available && (editing || input_available),
            queue_enabled: available && !editing && self.navigation.thread_id.is_some(),
        }
    }

    pub fn composer_draft_key(&self) -> DraftKey {
        self.composer_key().clone()
    }
    pub fn queue_messages(&self) -> Vec<crate::presentation::conversation::QueueMessage> {
        let editing = self.editing_queue_id();
        self.conversation_thread().map_or_else(Vec::new, |thread| {
            let active = thread.active_turn_id();
            let target = thread.id.as_ref().zip(agent_protocol::queue::steering_turn(
                thread.status,
                active.as_deref(),
                thread.capabilities.unwrap_or_default().active_steering,
            ));
            crate::presentation::conversation::queue_messages(
                &thread.queued_inputs,
                editing.as_deref(),
                target,
            )
        })
    }
    pub fn editing_queue_id(&self) -> Option<String> {
        match self.composer_key() {
            DraftKey::Queued { id, .. } => Some(id.to_string()),
            _ => None,
        }
    }
}

fn editable_entry<'a>(
    inputs: &'a [QueueEntry],
    receipts: &BTreeMap<agent_protocol::ids::ClientInputId, SubmissionDelivery>,
    id: &agent_protocol::ids::ClientInputId,
) -> Option<&'a QueueEntry> {
    if receipts
        .get(id)
        .is_some_and(|delivery| *delivery != SubmissionDelivery::Queued)
    {
        return None;
    }
    inputs.iter().find(|entry| {
        entry.submission.client_user_message_id == *id
            && entry.delivery == SubmissionDelivery::Queued
    })
}

pub(super) fn begin(snapshot: &mut Snapshot, id: agent_protocol::ids::ClientInputId) {
    let Some(session) = snapshot.navigation.thread_id.as_ref() else {
        return;
    };
    let key = DraftKey::Queued {
        session: session.clone(),
        id: id.clone(),
    };
    if snapshot.queue_edits.contains_key(&key) {
        return;
    }
    let Some(entry) = snapshot
        .conversations
        .get(session)
        .and_then(|thread| editable_entry(&thread.queued_inputs, &thread.submissions, &id))
    else {
        snapshot.error = Some("queued input is no longer available".into());
        return;
    };
    let draft = Arc::new(draft_from_submission(&entry.submission));
    let previous = snapshot.composer_key().clone();
    finish(snapshot, &previous, false);
    Arc::make_mut(&mut snapshot.drafts).insert(key.clone(), draft.clone());
    Arc::make_mut(&mut snapshot.queue_edits).insert(key, draft);
    snapshot.error = None;
}

/// A remote start/removal recovers changed user content only into an empty
/// ordinary composer. Its model settings belong to that ordinary draft.
fn recovered(original: &Draft, edited: &Draft, ordinary: &Draft) -> Option<Draft> {
    let dirty = original.text != edited.text
        || original.attachments != edited.attachments
        || original.invocations != edited.invocations;
    if !dirty
        || !ordinary.text.trim().is_empty()
        || !ordinary.attachments.is_empty()
        || !ordinary.invocations.is_empty()
    {
        return None;
    }
    Some(Draft {
        text: edited.text.clone(),
        attachments: edited.attachments.clone(),
        invocations: edited.invocations.clone(),
        ..ordinary.clone()
    })
}

pub(super) fn finish(snapshot: &mut Snapshot, key: &DraftKey, recover: bool) {
    if !snapshot.queue_edits.contains_key(key) {
        return;
    }
    let original = Arc::make_mut(&mut snapshot.queue_edits)
        .remove(key)
        .expect("open queue editor");
    let edited = Arc::make_mut(&mut snapshot.drafts)
        .remove(key)
        .unwrap_or_default();
    if recover && let DraftKey::Queued { session, .. } = key {
        let key = DraftKey::from(session);
        let ordinary = snapshot.drafts.get(&key).cloned().unwrap_or_default();
        if let Some(draft) = recovered(&original, &edited, &ordinary) {
            Arc::make_mut(&mut snapshot.drafts).insert(key, Arc::new(draft));
        }
    }
}

pub(super) fn reconcile(snapshot: &mut Snapshot) {
    let stale: Vec<_> = snapshot
        .queue_edits
        .keys()
        .filter(|key| {
            let DraftKey::Queued { session, id } = key else {
                return false;
            };
            snapshot.conversations.get(session).is_some_and(|thread| {
                editable_entry(&thread.queued_inputs, &thread.submissions, id).is_none()
            })
        })
        .cloned()
        .collect();
    for key in stale {
        finish(snapshot, &key, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::operations::{Operation, SaveQueuedInput};
    use agent_protocol::{
        queue::QueueEntry,
        session::{ProviderKind, SessionRef},
    };

    fn state(text: &str) -> (Snapshot, SessionRef) {
        let session = SessionRef {
            provider: ProviderKind::Codex,
            id: "isolated".into(),
        };
        let original = Draft {
            text: "Use $review and @repo".into(),
            attachments: vec![
                Attachment {
                    path: "/isolated/old.png".into(),
                    name: "old.png".into(),
                    is_image: true,
                },
                Attachment {
                    path: "/isolated/doc.txt".into(),
                    name: "doc.txt".into(),
                    is_image: false,
                },
            ],
            invocations: vec![
                Invocation {
                    provider: session.provider,
                    kind: InvocationKind::Skill,
                    name: "review".into(),
                    path: "/isolated/review/SKILL.md".into(),
                },
                Invocation {
                    provider: session.provider,
                    kind: InvocationKind::Plugin,
                    name: "repo".into(),
                    path: "app://repo".into(),
                },
            ],
            model: Some(crate::models::ModelRef {
                provider: session.provider,
                id: "queue-model".into(),
            }),
            effort: Some("high".into()),
            service_tier: Some("fast".into()),
        };
        let mut snapshot = Snapshot {
            navigation: Arc::new(Navigation {
                thread_id: Some(session.clone()),
                draft_key: session.clone().into(),
                cwd: "/isolated".into(),
            }),
            ..Default::default()
        };
        Arc::make_mut(&mut snapshot.conversations).insert(
            session.clone(),
            Arc::new(crate::models::Thread {
                id: Some(session.clone()),
                queued_inputs: vec![QueueEntry {
                    submission: original.submission(session.clone(), "waiting".into()),
                    delivery: SubmissionDelivery::Queued,
                }],
                ..Default::default()
            }),
        );
        Arc::make_mut(&mut snapshot.drafts).insert(
            session.clone().into(),
            Arc::new(Draft {
                text: text.into(),
                ..Default::default()
            }),
        );
        (snapshot, session)
    }

    #[test]
    fn queued_steering_requires_capability_and_an_active_target_and_preserves_drafts() {
        use agent_protocol::execution::{SessionStatus, TurnStatus};
        let (mut snapshot, session) = state("unsent draft");
        snapshot.connected = true;
        let ordinary = snapshot.drafts.clone();
        let thread = shared_mut(&mut snapshot.conversations, &session).unwrap();
        thread.status = SessionStatus::Running;
        thread.turns = Some(vec![Arc::new(crate::models::Turn {
            id: "active".into(),
            status: TurnStatus::Running,
            ..Default::default()
        })]);
        thread.capabilities = Some(crate::session::Capabilities {
            active_steering: true,
            ..Default::default()
        });
        let steer = snapshot.queue_messages()[0].steer.clone().unwrap();
        assert_eq!(steer.session, session);
        assert_eq!(steer.id.as_str(), "waiting");
        assert_eq!(steer.turn_id.as_str(), "active");
        let (mut next, effects) =
            reduce(&snapshot, Event::Intent(Intent::SteerQueued(steer.clone())));
        assert!(!effects.is_empty());
        assert!(Arc::ptr_eq(&ordinary, &next.drafts));
        assert_eq!(
            next.conversations[&session].queued_inputs,
            snapshot.conversations[&session].queued_inputs
        );
        assert!(steer.apply(&mut next, crate::models::Empty {}).len() == 1);
        assert!(Arc::ptr_eq(&ordinary, &next.drafts));
        let thread = shared_mut(&mut snapshot.conversations, &session).unwrap();
        thread.capabilities.as_mut().unwrap().active_steering = false;
        assert!(snapshot.queue_messages()[0].steer.is_none());
        let thread = shared_mut(&mut snapshot.conversations, &session).unwrap();
        thread.capabilities.as_mut().unwrap().active_steering = true;
        thread.queued_inputs[0].delivery = SubmissionDelivery::Sending;
        assert!(snapshot.queue_messages()[0].steer.is_none());
        let thread = shared_mut(&mut snapshot.conversations, &session).unwrap();
        thread.queued_inputs[0].delivery = SubmissionDelivery::Queued;
        thread.status = SessionStatus::Idle;
        assert!(snapshot.queue_messages()[0].steer.is_none());
    }

    #[test]
    fn normal_composer_editing_keeps_the_queue_and_restores_the_ordinary_draft() {
        let (mut snapshot, session) = state("ordinary draft");
        let ordinary = snapshot.drafts[&DraftKey::from(&session)].clone();
        let queued = snapshot.conversations[&session].queued_inputs.clone();
        begin(&mut snapshot, "waiting".into());
        let key = snapshot.composer_draft_key();
        let draft = snapshot.drafts[&key].clone();
        assert!(snapshot.queue_messages()[0].editing);
        assert_eq!(draft.attachments.len(), 2);
        assert_eq!(draft.invocations.len(), 2);
        assert_eq!(draft.model.as_ref().unwrap().id, "queue-model");
        assert_eq!(draft.effort.as_deref(), Some("high"));
        assert_eq!(draft.service_tier.as_deref(), Some("fast"));
        assert_eq!(
            draft.submission(session.clone(), "waiting".into()),
            queued[0].submission
        );
        set_draft_text(&mut snapshot, key.clone(), "edited".into());
        begin(&mut snapshot, "waiting".into());
        assert_eq!(
            snapshot.drafts[&key].text, "edited",
            "opening the same editor preserves changes"
        );
        let (snapshot, _) = reduce(&snapshot, Event::Intent(Intent::CancelQueueEdit));
        assert_eq!(snapshot.composer_key(), &DraftKey::from(&session));
        assert!(Arc::ptr_eq(
            &ordinary,
            &snapshot.drafts[&DraftKey::from(&session)]
        ));
        assert_eq!(snapshot.conversations[&session].queued_inputs, queued);
        assert!(snapshot.queue_edits.is_empty());
        assert!(!snapshot.queue_messages()[0].editing);
        assert!(!snapshot.drafts.contains_key(&key));
    }

    #[test]
    fn saving_replaces_the_whole_input_and_keeps_the_ordinary_composer_unchanged() {
        let (mut snapshot, session) = state("unsent ordinary draft");
        begin(&mut snapshot, "waiting".into());
        let key = snapshot.composer_draft_key();
        set_draft_text(&mut snapshot, key.clone(), "edited $review".into());
        let draft = shared_mut(&mut snapshot.drafts, &key).unwrap();
        draft.attachments = vec![Attachment {
            path: "/isolated/new.png".into(),
            name: "new.png".into(),
            is_image: true,
        }];
        draft.effort = Some("low".into());
        let input = draft.submission(session.clone(), "waiting".into());
        assert_eq!(input.input.len(), 3);
        assert_eq!(
            input.input[1],
            Input::Skill {
                name: "review".into(),
                path: "/isolated/review/SKILL.md".into()
            }
        );
        assert_eq!(
            input.input[2],
            Input::LocalImage {
                path: "/isolated/new.png".into()
            }
        );
        let (mut snapshot, effects) = reduce(
            &snapshot,
            Event::Intent(Intent::Submit {
                thread_id: None,
                client_user_message_id: "must-not-create-another-message".into(),
            }),
        );
        assert_eq!(effects.len(), 1);
        assert!(snapshot.pending_submissions.is_empty());
        assert_eq!(snapshot.editing_queue_id().as_deref(), Some("waiting"));
        let mut save = SaveQueuedInput {
            submission: input,
            draft_key: key.clone(),
        };
        assert_eq!(
            save.key(),
            Some(op::OperationKey::Submission {
                draft_key: key.clone()
            })
        );
        save.prepare(&mut snapshot).unwrap();
        let captured = save.capture(&snapshot).unwrap();
        save.apply(&mut snapshot, captured);
        assert_eq!(
            snapshot.drafts[&DraftKey::from(&session)].text,
            "unsent ordinary draft"
        );
        assert!(snapshot.editing_queue_id().is_none());
        assert!(!snapshot.drafts.contains_key(&key));
    }

    #[rstest::rstest]
    #[case::claimed(SubmissionDelivery::Sending, "", true, "edited")]
    #[case::removed(SubmissionDelivery::Rejected, "", true, "edited")]
    #[case::ordinary_text(SubmissionDelivery::Sending, "keep mine", true, "keep mine")]
    #[case::clean(SubmissionDelivery::Sending, "", false, "")]
    fn claimed_or_removed_input_recovers_only_changed_content_into_an_empty_composer(
        #[case] delivery: SubmissionDelivery,
        #[case] ordinary: &str,
        #[case] edit: bool,
        #[case] expected: &str,
    ) {
        let (mut snapshot, session) = state(ordinary);
        begin(&mut snapshot, "waiting".into());
        let key = snapshot.composer_draft_key();
        if edit {
            set_draft_text(&mut snapshot, key.clone(), "edited".into());
        }
        shared_mut(&mut snapshot.conversations, &session)
            .unwrap()
            .queued_inputs[0]
            .delivery = delivery;
        reconcile(&mut snapshot);
        assert_eq!(snapshot.drafts[&DraftKey::from(&session)].text, expected);
        assert!(snapshot.queue_edits.is_empty());
        assert!(!snapshot.drafts.contains_key(&key));
        assert_eq!(snapshot.composer_key(), &DraftKey::from(&session));
    }

    #[test]
    fn remote_removal_never_replaces_existing_attachments_or_model_defaults() {
        let (mut snapshot, session) = state("");
        let normal = shared_mut(&mut snapshot.drafts, &DraftKey::from(&session)).unwrap();
        normal.effort = Some("ordinary-effort".into());
        normal.attachments = vec![Attachment {
            path: "/isolated/ordinary.png".into(),
            name: "ordinary.png".into(),
            is_image: true,
        }];
        let ordinary = snapshot.drafts[&DraftKey::from(&session)].clone();
        begin(&mut snapshot, "waiting".into());
        let key = snapshot.composer_draft_key();
        shared_mut(&mut snapshot.drafts, &key)
            .unwrap()
            .attachments
            .clear();
        shared_mut(&mut snapshot.conversations, &session)
            .unwrap()
            .queued_inputs
            .clear();
        reconcile(&mut snapshot);
        assert_eq!(snapshot.drafts[&DraftKey::from(&session)], ordinary);
        let mut empty = ordinary.as_ref().clone();
        empty.attachments.clear();
        let original = Draft {
            text: "old".into(),
            ..Default::default()
        };
        let edited = Draft {
            text: "new".into(),
            effort: Some("queue-effort".into()),
            ..Default::default()
        };
        assert_eq!(
            recovered(&original, &edited, &empty)
                .unwrap()
                .effort
                .as_deref(),
            Some("ordinary-effort")
        );
    }

    #[test]
    fn editor_survives_navigation_restart_and_storage_scope_switches() {
        let (mut snapshot, session) = state("ordinary");
        let (next, _) = reduce(&snapshot, Event::StorageScope("host-a".into()));
        snapshot = next;
        begin(&mut snapshot, "waiting".into());
        let key = snapshot.composer_draft_key();
        set_draft_text(&mut snapshot, key.clone(), "unsaved edit".into());
        let (hidden, _) = reduce(&snapshot, Event::Intent(Intent::ShowThreadList));
        assert!(hidden.editing_queue_id().is_none());
        let mut restored =
            crate::persistence::decode(&crate::persistence::encode(&hidden).unwrap()).unwrap();
        assert!(restored.conversations.is_empty());
        navigate(&mut restored, snapshot.navigation.as_ref().clone());
        reconcile(&mut restored);
        assert_eq!(restored.composer_key(), &key);
        assert_eq!(restored.drafts[&key].text, "unsaved edit");
        let (host_b, _) = reduce(&restored, Event::StorageScope("host-b".into()));
        assert!(host_b.queue_edits.is_empty());
        let (host_a, _) = reduce(&host_b, Event::StorageScope("host-a".into()));
        assert_eq!(host_a.composer_key(), &key);
        assert_eq!(host_a.drafts[&DraftKey::from(&session)].text, "ordinary");
    }

    #[test]
    fn stale_text_attachment_and_save_cannot_reopen_a_finished_editor() {
        let (mut snapshot, session) = state("ordinary");
        begin(&mut snapshot, "waiting".into());
        let key = snapshot.composer_draft_key();
        let mut save = SaveQueuedInput {
            submission: snapshot.drafts[&key].submission(session, "waiting".into()),
            draft_key: key.clone(),
        };
        finish(&mut snapshot, &key, false);
        let (next, _) = reduce(
            &snapshot,
            Event::Intent(Intent::SetDraftText {
                thread_id: key.clone(),
                text: "late text".into(),
            }),
        );
        snapshot = next;
        add_attachment(
            &mut snapshot,
            key.clone(),
            Attachment {
                path: "/isolated/late.png".into(),
                name: "late.png".into(),
                is_image: true,
            },
        );
        assert!(!snapshot.drafts.contains_key(&key));
        assert!(save.prepare(&mut snapshot).is_err());
        assert!(snapshot.error.is_some());
    }

    #[test]
    fn live_receipt_closes_editing_before_the_next_queue_snapshot_arrives() {
        let (mut snapshot, session) = state("");
        begin(&mut snapshot, "waiting".into());
        let key = snapshot.composer_draft_key();
        set_draft_text(&mut snapshot, key.clone(), "keep this edit".into());
        let updated = (crate::session::SessionChange::Submission {
            id: "waiting".into(),
            delivery: SubmissionDelivery::Sending,
        })
        .apply(&snapshot.conversations[&session])
        .unwrap();
        Arc::make_mut(&mut snapshot.conversations).insert(session.clone(), Arc::new(updated));
        reconcile(&mut snapshot);
        assert!(snapshot.editing_queue_id().is_none());
        assert_eq!(
            snapshot.drafts[&DraftKey::from(&session)].text,
            "keep this edit"
        );
        begin(&mut snapshot, "waiting".into());
        assert!(snapshot.editing_queue_id().is_none());
        assert!(snapshot.error.is_some());
    }

    #[test]
    fn saving_keeps_new_changes_made_while_the_host_is_settling_the_edit() {
        let (mut snapshot, session) = state("");
        begin(&mut snapshot, "waiting".into());
        let key = snapshot.composer_draft_key();
        let save = SaveQueuedInput {
            submission: snapshot.drafts[&key].submission(session.clone(), "waiting".into()),
            draft_key: key.clone(),
        };
        set_draft_text(&mut snapshot, key.clone(), "typed during save".into());
        let captured = save.capture(&snapshot).unwrap();
        save.apply(&mut snapshot, captured);
        assert_eq!(
            snapshot.drafts[&DraftKey::from(&session)].text,
            "typed during save"
        );
        assert!(snapshot.editing_queue_id().is_none());
        assert!(!snapshot.drafts.contains_key(&key));
    }

    #[test]
    fn an_older_save_response_cannot_close_a_new_editor_for_the_same_message() {
        let (mut snapshot, session) = state("ordinary");
        begin(&mut snapshot, "waiting".into());
        let key = snapshot.composer_draft_key();
        let save = SaveQueuedInput {
            submission: snapshot.drafts[&key].submission(session, "waiting".into()),
            draft_key: key.clone(),
        };
        let captured = save.capture(&snapshot).unwrap();
        finish(&mut snapshot, &key, false);
        begin(&mut snapshot, "waiting".into());
        set_draft_text(&mut snapshot, key.clone(), "new editor".into());
        save.apply(&mut snapshot, captured);
        assert_eq!(snapshot.editing_queue_id().as_deref(), Some("waiting"));
        assert_eq!(snapshot.drafts[&key].text, "new editor");
    }

    #[test]
    fn recovery_preserves_changed_attachments_and_context_even_without_a_text_edit() {
        let (snapshot, session) = state("");
        let original =
            draft_from_submission(&snapshot.conversations[&session].queued_inputs[0].submission);
        let ordinary = Draft {
            effort: Some("ordinary-effort".into()),
            ..Default::default()
        };
        let mut edited = original.clone();
        edited.attachments.remove(0);
        let recovered = recovered(&original, &edited, &ordinary).unwrap();
        assert_eq!(recovered.text, edited.text);
        assert_eq!(recovered.attachments, edited.attachments);
        assert_eq!(recovered.invocations, edited.invocations);
        assert_eq!(recovered.effort, ordinary.effort);
        edited = original.clone();
        edited.invocations.remove(0);
        let restored = super::recovered(&original, &edited, &ordinary).unwrap();
        assert_eq!(restored.invocations, edited.invocations);
        assert_eq!(restored.attachments, edited.attachments);
        let with_context = Draft {
            invocations: original.invocations.clone(),
            ..ordinary.clone()
        };
        assert!(super::recovered(&original, &edited, &with_context).is_none());
        assert!(super::recovered(&original, &original, &ordinary).is_none());
    }

    #[test]
    fn missing_queue_targets_leave_the_open_editor_and_ordinary_draft_intact() {
        let (mut snapshot, session) = state("ordinary");
        begin(&mut snapshot, "waiting".into());
        let key = snapshot.composer_draft_key();
        set_draft_text(&mut snapshot, key.clone(), "unsaved".into());
        begin(&mut snapshot, "missing".into());
        assert_eq!(snapshot.composer_key(), &key);
        assert_eq!(snapshot.drafts[&key].text, "unsaved");
        assert_eq!(snapshot.drafts[&DraftKey::from(session)].text, "ordinary");
        assert!(snapshot.error.is_some());
    }

    #[rstest::rstest]
    #[case::ordinary(false, false, true, true, true, true)]
    #[case::editing(true, false, true, true, true, false)]
    #[case::queue_only_ordinary(false, false, true, false, true, true)]
    #[case::queue_only_editor(true, false, true, false, true, false)]
    #[case::busy_editor(true, true, true, true, false, false)]
    #[case::offline_editor(true, false, false, true, false, false)]
    fn composer_controls_keep_queue_saving_separate_from_native_execution(
        #[case] editing: bool,
        #[case] busy: bool,
        #[case] connected: bool,
        #[case] active_steering: bool,
        #[case] can_send: bool,
        #[case] can_queue: bool,
    ) {
        let (mut snapshot, session) = state("ordinary");
        snapshot.connected = connected;
        let thread = shared_mut(&mut snapshot.conversations, &session).unwrap();
        thread.turns = Some(vec![Arc::new(crate::models::Turn {
            id: "running".into(),
            status: agent_protocol::execution::TurnStatus::Running,
            ..Default::default()
        })]);
        thread.capabilities = Some(crate::session::Capabilities {
            active_steering,
            ..Default::default()
        });
        if editing {
            begin(&mut snapshot, "waiting".into());
        }
        let controls = snapshot.composer_controls(busy);
        assert_eq!(controls.editing, editing);
        assert_eq!(controls.send_enabled, can_send);
        assert_eq!(controls.queue_enabled, can_queue);
        let key = snapshot.composer_draft_key();
        let draft = shared_mut(&mut snapshot.drafts, &key).unwrap();
        draft.text.clear();
        draft.attachments.clear();
        assert!(!snapshot.composer_controls(false).send_enabled);
        assert!(!snapshot.composer_controls(false).queue_enabled);
    }

    #[test]
    fn late_uploads_and_dictation_do_not_modify_a_reopened_editor() {
        let (mut snapshot, session) = state("");
        begin(&mut snapshot, "waiting".into());
        let key = snapshot.composer_draft_key();
        let upload = op::UploadAttachment {
            draft_key: key.clone(),
            attachment: Attachment {
                path: "/device/local.png".into(),
                name: "local.png".into(),
                is_image: true,
            },
            directory: "/isolated".into(),
        };
        let uploading = upload.capture(&snapshot).unwrap();
        let dictate = op::Dictate {
            draft_key: key.clone(),
            preparation: None,
            audio: vec![],
            send: true,
            client_user_message_id: "late-dictation".into(),
        };
        let dictating = dictate.capture(&snapshot).unwrap();
        finish(&mut snapshot, &key, false);
        begin(&mut snapshot, "waiting".into());
        set_draft_text(&mut snapshot, key.clone(), "reopened editor".into());
        let current = snapshot.drafts[&key].clone();
        upload.apply(&mut snapshot, (uploading, "/isolated/uploaded.png".into()));
        assert_eq!(snapshot.drafts[&key], current);
        assert!(snapshot.error.is_some());
        let effects = dictate.apply(
            &mut snapshot,
            (
                dictating,
                agent_protocol::operations::Transcription {
                    text: "captured speech".into(),
                },
            ),
        );
        assert!(effects.is_empty());
        assert_eq!(snapshot.drafts[&key], current);
        assert_eq!(
            snapshot.drafts[&DraftKey::from(&session)].text,
            "captured speech"
        );
        assert!(snapshot.pending_submissions.is_empty());
        assert_eq!(snapshot.editing_queue_id().as_deref(), Some("waiting"));
    }

    proptest::proptest! {
        #[test]
        fn cancel_never_changes_the_ordinary_draft(ordinary in ".{0,96}", edited in ".{0,96}") {
            let (mut snapshot, session) = state(&ordinary);
            let before = snapshot.drafts[&DraftKey::from(&session)].clone();
            begin(&mut snapshot, "waiting".into());
            let key = snapshot.composer_draft_key();
            set_draft_text(&mut snapshot, key.clone(), edited);
            finish(&mut snapshot, &key, false);
            proptest::prop_assert_eq!(&snapshot.drafts[&DraftKey::from(&session)], &before);
            proptest::prop_assert!(!snapshot.drafts.contains_key(&key));
            proptest::prop_assert!(snapshot.queue_edits.is_empty());
        }
    }
}

use super::*;
use agent_protocol::{
    composer::{Invocation, InvocationKind},
    conversation::{Command, records},
    ids::ClientInputId,
    session::{PreparationStatus, SessionRef},
};

#[derive(Debug)]
struct ResolveSavedInput {
    session: SessionRef,
    input: ClientInputId,
    restore: bool,
    operation: uuid::Uuid,
}

pub(crate) fn reconnect_input_recovery(snapshot: &Snapshot) -> Vec<Effect> {
    snapshot
        .input
        .input_recoveries
        .iter()
        .map(|(input, recovery)| {
            Effect::execute(ResolveSavedInput {
                session: recovery.session.clone(),
                input: input.clone(),
                restore: recovery.restore,
                operation: recovery.operation,
            })
        })
        .collect()
}

pub(crate) fn resolve_saved_input(
    snapshot: &mut Snapshot,
    input: &ClientInputId,
    restore: bool,
) -> Option<Effect> {
    if let Some(recovery) = Arc::make_mut(&mut snapshot.input.input_recoveries).get_mut(input) {
        let recovery = Arc::make_mut(recovery);
        recovery.restore = restore;
        return Some(Effect::execute(ResolveSavedInput {
            session: recovery.session.clone(),
            input: input.clone(),
            restore,
            operation: recovery.operation,
        }));
    }
    let session = snapshot
        .sessions
        .conversations
        .iter()
        .find_map(|(id, thread)| {
            thread
                .preparations
                .iter()
                .any(|preparation| {
                    &preparation.submission.client_user_message_id == input
                        && preparation.status == PreparationStatus::Unknown
                })
                .then(|| id.clone())
        })?;
    let mut operation = ResolveSavedInput {
        session,
        input: input.clone(),
        restore,
        operation: uuid::Uuid::new_v4(),
    };
    operation.prepare(snapshot, "").ok()?;
    Some(Effect::execute(operation))
}

impl ResolveSavedInput {
    fn unresolved<'a>(
        &self,
        snapshot: &'a Snapshot,
    ) -> Result<
        (
            Option<agent_protocol::session::ProviderKind>,
            &'a [agent_protocol::operations::Input],
        ),
        PeerError,
    > {
        let thread = snapshot
            .sessions
            .conversations
            .get(&self.session)
            .ok_or_else(|| PeerError::InvalidMessage("conversation is no longer open".into()))?;
        let input = thread
            .preparations
            .iter()
            .find(|preparation| {
                preparation.submission.client_user_message_id == self.input
                    && preparation.status == PreparationStatus::Unknown
            })
            .ok_or_else(|| PeerError::InvalidMessage("input is no longer unresolved".into()))?;
        Ok((
            thread.active_provider.or(self.session.provider),
            &input.submission.input,
        ))
    }
}

impl Operation for ResolveSavedInput {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::InputRecovery {
            input: self.input.clone(),
        })
    }
    const STALE_POLICY: StalePolicy = StalePolicy::Apply;
    fn navigation_bound(&self) -> bool {
        false
    }
    fn prepare(&mut self, snapshot: &mut Snapshot, pane: &str) -> Result<(), String> {
        if self.restore {
            if let Some(recovery) = snapshot.input.input_recoveries.get(&self.input) {
                self.operation = recovery.operation;
            } else {
                let (_, store) = self
                    .capture(snapshot, pane)
                    .map_err(|error| error.to_string())?;
                let (provider, input) = self
                    .unresolved(snapshot)
                    .map_err(|error| error.to_string())?;
                let provider =
                    provider.ok_or_else(|| "conversation provider is missing".to_string())?;
                let draft = recover_draft(input, provider);
                Arc::make_mut(&mut snapshot.input.input_recoveries).insert(
                    self.input.clone(),
                    Arc::new(InputRecovery {
                        restore: true,
                        store,
                        session: self.session.clone(),
                        operation: self.operation,
                        draft: Arc::new(draft),
                    }),
                );
            }
        }
        Ok(())
    }
    type Input = (String, uuid::Uuid);
    fn capture(&self, snapshot: &Snapshot, _pane: &str) -> Result<Self::Input, PeerError> {
        if let Some(recovery) = snapshot.input.input_recoveries.get(&self.input) {
            return Ok((snapshot.connection.storage_scope.clone(), recovery.store));
        }
        let store = snapshot
            .sessions
            .replicas
            .get(&self.session)
            .and_then(|replica| replica.cursor())
            .map(|cursor| cursor.store)
            .ok_or_else(|| {
                PeerError::InvalidMessage("conversation store identity is missing".into())
            })?;
        self.unresolved(snapshot)?;
        Ok((snapshot.connection.storage_scope.clone(), store))
    }

    type Output = ((String, uuid::Uuid), records::Acknowledged);
    async fn run(
        &self,
        recovered: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        let acknowledged = context
            .call(&records::Change {
                session: self.session.clone(),
                operation: self.operation,
                // Restoring returns input to the composer; sending it remains an
                // explicit new action, after the old delivery has been fenced.
                command: Command::ResolveUnknown {
                    id: self.input.clone(),
                    retry: false,
                },
            })
            .await?;
        Ok((recovered, acknowledged))
    }
    fn complete(
        self,
        snapshot: &mut Snapshot,
        _pane: &str,
        ((scope, store), acknowledged): Self::Output,
        _current: bool,
    ) -> Result<Vec<Effect>, PeerError> {
        if snapshot.connection.storage_scope != scope {
            return Ok(Vec::new());
        }
        if acknowledged.cursor.store != store {
            return Err(PeerError::InvalidMessage(
                "conversation store changed during input recovery".into(),
            ));
        }
        if self.restore && !snapshot.input.input_recoveries.contains_key(&self.input) {
            return Ok(Vec::new());
        }
        if let Some(recovery) = snapshot.input.input_recoveries.get(&self.input)
            && recovery.operation != self.operation
        {
            return Ok(Vec::new());
        }
        let recovered =
            match Arc::make_mut(&mut snapshot.input.input_recoveries).remove(&self.input) {
                Some(saved) => saved.restore.then(|| (*saved.draft).clone()),
                None => None,
            };
        crate::client::command_result(acknowledged)?;
        if let Some(recovered) = recovered {
            let current = Arc::make_mut(&mut snapshot.input.drafts)
                .entry(self.session.into())
                .or_default();
            *current = Arc::new(super::super::super::merge_recovered_draft(
                &recovered, current,
            ));
        }
        Ok(Vec::new())
    }
}

#[derive(Debug)]
struct ResolveLocalInput {
    input: ClientInputId,
    restore: bool,
}

pub(crate) fn resolve_local_input(
    snapshot: &Snapshot,
    input: &ClientInputId,
    restore: bool,
) -> Option<Effect> {
    snapshot
        .sessions
        .pending_submissions
        .get(input)
        .filter(|pending| pending.delivery_unknown)
        .map(|_| {
            Effect::execute(ResolveLocalInput {
                input: input.clone(),
                restore,
            })
        })
}

impl Operation for ResolveLocalInput {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::InputRecovery {
            input: self.input.clone(),
        })
    }
    const STALE_POLICY: StalePolicy = StalePolicy::Apply;
    fn navigation_bound(&self) -> bool {
        false
    }
    type Input = (String, SessionRef);
    fn capture(&self, snapshot: &Snapshot, _pane: &str) -> Result<Self::Input, PeerError> {
        let pending = snapshot
            .sessions
            .pending_submissions
            .get(&self.input)
            .filter(|pending| pending.delivery_unknown)
            .ok_or_else(|| PeerError::InvalidMessage("input is no longer unresolved".into()))?;
        let DraftKey::Session { session } = &pending.draft_key else {
            return Err(PeerError::InvalidMessage(
                "input has no conversation address".into(),
            ));
        };
        Ok((snapshot.connection.storage_scope.clone(), session.clone()))
    }
    type Output = (Self::Input, records::Acknowledged);
    async fn run(
        &self,
        input: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        // The initial submission uses its input UUID as the command receipt.
        // Reusing that receipt either returns the original outcome or records
        // rejection before a delayed original command can reach the Host.
        let operation = uuid::Uuid::parse_str(self.input.as_str())
            .map_err(|error| PeerError::InvalidMessage(error.to_string()))?;
        let result = context
            .call(&records::Change {
                session: input.1.clone(),
                operation,
                command: Command::ResolveUnknown {
                    id: self.input.clone(),
                    retry: false,
                },
            })
            .await?;
        Ok((input, result))
    }
    fn apply(
        self,
        snapshot: &mut Snapshot,
        _pane: &str,
        ((scope, session), reply): Self::Output,
    ) -> Vec<Effect> {
        if snapshot.connection.storage_scope != scope
            || !snapshot
                .sessions
                .pending_submissions
                .contains_key(&self.input)
        {
            return Vec::new();
        }
        match reply.outcome {
            agent_protocol::conversation::Outcome::Rejected(_) => {
                if self.restore {
                    *snapshot = super::super::super::reduce_event(
                        snapshot,
                        Event::SubmissionFailed(self.input),
                    )
                    .0;
                } else {
                    Arc::make_mut(&mut snapshot.sessions.pending_submissions).remove(&self.input);
                }
                Vec::new()
            }
            agent_protocol::conversation::Outcome::Accepted
            | agent_protocol::conversation::Outcome::Ignored => {
                if let Some(pending) =
                    shared_mut(&mut snapshot.sessions.pending_submissions, &self.input)
                {
                    pending.cursor = Some(reply.cursor);
                    pending.accepted = true;
                    pending.delivery_unknown = false;
                }
                vec![Effect::execute(ReadThread::new(session))]
            }
            agent_protocol::conversation::Outcome::DeliveryUnknown => Vec::new(),
        }
    }
}

fn recover_draft(
    input: &[agent_protocol::operations::Input],
    provider: crate::session::ProviderKind,
) -> Draft {
    use agent_protocol::operations::Input;
    let mut draft = Draft::default();
    for part in input {
        match part {
            Input::Text { text } => append_transcript(&mut draft.text, text),
            Input::Skill { name, path } => draft.invocations.push(Invocation {
                provider,
                kind: InvocationKind::Skill,
                name: name.clone(),
                path: path.clone(),
            }),
            Input::Mention { name, path }
                if path.starts_with("plugin://") || path.starts_with("app://") =>
            {
                draft.invocations.push(Invocation {
                    provider,
                    kind: InvocationKind::Plugin,
                    name: name.clone(),
                    path: path.clone(),
                });
            }
            Input::Mention { name, path } => draft.attachments.push(Attachment {
                path: path.clone(),
                name: name.clone(),
                is_image: false,
            }),
            Input::LocalImage { path } => draft.attachments.push(Attachment {
                name: path.rsplit(['/', '\\']).next().unwrap_or(path).into(),
                path: path.clone(),
                is_image: true,
            }),
        }
    }
    draft
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::{
        operations::{FollowUp, Input, Submission},
        session::{PreparationStage, ProviderKind, SessionPreparation},
    };

    #[rstest::rstest]
    fn a_restore_survives_a_lost_reply_and_restart_without_duplicating_the_draft(
        #[values(false, true)] discard: bool,
    ) {
        let session = SessionRef::new(None, uuid::Uuid::new_v4().to_string()).unwrap();
        let input: ClientInputId = uuid::Uuid::new_v4().to_string().into();
        let operation = uuid::Uuid::new_v4();
        let mut snapshot = Snapshot::default();
        snapshot.connection.storage_scope = "host-area".into();
        Arc::make_mut(&mut snapshot.input.input_recoveries).insert(
            input.clone(),
            Arc::new(InputRecovery {
                restore: true,
                store: uuid::Uuid::from_u128(1),
                session: session.clone(),
                operation,
                draft: Arc::new(Draft {
                    text: "unresolved text".into(),
                    ..Default::default()
                }),
            }),
        );
        Arc::make_mut(&mut snapshot.input.drafts).insert(
            session.clone().into(),
            Arc::new(Draft {
                text: "new typing".into(),
                ..Default::default()
            }),
        );
        // The Host removed its unresolved row, but its reply was lost before
        // the device saved completion. Reopening must retain the restore.
        let mut reopened =
            crate::persistence::decode(&crate::persistence::encode(&snapshot).unwrap()).unwrap();
        assert!(reopened.sessions.conversations.is_empty());
        assert!(resolve_saved_input(&mut reopened, &input, true).is_some());
        assert_eq!(reconnect_input_recovery(&reopened).len(), 1);
        let make_operation = || ResolveSavedInput {
            session: session.clone(),
            input: input.clone(),
            restore: true,
            operation,
        };
        let mut replay = make_operation();
        replay.prepare(&mut reopened, "").unwrap();
        assert_eq!(
            replay.operation, operation,
            "a replay must use the original durable receipt"
        );
        let captured = replay.capture(&reopened, "").unwrap();
        let acknowledged = records::Acknowledged {
            outcome: agent_protocol::conversation::Outcome::Accepted,
            cursor: agent_protocol::sync::Cursor {
                store: uuid::Uuid::from_u128(1),
                sequence: 12,
            },
        };
        let mut wrong_store = acknowledged.clone();
        wrong_store.cursor.store = uuid::Uuid::new_v4();
        assert!(
            make_operation()
                .complete(
                    &mut reopened,
                    "",
                    ((captured.0.clone(), captured.1), wrong_store),
                    true
                )
                .is_err()
        );
        assert!(reopened.input.input_recoveries.contains_key(&input));
        if discard {
            assert!(resolve_saved_input(&mut reopened, &input, false).is_some());
            reopened = crate::persistence::decode(&crate::persistence::encode(&reopened).unwrap())
                .unwrap();
        }
        replay
            .complete(
                &mut reopened,
                "",
                ((captured.0.clone(), captured.1), acknowledged.clone()),
                true,
            )
            .unwrap();
        make_operation()
            .complete(
                &mut reopened,
                "",
                ((captured.0, captured.1), acknowledged),
                true,
            )
            .unwrap();
        assert_eq!(
            reopened.input.drafts[&DraftKey::from(&session)].text,
            if discard {
                "new typing"
            } else {
                "unresolved text\nnew typing"
            }
        );
        assert!(reconnect_input_recovery(&reopened).is_empty());
    }

    #[rstest::rstest]
    fn local_recovery_restores_only_after_the_original_submission_is_fenced(
        #[values(true, false)] accepted: bool,
        #[values(true, false)] restore: bool,
    ) {
        let session = SessionRef::new(None, uuid::Uuid::new_v4().to_string()).unwrap();
        let input: ClientInputId = uuid::Uuid::new_v4().to_string().into();
        let key = DraftKey::from(&session);
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut snapshot.input.drafts).insert(
            key.clone(),
            Arc::new(Draft {
                text: "new typing".into(),
                ..Default::default()
            }),
        );
        Arc::make_mut(&mut snapshot.sessions.pending_submissions).insert(
            input.clone(),
            Arc::new(PendingSubmission {
                new_draft_slot: None,
                draft_key: key.clone(),
                draft: Arc::new(Draft {
                    text: "uncertain input".into(),
                    ..Default::default()
                }),
                delivery_unknown: true,
                cursor: None,
                sequence: 0,
                submitted_at_ms: None,
                turn_id: None,
                after_item_id: None,
                accepted: false,
                queued: false,
            }),
        );
        let operation = ResolveLocalInput {
            input: input.clone(),
            restore,
        };
        let captured = operation.capture(&snapshot, "").unwrap();
        let effects = operation.apply(
            &mut snapshot,
            "",
            (
                captured,
                records::Acknowledged {
                    outcome: if accepted {
                        agent_protocol::conversation::Outcome::Accepted
                    } else {
                        agent_protocol::conversation::Outcome::Rejected("not sent".into())
                    },
                    cursor: agent_protocol::sync::Cursor {
                        store: uuid::Uuid::new_v4(),
                        sequence: 10,
                    },
                },
            ),
        );
        assert_eq!(
            snapshot.input.drafts[&key].text,
            if !accepted && restore {
                "uncertain input\nnew typing"
            } else {
                "new typing"
            }
        );
        assert_eq!(
            snapshot.sessions.pending_submissions.contains_key(&input),
            accepted
        );
        assert_eq!(effects.len(), usize::from(accepted));
    }

    #[rstest::rstest]
    fn saved_input_recovery_preserves_new_edits_and_never_resends(
        #[values(true, false)] restore: bool,
    ) {
        let session = SessionRef::new(None, uuid::Uuid::new_v4().to_string()).unwrap();
        let input: ClientInputId = uuid::Uuid::new_v4().to_string().into();
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut snapshot.sessions.conversations).insert(
            session.clone(),
            Arc::new(crate::models::Thread {
                id: Some(session.clone()),
                active_provider: Some(ProviderKind::Codex),
                preparations: vec![SessionPreparation {
                    submission: Submission {
                        thread_id: session.clone(),
                        client_user_message_id: input.clone(),
                        input: vec![
                            Input::Text {
                                text: "unresolved text".into(),
                            },
                            Input::LocalImage {
                                path: "C:\\images\\one.png".into(),
                            },
                            Input::Mention {
                                name: "file.rs".into(),
                                path: "/project/file.rs".into(),
                            },
                            Input::Skill {
                                name: "check".into(),
                                path: "/skills/check".into(),
                            },
                            Input::Mention {
                                name: "tool".into(),
                                path: "plugin://tool".into(),
                            },
                        ],
                        model: None,
                        effort: None,
                        service_tier: None,
                        follow_up: FollowUp::Queue,
                    },
                    status: PreparationStatus::Unknown,
                    stage: PreparationStage::Provider,
                }],
                ..Default::default()
            }),
        );
        let thread = snapshot.sessions.conversations[&session].as_ref().clone();
        crate::test_conversations::open(
            &mut snapshot,
            crate::models::ThreadResponse {
                thread,
                ..Default::default()
            },
        );
        let intent = if restore {
            Intent::RestoreUnknownSubmission {
                client_user_message_id: input.clone(),
            }
        } else {
            Intent::DiscardUnknownSubmission {
                client_user_message_id: input.clone(),
            }
        };
        let (_, effects) = super::super::super::super::reduce(&snapshot, "", Event::Intent(intent));
        assert_eq!(
            effects.len(),
            1,
            "a second device must resolve the saved input on the Host"
        );
        let mut operation = ResolveSavedInput {
            session: session.clone(),
            input: input.clone(),
            restore,
            operation: uuid::Uuid::new_v4(),
        };
        operation.prepare(&mut snapshot, "").unwrap();
        let captured = operation.capture(&snapshot, "").unwrap();
        let key = DraftKey::from(&session);
        Arc::make_mut(&mut snapshot.input.drafts).insert(
            key.clone(),
            Arc::new(Draft {
                text: "newer typing".into(),
                ..Default::default()
            }),
        );
        assert!(
            operation
                .complete(
                    &mut snapshot,
                    "",
                    (
                        (captured.0, captured.1),
                        records::Acknowledged {
                            outcome: agent_protocol::conversation::Outcome::Accepted,
                            cursor: agent_protocol::sync::Cursor {
                                store: uuid::Uuid::from_u128(1),
                                sequence: 1
                            },
                        }
                    ),
                    true
                )
                .unwrap()
                .is_empty(),
            "restoration never submits automatically"
        );
        let draft = &snapshot.input.drafts[&key];
        assert_eq!(
            draft.text,
            if restore {
                "unresolved text\nnewer typing"
            } else {
                "newer typing"
            }
        );
        assert_eq!(draft.attachments.len(), if restore { 2 } else { 0 });
        assert_eq!(draft.invocations.len(), if restore { 2 } else { 0 });
        if restore {
            assert_eq!(draft.attachments[0].name, "one.png");
        }
        assert!(snapshot.sessions.pending_submissions.is_empty());
        let mut settled = snapshot.clone();
        Arc::make_mut(
            Arc::make_mut(&mut settled.sessions.conversations)
                .get_mut(&session)
                .unwrap(),
        )
        .preparations
        .clear();
        assert!(resolve_saved_input(&mut settled, &input, restore).is_none());
        assert!(
            ResolveSavedInput {
                session,
                input,
                restore,
                operation: uuid::Uuid::new_v4(),
            }
            .capture(&settled, "")
            .is_err()
        );
    }
}

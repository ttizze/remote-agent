mod recovery;
use super::{sessions::open_thread, *};
use crate::client::ClientExt;
pub(crate) use recovery::{reconnect_input_recovery, resolve_local_input, resolve_saved_input};

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dictate {
    pub draft_key: DraftKey,
    pub preparation: Option<String>,
    pub audio: Vec<u8>,
    pub send: bool,
    pub client_user_message_id: agent_protocol::ids::ClientInputId,
}

impl Operation for Dictate {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Dictation {
            draft_key: self.draft_key.clone(),
        })
    }
    type Input = Arc<Draft>;
    fn capture(&self, snapshot: &Snapshot, _pane_id: &str) -> Result<Self::Input, PeerError> {
        Ok(snapshot
            .input
            .drafts
            .get(&self.draft_key)
            .cloned()
            .unwrap_or_default())
    }
    type Output = (Arc<Draft>, rpc::Transcription);
    async fn run(
        &self,
        draft: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        Ok((
            draft,
            context
                .call(&rpc::Transcribe {
                    preparation: self.preparation.clone(),
                    audio: &self.audio,
                })
                .await?,
        ))
    }
    fn apply(self, snapshot: &mut Snapshot, pane_id: &str, output: Self::Output) -> Vec<Effect> {
        if output.1.text.trim().is_empty() {
            return Vec::new();
        }
        if !self.send {
            return self.stale(snapshot, pane_id, output);
        }
        let (mut draft, output) = output;
        let clear_draft = draft.clone();
        append_transcript(&mut Arc::make_mut(&mut draft).text, &output.text);
        let pane = snapshot.panes.get(pane_id);
        let destination = Destination {
            thread_id: pane.and_then(|pane| pane.navigation.thread_id.clone()),
            cwd: pane
                .map(|pane| pane.navigation.cwd.clone())
                .filter(|cwd| !cwd.trim().is_empty()),
            draft_key: self.draft_key.clone(),
        };
        let (next, effects) = submission(
            snapshot,
            destination,
            draft,
            self.client_user_message_id.clone(),
            Some(clear_draft.clone()),
            snapshot.input.composer_preferences.follow_up,
        );
        *snapshot = next;
        if effects.is_empty() {
            return self.stale(snapshot, pane_id, (clear_draft, output));
        }
        effects
    }
    fn stale(
        self,
        snapshot: &mut Snapshot,
        pane_id: &str,
        (_, output): Self::Output,
    ) -> Vec<Effect> {
        if snapshot.panes.get(pane_id).is_none() || output.text.trim().is_empty() {
            return Vec::new();
        }
        let draft = Arc::make_mut(
            Arc::make_mut(&mut snapshot.input.drafts)
                .entry(self.draft_key)
                .or_default(),
        );
        append_transcript(&mut draft.text, &output.text);
        Vec::new()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Respond {
    pub request_id: agent_protocol::ids::RequestId,
    pub answer: Answer,
}
impl Operation for Respond {
    fn scheduling(&self) -> Scheduling {
        Scheduling::Control
    }

    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Request {
            id: self.request_id.clone(),
        })
    }
    type Input = (crate::session::SessionRef, Arc<Request>);
    fn capture(&self, snapshot: &Snapshot, _pane_id: &str) -> Result<Self::Input, PeerError> {
        snapshot
            .sessions
            .conversations
            .iter()
            .find_map(|(id, thread)| {
                thread
                    .requests
                    .get(&self.request_id)
                    .map(|request| (id.clone(), request.clone()))
            })
            .ok_or_else(|| PeerError::InvalidMessage("server request is no longer pending".into()))
    }
    type Output = ();
    const STALE_POLICY: StalePolicy = StalePolicy::Apply;
    async fn run(
        &self,
        (session, request): Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        context
            .client
            .respond(&session, &request, &self.answer)
            .await
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartSubmission {
    pub thread_id: crate::session::SessionRef,
    pub provider: crate::session::ProviderKind,
    pub cwd: Option<String>,
    pub client_user_message_id: agent_protocol::ids::ClientInputId,
    pub draft: Arc<Draft>,
}
impl Operation for StartSubmission {
    fn subscription_scope(
        &self,
    ) -> Option<agent_protocol::sync::Scope<crate::session::SessionRef>> {
        Some(agent_protocol::sync::Scope::Conversation(
            self.thread_id.clone(),
        ))
    }
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Submission {
            draft_key: self.thread_id.clone().into(),
        })
    }
    type Input = Arc<agent_protocol::conversation::records::Replica>;
    fn capture(&self, snapshot: &Snapshot, _pane_id: &str) -> Result<Self::Input, PeerError> {
        snapshot
            .sessions
            .replicas
            .get(&self.thread_id)
            .cloned()
            .ok_or_else(|| {
                PeerError::InvalidMessage("conversation creation was not scheduled".into())
            })
    }
    fn submission_id(&self) -> Option<&str> {
        Some(&self.client_user_message_id)
    }
    type Output = super::super::conversations::Opened;
    fn stale(self, snapshot: &mut Snapshot, _pane_id: &str, output: Self::Output) -> Vec<Effect> {
        super::super::conversations::install(snapshot, output);
        Vec::new()
    }
    fn outcome(output: &mut Self::Output) -> Outcome {
        Outcome::StartedThread {
            id: match output.replica.scope() {
                agent_protocol::sync::Scope::Conversation(id) => id.clone(),
                _ => unreachable!("conversation operation"),
            },
        }
    }
    async fn run(
        &self,
        replica: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        let response = context
            .call(&agent_protocol::conversation::records::Create {
                provider: self.provider,
                session: self.thread_id.clone(),
                subscription: uuid::Uuid::nil(),
                cwd: self.cwd.clone(),
                workspace: self.draft.workspace.clone(),
                selection: agent_protocol::conversation::records::Selection {
                    model: self.draft.model.clone(),
                    effort: self.draft.effort.clone(),
                    service_tier: self.draft.service_tier.clone(),
                },
                initial: Some((
                    self.client_user_message_id.clone(),
                    agent_protocol::conversation::InputContent {
                        parts: submission_input(
                            &self.draft.text,
                            &self.draft.attachments,
                            &self.draft.invocations,
                            Some(self.provider),
                        ),
                        model: self.draft.model.clone(),
                        effort: self.draft.effort.clone(),
                        service_tier: self.draft.service_tier.clone(),
                        follow_up: Default::default(),
                    },
                )),
            })
            .await?;
        super::super::conversations::opened(&replica, response)
    }
    fn apply(self, snapshot: &mut Snapshot, pane_id: &str, output: Self::Output) -> Vec<Effect> {
        let Some(response) = super::super::conversations::install(snapshot, output) else {
            return Vec::new();
        };
        open_thread(
            snapshot,
            pane_id,
            self.thread_id,
            response.thread.cwd.unwrap_or_default(),
            None,
            None,
            None,
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendSubmission {
    pub thread_id: crate::session::SessionRef,
    pub client_user_message_id: agent_protocol::ids::ClientInputId,
    pub draft: Arc<Draft>,
    pub follow_up: agent_protocol::operations::FollowUp,
}
impl Operation for SendSubmission {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Submission {
            draft_key: self.thread_id.clone().into(),
        })
    }
    type Input = Option<crate::session::ProviderKind>;
    fn capture(&self, snapshot: &Snapshot, _pane_id: &str) -> Result<Self::Input, PeerError> {
        Ok(snapshot.draft_provider(&self.thread_id.clone().into(), self.draft.model.as_ref()))
    }
    const STALE_POLICY: StalePolicy = StalePolicy::Apply;
    fn submission_id(&self) -> Option<&str> {
        Some(&self.client_user_message_id)
    }
    type Output = agent_protocol::sync::Cursor;
    fn outcome(_: &mut Self::Output) -> Outcome {
        Outcome::Submitted
    }
    async fn run(
        &self,
        provider: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        let operation = uuid::Uuid::parse_str(self.client_user_message_id.as_str())
            .map_err(|_| PeerError::InvalidMessage("input ID is not a UUID".into()))?;
        let reply = context
            .call(&agent_protocol::conversation::records::Change {
                session: self.thread_id.clone(),
                operation,
                command: agent_protocol::conversation::Command::Submit {
                    id: self.client_user_message_id.clone(),
                    content: agent_protocol::conversation::InputContent {
                        parts: submission_input(
                            &self.draft.text,
                            &self.draft.attachments,
                            &self.draft.invocations,
                            provider,
                        ),
                        model: self.draft.model.clone(),
                        effort: self.draft.effort.clone(),
                        service_tier: self.draft.service_tier.clone(),
                        follow_up: self.follow_up,
                    },
                },
            })
            .await?;
        crate::client::command_result(reply)
    }
    fn apply(self, snapshot: &mut Snapshot, _pane_id: &str, output: Self::Output) -> Vec<Effect> {
        if let Some(pending) = shared_mut(
            &mut snapshot.sessions.pending_submissions,
            &self.client_user_message_id,
        ) {
            pending.accepted = true;
            pending.delivery_unknown = false;
            pending.cursor = Some(output);
        }
        reconcile_pending(snapshot, &self.thread_id);
        if !snapshot
            .sessions
            .subscriptions
            .contains_key(&self.thread_id)
        {
            vec![Effect::execute(ReadThread::new(self.thread_id))]
        } else {
            Vec::new()
        }
    }
}

fn submission_input(
    text: &str,
    attachments: &[Attachment],
    invocations: &[agent_protocol::composer::Invocation],
    provider: Option<crate::session::ProviderKind>,
) -> Vec<Input> {
    let mut input = Vec::with_capacity(attachments.len() + usize::from(!text.is_empty()));
    if !text.is_empty() {
        input.push(Input::Text { text: text.into() });
    }
    input.extend(
        invocations
            .iter()
            .filter(|item| Some(item.provider) == provider && item.is_in(text))
            .map(agent_protocol::composer::Invocation::input),
    );
    input.extend(attachments.iter().map(|attachment| {
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
    input
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadAttachment {
    pub draft_key: DraftKey,
    pub attachment: Attachment,
}

impl Operation for UploadAttachment {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Attachment {
            draft_key: self.draft_key.clone(),
        })
    }
    no_input!();
    type Output = String;
    const STALE_POLICY: StalePolicy = StalePolicy::Apply;
    async fn run(
        &self,
        _: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        let session = context.session.ok_or_else(|| {
            PeerError::InvalidMessage("binary transfers require an iroh session".into())
        })?;
        let uploaded = crate::transfers::upload_file(
            context.client,
            || async { session.open_stream().await.map_err(std::io::Error::other) },
            std::path::Path::new(&self.attachment.path),
            &self.attachment.name,
        )
        .await
        .map_err(PeerError::from)?;
        Ok(uploaded.path)
    }
    fn apply(mut self, snapshot: &mut Snapshot, _pane_id: &str, path: Self::Output) -> Vec<Effect> {
        self.attachment.path = path;
        Arc::make_mut(
            Arc::make_mut(&mut snapshot.input.drafts)
                .entry(self.draft_key)
                .or_default(),
        )
        .attachments
        .push(self.attachment);
        Vec::new()
    }
}

#[cfg(test)]
mod startup_identity_tests {
    use super::*;
    #[rstest::rstest]
    fn completed_startup_keeps_the_panel_identity_after_pending_input_is_gone(
        #[values(true, false)] current_view: bool,
    ) {
        let session = crate::session::SessionRef {
            provider: None,
            id: uuid::Uuid::new_v4().to_string(),
        };
        let draft_key = DraftKey::from(&session);
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut Arc::make_mut(snapshot.panes.primary_mut()).navigation).draft_key =
            if current_view {
                draft_key.clone()
            } else {
                DraftKey::from("other")
            };
        let navigation = snapshot.panes.primary().navigation.clone();
        let input = uuid::Uuid::from_u128(1).to_string();
        Arc::make_mut(&mut snapshot.input.new_drafts).insert("0:/project".into(), session.clone());
        Arc::make_mut(&mut snapshot.sessions.pending_submissions).insert(
            input.clone().into(),
            Arc::new(PendingSubmission {
                cursor: None,
                sequence: 1,
                submitted_at_ms: None,
                draft_key: draft_key.clone(),
                draft: Arc::default(),
                turn_id: None,
                after_item_id: None,
                accepted: false,
                delivery_unknown: false,
                queued: false,
            }),
        );
        let thread: Thread = serde_json::from_value(serde_json::json!({"id":session, "activeProvider":"codex", "cwd":"/project", "turns":[{"id":"first", "status":"completed", "items":[{"id":input,"status":"completed","clientInputId":input,"body":{"inline":{"body":{"userMessage":{"text":"first","content":[]}}}}}]}]})).unwrap();
        let output = crate::test_conversations::opened(
            crate::models::ThreadResponse {
                thread,
                ..Default::default()
            },
            &mut snapshot,
        );
        let operation = StartSubmission {
            thread_id: session.clone(),
            provider: crate::session::ProviderKind::Codex,
            cwd: Some("/project".into()),
            client_user_message_id: input.into(),
            draft: Arc::default(),
        };
        let effects = if current_view {
            operation.apply(&mut snapshot, "", output)
        } else {
            operation.stale(&mut snapshot, "", output)
        };
        assert!(!snapshot.input.new_drafts.values().any(|id| id == &session));
        assert!(effects.is_empty(), "the Host owns the admitted first input");
        assert!(snapshot.sessions.pending_submissions.is_empty());
        assert_eq!(
            snapshot.panes.primary().navigation.draft_key,
            if current_view {
                draft_key.clone()
            } else {
                DraftKey::from("other")
            }
        );
        if current_view {
            assert_eq!(
                snapshot.panes.primary().navigation.thread_id.as_ref(),
                Some(&session)
            );
        } else {
            assert_eq!(snapshot.panes.primary().navigation, navigation);
        }
        let restored =
            crate::persistence::decode(&crate::persistence::encode(&snapshot).unwrap()).unwrap();
        assert_eq!(
            restored.panes.primary().navigation.draft_key,
            snapshot.panes.primary().navigation.draft_key
        );
    }
}

#[cfg(test)]
mod dictation_tests {
    use super::*;

    #[rstest::rstest]
    fn transcription_stays_in_the_current_draft_without_an_available_agent(
        #[values(true, false)] send: bool,
    ) {
        let mut snapshot = Snapshot::default();
        let draft_key = snapshot.panes.primary().navigation.draft_key.clone();
        let operation = Dictate {
            draft_key: draft_key.clone(),
            preparation: None,
            audio: vec![],
            send,
            client_user_message_id: "voice".into(),
        };
        let captured = operation.capture(&snapshot, "").unwrap();
        Arc::make_mut(&mut snapshot.input.drafts).insert(
            draft_key.clone(),
            Arc::new(Draft {
                text: "typed while recording".into(),
                ..Default::default()
            }),
        );
        let effects = operation.apply(
            &mut snapshot,
            "",
            (
                captured,
                rpc::Transcription {
                    text: "recognized speech".into(),
                },
            ),
        );
        assert!(effects.is_empty());
        assert!(snapshot.sessions.pending_submissions.is_empty());
        assert_eq!(
            snapshot.input.drafts[&draft_key].text,
            "typed while recording\nrecognized speech"
        );
        assert_eq!(
            snapshot
                .operation_error(OperationKey::Submission { draft_key })
                .is_some(),
            send
        );
    }
}

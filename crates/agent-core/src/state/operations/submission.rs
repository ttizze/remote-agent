use super::{
    sessions::{open_thread, refresh_thread},
    *,
};
use crate::client::ClientExt;

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
    type Input = Arc<Request>;
    fn capture(&self, snapshot: &Snapshot, _pane_id: &str) -> Result<Self::Input, PeerError> {
        snapshot
            .request(&self.request_id)
            .cloned()
            .ok_or_else(|| PeerError::InvalidMessage("server request is no longer pending".into()))
    }
    type Output = ();
    const STALE_POLICY: StalePolicy = StalePolicy::Apply;
    async fn run(
        &self,
        request: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        context.client.respond(&request, &self.answer).await
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartSubmission {
    pub provider: crate::session::ProviderKind,
    pub draft_key: DraftKey,
    pub cwd: Option<String>,
    pub client_user_message_id: agent_protocol::ids::ClientInputId,
    pub draft: Arc<Draft>,
}
impl Operation for StartSubmission {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Submission {
            draft_key: self.draft_key.clone(),
        })
    }
    no_input!();
    fn submission_id(&self) -> Option<&str> {
        Some(&self.client_user_message_id)
    }
    type Output = crate::session::OpenedSession;
    fn stale(self, snapshot: &mut Snapshot, pane_id: &str, output: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.sessions.subscriptions)
            .insert(output.session, output.subscription_id);
        self.complete(snapshot, pane_id, output.response.thread, false)
    }
    fn outcome(output: &mut Self::Output) -> Outcome {
        Outcome::StartedThread {
            id: output.session.clone(),
        }
    }
    async fn run(
        &self,
        _: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        context
            .call(&CreateSession {
                provider: self.provider,
                cwd: self.cwd.clone(),
                model: self.draft.model.clone(),
                initial_input: Some(rpc::InitialInput {
                    workspace: self.draft.workspace.clone(),
                    client_user_message_id: self.client_user_message_id.clone(),
                    input: submission_input(
                        &self.draft.text,
                        &self.draft.attachments,
                        &self.draft.invocations,
                        &self.draft.contexts,
                        Some(self.provider),
                    ),
                    effort: self.draft.effort.clone(),
                    service_tier: self.draft.service_tier.clone(),
                }),
            })
            .await
    }
    fn apply(self, snapshot: &mut Snapshot, pane_id: &str, output: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.sessions.subscriptions)
            .insert(output.session, output.subscription_id);
        self.complete(snapshot, pane_id, output.response.thread, true)
    }
}

impl StartSubmission {
    fn complete(
        self,
        snapshot: &mut Snapshot,
        pane_id: &str,
        mut thread: Thread,
        current_view: bool,
    ) -> Vec<Effect> {
        let Self {
            draft_key,
            client_user_message_id,
            draft,
            ..
        } = self;
        let Some(id) = thread.id.clone() else {
            if let Some(pane) = snapshot.panes.get_mut(pane_id) {
                Arc::make_mut(pane).error = Some("thread ID is missing".into());
            }
            return Vec::new();
        };
        thread
            .submissions
            .entry(client_user_message_id.clone())
            .or_insert(crate::session::SubmissionDelivery::Accepted { turn_id: None });
        Arc::make_mut(&mut snapshot.sessions.draft_sessions).insert(draft_key.clone(), id.clone());
        let current = snapshot.input.drafts.get(&draft_key);
        let empty = Arc::new(Draft {
            text: String::new(),
            attachments: Vec::new(),
            invocations: Vec::new(),
            contexts: Vec::new(),
            ..draft.as_ref().clone()
        });
        let remove_original = current_view || current == Some(&empty);
        let target = if current_view {
            current.unwrap_or(&empty).clone()
        } else {
            empty
        };
        if let Some(pane) = snapshot.panes.get_mut(pane_id) {
            let pane = Arc::make_mut(pane);
            if current_view {
                let location = pane.workspace.location(pane.navigation.cwd.clone());
                Arc::make_mut(&mut pane.workspaces).insert(id.clone().into(), location);
            } else if let Some(workspace) = Arc::make_mut(&mut pane.workspaces).remove(&draft_key) {
                Arc::make_mut(&mut pane.workspaces).insert(id.clone().into(), workspace);
            }
        }
        let effects = if current_view {
            open_thread(snapshot, pane_id, thread, None, None, None)
        } else {
            refresh_thread(snapshot, pane_id, thread);
            Vec::new()
        };
        if remove_original && let Some(pane) = snapshot.panes.get_mut(pane_id) {
            Arc::make_mut(&mut Arc::make_mut(pane).workspaces).remove(&draft_key);
        }
        let drafts = Arc::make_mut(&mut snapshot.input.drafts);
        if remove_original {
            drafts.remove(&draft_key);
        }
        drafts.insert(id.clone().into(), target);
        if let Some(pending) = Arc::make_mut(&mut snapshot.sessions.pending_submissions)
            .get_mut(&client_user_message_id)
        {
            Arc::make_mut(pending).draft_key = id.clone().into();
        }
        // Keep the accepted local input until the provider echoes its body.
        reconcile_pending(snapshot, &id);
        effects
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendSubmission {
    pub thread_id: crate::session::SessionRef,
    pub client_user_message_id: agent_protocol::ids::ClientInputId,
    pub draft: Arc<Draft>,
    pub follow_up: agent_protocol::operations::FollowUp,
}
#[derive(Debug)]
pub enum SubmissionProgress {
    Opened(Box<crate::session::OpenedSession>),
    Sent(Option<agent_protocol::ids::TurnId>),
}
impl Operation for SendSubmission {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Submission {
            draft_key: self.thread_id.clone().into(),
        })
    }
    type Input = (bool, Option<crate::session::ProviderKind>);
    fn capture(&self, snapshot: &Snapshot, _pane_id: &str) -> Result<Self::Input, PeerError> {
        Ok((
            !snapshot
                .sessions
                .subscriptions
                .contains_key(&self.thread_id),
            snapshot.draft_provider(&self.thread_id.clone().into(), self.draft.model.as_ref()),
        ))
    }
    const STALE_POLICY: StalePolicy = StalePolicy::Apply;
    fn submission_id(&self) -> Option<&str> {
        Some(&self.client_user_message_id)
    }
    type Output = SubmissionProgress;
    fn outcome(output: &mut Self::Output) -> Outcome {
        match output {
            SubmissionProgress::Opened(_) => Outcome::Applied,
            SubmissionProgress::Sent(turn_id) => Outcome::Submitted {
                turn_id: turn_id.clone(),
            },
        }
    }
    async fn run(
        &self,
        (needs_subscription, provider): Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        if needs_subscription {
            let read = ReadThread::new(self.thread_id.clone());
            return context
                .call(&OpenThread {
                    read: &read,
                    known_version: None,
                })
                .await
                .map(|opened| SubmissionProgress::Opened(Box::new(opened)));
        }
        let reply = context
            .client
            .call(&Submission {
                fresh_context: self.draft.fresh_context,
                thread_id: self.thread_id.clone(),
                client_user_message_id: self.client_user_message_id.clone(),
                input: submission_input(
                    &self.draft.text,
                    &self.draft.attachments,
                    &self.draft.invocations,
                    &self.draft.contexts,
                    provider,
                ),
                model: self.draft.model.clone(),
                effort: self.draft.effort.clone(),
                service_tier: self.draft.service_tier.clone(),
                follow_up: self.follow_up,
            })
            .await?;
        Ok(SubmissionProgress::Sent(reply.turn_id))
    }
    fn apply(self, snapshot: &mut Snapshot, pane_id: &str, output: Self::Output) -> Vec<Effect> {
        let turn_id = match output {
            SubmissionProgress::Opened(opened) => {
                let mut effects =
                    ReadThread::new(self.thread_id.clone()).apply(snapshot, pane_id, *opened);
                effects.push(Effect::execute(self));
                return effects;
            }
            SubmissionProgress::Sent(turn_id) => turn_id,
        };
        let Self {
            thread_id,
            client_user_message_id,
            ..
        } = self;

        if let Some(thread) = shared_mut(&mut snapshot.sessions.conversations, &thread_id) {
            thread.submissions.insert(
                client_user_message_id,
                crate::session::SubmissionDelivery::Accepted { turn_id },
            );
        }
        reconcile_pending(snapshot, &thread_id);

        Vec::new()
    }
}

fn submission_input(
    text: &str,
    attachments: &[Attachment],
    invocations: &[agent_protocol::composer::Invocation],
    contexts: &[agent_protocol::context::MessageContext],
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
    input.extend(contexts.iter().cloned().map(|context| Input::Context {
        context: Box::new(context),
    }));
    input
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadAttachment {
    pub draft_key: DraftKey,
    pub attachment: Attachment,
    pub snapshot: Option<crate::composer::snapshot::SnapShotCapture>,
}

impl Operation for UploadAttachment {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Attachment {
            draft_key: self.draft_key.clone(),
        })
    }
    no_input!();
    type Output = String;
    fn prepare(&mut self, snapshot: &mut Snapshot, _pane_id: &str) -> Result<(), String> {
        if let Some(capture) = &self.snapshot {
            let current = snapshot
                .input
                .drafts
                .get(&self.draft_key)
                .map_or(&[][..], |draft| draft.contexts.as_slice());
            capture
                .clone()
                .append(
                    current,
                    self.attachment.path.clone(),
                    self.attachment.is_image,
                )
                .map_err(str::to_owned)?;
        }
        Ok(())
    }
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
    fn complete(
        mut self,
        snapshot: &mut Snapshot,
        _pane_id: &str,
        path: Self::Output,
        _current: bool,
    ) -> Result<Vec<Effect>, PeerError> {
        // Navigation can retire the requesting pane; the upload still belongs
        // to its original draft. Validate both additions before publishing either.
        let contexts = if let Some(capture) = self.snapshot.take() {
            let current = snapshot
                .input
                .drafts
                .get(&self.draft_key)
                .map_or(&[][..], |draft| draft.contexts.as_slice());
            Some(
                capture
                    .append(current, path.clone(), self.attachment.is_image)
                    .map_err(|reason| PeerError::InvalidMessage(reason.into()))?,
            )
        } else {
            None
        };
        self.attachment.path = path;
        let draft = Arc::make_mut(
            Arc::make_mut(&mut snapshot.input.drafts)
                .entry(self.draft_key)
                .or_default(),
        );
        if let Some(contexts) = contexts {
            draft.contexts = contexts;
        }
        draft.attachments.push(self.attachment);
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod startup_identity_tests {
    use super::*;
    #[rstest::rstest]
    fn completed_startup_keeps_the_panel_identity_after_pending_input_is_gone(
        #[values(true, false)] current_view: bool,
    ) {
        let draft_key = DraftKey::from("local");
        let session = crate::session::SessionRef {
            provider: crate::session::ProviderKind::Codex,
            id: "created".into(),
        };
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut Arc::make_mut(snapshot.panes.primary_mut()).navigation).draft_key =
            if current_view {
                draft_key.clone()
            } else {
                DraftKey::from("other")
            };
        let navigation = snapshot.panes.primary().navigation.clone();
        let thread: Thread =
            serde_json::from_value(serde_json::json!({"id":session, "cwd":"/project"})).unwrap();
        let effects = StartSubmission {
            provider: session.provider,
            draft_key: draft_key.clone(),
            cwd: Some("/project".into()),
            client_user_message_id: "submitted".into(),
            draft: Arc::default(),
        }
        .complete(&mut snapshot, "", thread, current_view);
        assert!(effects.is_empty(), "the Host owns the admitted first input");
        assert!(snapshot.sessions.pending_submissions.is_empty());
        assert_eq!(
            snapshot.sessions.draft_sessions.get(&draft_key),
            Some(&session)
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
        assert!(restored.sessions.draft_sessions.is_empty());
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

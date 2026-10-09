use super::{
    threads::{open_thread, refresh_thread},
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
    fn capture(&self, snapshot: &Snapshot) -> Result<Self::Input, PeerError> {
        Ok(snapshot
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
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        if output.1.text.trim().is_empty() {
            return Vec::new();
        }
        if !self.send {
            return self.stale(snapshot, output);
        }
        let (mut draft, output) = output;
        let clear_draft = draft.clone();
        append_transcript(&mut Arc::make_mut(&mut draft).text, &output.text);
        let (next, effects) = submission(
            snapshot,
            snapshot.navigation.thread_id.clone(),
            self.draft_key.clone(),
            draft,
            self.client_user_message_id.clone(),
            Some(clear_draft.clone()),
        );
        *snapshot = next;
        if effects.is_empty() {
            return self.stale(snapshot, (clear_draft, output));
        }
        effects
    }
    fn stale(self, snapshot: &mut Snapshot, (_, output): Self::Output) -> Vec<Effect> {
        if output.text.trim().is_empty() {
            return Vec::new();
        }
        let draft = Arc::make_mut(
            Arc::make_mut(&mut snapshot.drafts)
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
    fn capture(&self, snapshot: &Snapshot) -> Result<Self::Input, PeerError> {
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
    fn stale(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.subscriptions).insert(output.session, output.subscription_id);
        self.complete(snapshot, output.response.thread, false)
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
            })
            .await
    }
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.subscriptions).insert(output.session, output.subscription_id);
        self.complete(snapshot, output.response.thread, true)
    }
}

impl StartSubmission {
    fn complete(self, snapshot: &mut Snapshot, thread: Thread, current_view: bool) -> Vec<Effect> {
        let Self {
            draft_key,
            client_user_message_id,
            draft,
            ..
        } = self;
        let Some(id) = thread.id.clone() else {
            snapshot.error = Some("thread ID is missing".into());
            return Vec::new();
        };
        let current = snapshot.drafts.get(&draft_key);
        let empty = Arc::new(Draft {
            text: String::new(),
            attachments: Vec::new(),
            invocations: Vec::new(),
            ..draft.as_ref().clone()
        });
        let remove_original = current_view || current == Some(&empty);
        let target = if current_view {
            current.unwrap_or(&empty).clone()
        } else {
            empty
        };
        let mut effects = if current_view {
            open_thread(snapshot, thread, None)
        } else {
            refresh_thread(snapshot, thread);
            Vec::new()
        };
        let drafts = Arc::make_mut(&mut snapshot.drafts);
        if remove_original {
            drafts.remove(&draft_key);
        }
        drafts.insert(id.clone().into(), target);
        if let Some(pending) =
            Arc::make_mut(&mut snapshot.pending_submissions).get_mut(&client_user_message_id)
        {
            Arc::make_mut(pending).draft_key = id.clone().into();
        }
        effects.push(Effect::execute(SendSubmission {
            thread_id: id,
            client_user_message_id,
            draft,
        }));
        effects
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendSubmission {
    pub thread_id: crate::session::SessionRef,
    pub client_user_message_id: agent_protocol::ids::ClientInputId,
    pub draft: Arc<Draft>,
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
    type Input = bool;
    fn capture(&self, snapshot: &Snapshot) -> Result<Self::Input, PeerError> {
        Ok(!snapshot.subscriptions.contains_key(&self.thread_id))
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
        needs_subscription: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        if needs_subscription {
            let open = ReadThread::new(self.thread_id.clone());
            return context
                .call(&open)
                .await
                .map(|opened| SubmissionProgress::Opened(Box::new(opened)));
        }
        let mut input = Vec::with_capacity(
            self.draft.attachments.len() + usize::from(!self.draft.text.is_empty()),
        );
        if !self.draft.text.is_empty() {
            input.push(Input::Text {
                text: self.draft.text.clone(),
            });
        }
        input.extend(
            self.draft
                .invocations
                .iter()
                .filter(|item| {
                    item.provider == self.thread_id.provider && item.is_in(&self.draft.text)
                })
                .map(agent_protocol::composer::Invocation::input),
        );
        for attachment in &self.draft.attachments {
            input.push(if attachment.is_image {
                Input::LocalImage {
                    path: attachment.path.clone(),
                }
            } else {
                Input::Mention {
                    path: attachment.path.clone(),
                    name: attachment.name.clone(),
                }
            });
        }
        let reply = context
            .client
            .call(&Submission {
                thread_id: self.thread_id.clone(),
                client_user_message_id: self.client_user_message_id.clone(),
                input,
                model: self.draft.model.clone(),
                effort: self.draft.effort.clone(),
                service_tier: self.draft.service_tier.clone(),
            })
            .await?;
        Ok(SubmissionProgress::Sent(reply.turn_id))
    }
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        let turn_id = match output {
            SubmissionProgress::Opened(opened) => {
                let mut effects = ReadThread::new(self.thread_id.clone()).apply(snapshot, *opened);
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

        if let Some(thread) = shared_mut(&mut snapshot.conversations, &thread_id) {
            thread.submissions.insert(
                client_user_message_id,
                crate::session::SubmissionDelivery::Accepted { turn_id },
            );
        }
        reconcile_pending(snapshot, &thread_id);

        Vec::new()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadAttachment {
    pub draft_key: DraftKey,
    pub attachment: Attachment,
    pub directory: String,
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
    fn prepare(&mut self, snapshot: &mut Snapshot) -> Result<(), String> {
        snapshot.error = None;
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
            std::path::Path::new(&self.directory),
            &self.attachment.name,
        )
        .await
        .map_err(|error| PeerError::InvalidMessage(error.to_string()))?;
        Ok(uploaded.path)
    }
    fn apply(mut self, snapshot: &mut Snapshot, path: Self::Output) -> Vec<Effect> {
        self.attachment.path = path;
        add_attachment(snapshot, self.draft_key, self.attachment);
        Vec::new()
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
        let draft_key = snapshot.navigation.draft_key.clone();
        let operation = Dictate {
            draft_key: draft_key.clone(),
            preparation: None,
            audio: vec![],
            send,
            client_user_message_id: "voice".into(),
        };
        let captured = operation.capture(&snapshot).unwrap();
        Arc::make_mut(&mut snapshot.drafts).insert(
            draft_key.clone(),
            Arc::new(Draft {
                text: "typed while recording".into(),
                ..Default::default()
            }),
        );
        let effects = operation.apply(
            &mut snapshot,
            (
                captured,
                rpc::Transcription {
                    text: "recognized speech".into(),
                },
            ),
        );
        assert!(effects.is_empty());
        assert!(snapshot.pending_submissions.is_empty());
        assert_eq!(
            snapshot.drafts[&draft_key].text,
            "typed while recording\nrecognized speech"
        );
        assert_eq!(snapshot.error.is_some(), send);
    }
}

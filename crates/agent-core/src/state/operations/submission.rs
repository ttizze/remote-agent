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
    pub alternate: bool,
    pub client_user_message_id: agent_protocol::ids::ClientInputId,
}

impl Operation for Dictate {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Dictation {
            draft_key: self.draft_key.clone(),
        })
    }
    type Input = (Arc<Draft>, Option<Arc<Draft>>);
    fn capture(&self, snapshot: &Snapshot) -> Result<Self::Input, PeerError> {
        let origin = snapshot.queue_edits.get(&self.draft_key).cloned();
        if !snapshot.queue_edit_matches(&self.draft_key, origin.as_ref()) {
            return Err(PeerError::InvalidMessage(
                "queued input is no longer being edited".into(),
            ));
        }
        Ok((
            snapshot
                .drafts
                .get(&self.draft_key)
                .cloned()
                .unwrap_or_default(),
            origin,
        ))
    }
    type Output = (Self::Input, rpc::Transcription);
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
        if !self.send || !snapshot.queue_edit_matches(&self.draft_key, output.0.1.as_ref()) {
            return self.stale(snapshot, output);
        }
        let ((mut draft, _), output) = output;
        let Self {
            draft_key,
            client_user_message_id,
            alternate,
            ..
        } = self;
        let clear_draft = draft.clone();
        append_transcript(&mut Arc::make_mut(&mut draft).text, &output.text);
        let (next, effects) = submission(
            snapshot,
            snapshot.navigation.thread_id.clone(),
            draft_key,
            draft,
            client_user_message_id,
            Some(clear_draft),
            snapshot.submission_action(snapshot.navigation.thread_id.as_ref(), alternate, false)
                == ComposerAction::Queue,
        );
        *snapshot = next;
        effects
    }
    fn stale(self, snapshot: &mut Snapshot, ((_, original), output): Self::Output) -> Vec<Effect> {
        if output.text.trim().is_empty() {
            return Vec::new();
        }
        let key = if let DraftKey::Queued { session, .. } = &self.draft_key {
            if snapshot.queue_edit_matches(&self.draft_key, original.as_ref()) {
                self.draft_key
            } else {
                let ordinary = DraftKey::from(session);
                if snapshot.drafts.get(&ordinary).is_some_and(|draft| {
                    !draft.text.trim().is_empty()
                        || !draft.attachments.is_empty()
                        || !draft.invocations.is_empty()
                }) {
                    snapshot.error =
                        Some("予約の編集が終了したため、文字起こしを追加できませんでした".into());
                    return Vec::new();
                }
                ordinary
            }
        } else {
            self.draft_key
        };
        let draft = Arc::make_mut(Arc::make_mut(&mut snapshot.drafts).entry(key).or_default());
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
            force_queue: false,
        }));
        if snapshot.threads.is_some() {
            effects.push(Effect::execute(ListSessions::new(
                (*snapshot.list_query).clone(),
            )));
        }
        effects
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendSubmission {
    pub thread_id: crate::session::SessionRef,
    pub client_user_message_id: agent_protocol::ids::ClientInputId,
    pub draft: Arc<Draft>,
    pub force_queue: bool,
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
        let submission = self
            .draft
            .submission(self.thread_id.clone(), self.client_user_message_id.clone());
        let reply = if self.force_queue {
            context
                .client
                .request(&agent_protocol::protocol::Call::QueueInput(submission))
                .await?
        } else {
            context.client.call(&submission).await?
        };
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
            // A queued receipt can arrive after the worker's Sending/Accepted
            // notification. Keep that later Host state instead of regressing it.
            if turn_id.is_some() {
                thread.submissions.insert(
                    client_user_message_id,
                    crate::session::SubmissionDelivery::Accepted { turn_id },
                );
            } else {
                thread
                    .submissions
                    .entry(client_user_message_id)
                    .or_insert(crate::session::SubmissionDelivery::Queued);
            }
        }
        reconcile_pending(snapshot, &thread_id);

        Vec::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveQueuedInput {
    pub submission: Submission,
    pub draft_key: DraftKey,
}
impl Operation for SaveQueuedInput {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Submission {
            draft_key: self.draft_key.clone(),
        })
    }
    type Input = Arc<Draft>;
    fn capture(&self, snapshot: &Snapshot) -> Result<Self::Input, PeerError> {
        snapshot
            .queue_edits
            .get(&self.draft_key)
            .cloned()
            .ok_or_else(|| {
                PeerError::InvalidMessage("queued input is no longer being edited".into())
            })
    }
    type Output = Arc<Draft>;
    const STALE_POLICY: StalePolicy = StalePolicy::Apply;
    fn prepare(&mut self, snapshot: &mut Snapshot) -> Result<(), String> {
        if !snapshot.queue_edits.contains_key(&self.draft_key) {
            return Err("queued input is no longer being edited".into());
        }
        snapshot.error = None;
        Ok(())
    }
    async fn run(
        &self,
        original: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        context
            .call(&agent_protocol::queue::QueueControl {
                session: self.submission.thread_id.clone(),
                action: agent_protocol::queue::QueueAction::Edit {
                    submission: self.submission.clone(),
                },
            })
            .await?;
        Ok(original)
    }
    fn apply(self, snapshot: &mut Snapshot, original: Self::Output) -> Vec<Effect> {
        if !snapshot.queue_edit_matches(&self.draft_key, Some(&original)) {
            return vec![Effect::continuation(ReadThread::new(
                self.submission.thread_id,
            ))];
        }
        let changed = snapshot.drafts.get(&self.draft_key).is_some_and(|draft| {
            draft.submission(
                self.submission.thread_id.clone(),
                self.submission.client_user_message_id.clone(),
            ) != self.submission
        });
        super::super::queued_edit::finish(snapshot, &self.draft_key, changed);
        vec![Effect::continuation(ReadThread::new(
            self.submission.thread_id,
        ))]
    }
}

impl Operation for agent_protocol::queue::QueueControl {
    rpc_operation!();
    fn apply(self, _: &mut Snapshot, _: Self::Output) -> Vec<Effect> {
        vec![Effect::continuation(ReadThread::new(self.session))]
    }
}

impl Operation for agent_protocol::queue::SteerQueued {
    rpc_operation!();
    const STALE_POLICY: StalePolicy = StalePolicy::Apply;
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Submission {
            draft_key: DraftKey::Queued {
                session: self.session.clone(),
                id: self.id.clone(),
            },
        })
    }
    fn apply(self, _: &mut Snapshot, _: Self::Output) -> Vec<Effect> {
        vec![Effect::continuation(ReadThread::new(self.session))]
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
    type Input = Option<Arc<Draft>>;
    fn capture(&self, snapshot: &Snapshot) -> Result<Self::Input, PeerError> {
        let original = snapshot.queue_edits.get(&self.draft_key).cloned();
        if !snapshot.queue_edit_matches(&self.draft_key, original.as_ref()) {
            return Err(PeerError::InvalidMessage(
                "queued input is no longer being edited".into(),
            ));
        }
        Ok(original)
    }
    type Output = (Self::Input, String);
    const STALE_POLICY: StalePolicy = StalePolicy::Apply;
    fn prepare(&mut self, snapshot: &mut Snapshot) -> Result<(), String> {
        snapshot.error = None;
        Ok(())
    }
    async fn run(
        &self,
        original: Self::Input,
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
        Ok((original, uploaded.path))
    }
    fn apply(mut self, snapshot: &mut Snapshot, (original, path): Self::Output) -> Vec<Effect> {
        if !snapshot.queue_edit_matches(&self.draft_key, original.as_ref()) {
            snapshot.error = Some("予約の編集が終了したため、添付を追加できませんでした".into());
            return Vec::new();
        }
        self.attachment.path = path;
        add_attachment(snapshot, self.draft_key, self.attachment);
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::session::{ProviderKind, SubmissionDelivery};

    #[test]
    fn queued_receipt_preserves_later_host_progress_and_keeps_the_input_editable_until_claimed() {
        let session = crate::session::SessionRef {
            provider: ProviderKind::Codex,
            id: "owned".into(),
        };
        let draft = Arc::new(Draft {
            text: "waiting message".into(),
            ..Default::default()
        });
        for current in [
            None,
            Some(SubmissionDelivery::Queued),
            Some(SubmissionDelivery::Sending),
            Some(SubmissionDelivery::Accepted {
                turn_id: Some("active".into()),
            }),
            Some(SubmissionDelivery::Unknown),
            Some(SubmissionDelivery::Rejected),
        ] {
            let mut thread = Thread {
                id: Some(session.clone()),
                ..Default::default()
            };
            thread
                .queued_inputs
                .push(agent_protocol::queue::QueueEntry {
                    submission: draft.submission(session.clone(), "message".into()),
                    delivery: SubmissionDelivery::Queued,
                });
            if let Some(current) = &current {
                thread.submissions.insert("message".into(), current.clone());
            }
            let mut snapshot = Snapshot {
                navigation: Arc::new(Navigation {
                    thread_id: Some(session.clone()),
                    draft_key: session.clone().into(),
                    ..Default::default()
                }),
                conversations: Arc::new([(session.clone(), Arc::new(thread))].into()),
                ..Default::default()
            };
            let original = snapshot.drafts.clone();
            SendSubmission {
                thread_id: session.clone(),
                client_user_message_id: "message".into(),
                draft: draft.clone(),
                force_queue: true,
            }
            .apply(&mut snapshot, SubmissionProgress::Sent(None));
            let expected = current.unwrap_or(SubmissionDelivery::Queued);
            assert_eq!(
                snapshot.conversations[&session].submissions["message"],
                expected
            );
            assert!(Arc::ptr_eq(&original, &snapshot.drafts));
            let (editing, _) = reduce(
                &snapshot,
                Event::Intent(Intent::BeginQueueEdit {
                    id: "message".into(),
                }),
            );
            assert_eq!(
                editing.editing_queue_id().is_some(),
                expected == SubmissionDelivery::Queued
            );
        }
    }
}

use super::threads::{open_thread, refresh_thread};
use super::*;

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dictate {
    pub draft_key: String,
    pub audio: Vec<u8>,
    pub send: bool,
    pub client_user_message_id: String,
}

impl Operation for Dictate {
    type Output = (Arc<Draft>, rpc::Transcription);
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        let draft = context
            .snapshot
            .drafts
            .get(&self.draft_key)
            .cloned()
            .unwrap_or_default();
        Ok((
            draft,
            context
                .call(&rpc::Transcribe {
                    audio: Base64Bytes(&self.audio),
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
        let Self {
            draft_key,
            client_user_message_id,
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
            Some(output.text),
            Some(clear_draft),
        );
        *snapshot = next;
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
    pub request_id: Value,
    pub answer: Answer,
}
impl Operation for Respond {
    type Output = ();
    const APPLY_WHEN_STALE: bool = true;
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        let request = context
            .snapshot
            .requests
            .get(&self.request_id.to_string())
            .ok_or_else(|| {
                PeerError::InvalidMessage("server request is no longer pending".into())
            })?;
        context.client.respond(request, &self.answer).await
    }
    fn apply(self, snapshot: &mut Snapshot, _output: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.requests).remove(&self.request_id.to_string());
        Vec::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartSubmission {
    pub draft_key: String,
    pub cwd: Option<String>,
    pub client_user_message_id: String,
    pub draft: Arc<Draft>,
}
impl Operation for StartSubmission {
    fn submission_id(&self) -> Option<&str> {
        Some(&self.client_user_message_id)
    }
    type Output = crate::models::ThreadResponse;
    const ORDERED: bool = true;
    fn stale(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        self.complete(snapshot, output.thread, false)
    }
    fn outcome(output: &mut Self::Output) -> Outcome {
        Outcome::StartedThread {
            id: output.thread.id.clone().expect("validated thread ID"),
        }
    }
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context
            .call(&StartThread {
                cwd: self.cwd.clone(),
                model: self.draft.model.clone(),
            })
            .await
    }
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        self.complete(snapshot, output.thread, true)
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
        let original = snapshot
            .pending_submissions
            .get(&client_user_message_id)
            .and_then(|pending| pending.clear_draft.as_ref())
            .unwrap_or(&draft);
        let remove_original = current_view || current == Some(original);
        let target = if current_view {
            current.unwrap_or(original).clone()
        } else {
            original.clone()
        };
        let mut effects = if current_view {
            open_thread(snapshot, thread, None)
        } else {
            refresh_thread(snapshot, thread)
        };
        let drafts = Arc::make_mut(&mut snapshot.drafts);
        if remove_original {
            drafts.remove(&draft_key);
        }
        drafts.insert(id.clone(), target);
        if let Some(pending) =
            Arc::make_mut(&mut snapshot.pending_submissions).get_mut(&client_user_message_id)
        {
            Arc::make_mut(pending).draft_key = id.clone();
        }
        effects.push(Effect::execute(SendSubmission {
            thread_id: id,
            client_user_message_id,
            draft,
        }));
        if snapshot.threads.is_some() {
            effects.push(Effect::execute(ListThreads::new(
                (*snapshot.list_query).clone(),
            )));
        }
        effects
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendSubmission {
    pub thread_id: String,
    pub client_user_message_id: String,
    pub draft: Arc<Draft>,
}
impl Operation for SendSubmission {
    fn submission_id(&self) -> Option<&str> {
        Some(&self.client_user_message_id)
    }
    type Output = Option<String>;
    const APPLY_WHEN_STALE: bool = true;
    fn outcome(output: &mut Self::Output) -> Outcome {
        Outcome::Submitted {
            turn_id: output.clone(),
        }
    }
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        let target = submission_target(
            context
                .snapshot
                .conversations
                .get(&self.thread_id)
                .map(Arc::as_ref),
            context.snapshot.threads.as_ref().and_then(|list| {
                list.data
                    .iter()
                    .find(|thread| thread.id.as_ref() == Some(&self.thread_id))
            }),
            context
                .snapshot
                .activity
                .active
                .get(&self.thread_id)
                .copied(),
        )?;
        let mut input = Vec::with_capacity(
            self.draft.attachments.len() + usize::from(!self.draft.text.is_empty()),
        );
        if !self.draft.text.is_empty() {
            input.push(Input::Text {
                text: &self.draft.text,
                text_elements: &[],
            });
        }
        for attachment in &self.draft.attachments {
            input.push(if attachment.is_image {
                Input::LocalImage {
                    path: &attachment.path,
                }
            } else {
                Input::Mention {
                    path: &attachment.path,
                    name: &attachment.name,
                }
            });
        }
        let reply = context
            .client
            .submit(
                &Submission {
                    thread_id: &self.thread_id,
                    client_user_message_id: &self.client_user_message_id,
                    input: &input,
                    model: self.draft.model.as_deref(),
                    effort: self.draft.effort.as_deref(),
                    service_tier: self.draft.service_tier.as_deref(),
                },
                target,
            )
            .await?;
        Ok(reply.value)
    }
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        let Self {
            thread_id,
            client_user_message_id,
            draft,
        } = self;
        let turn_id = output;

        let draft = snapshot
            .pending_submissions
            .get(&client_user_message_id)
            .and_then(|pending| pending.clear_draft.as_ref())
            .unwrap_or(&draft);
        if let Some(current) = snapshot.drafts.get(&thread_id) {
            let clear_text = !current.text.is_empty() && current.text == draft.text;
            let sent_attachment = |attachment: &Attachment| {
                draft
                    .attachments
                    .iter()
                    .any(|sent| sent.path == attachment.path)
            };
            if clear_text || current.attachments.iter().any(sent_attachment) {
                let retained = Draft {
                    text: if clear_text {
                        String::new()
                    } else {
                        current.text.clone()
                    },
                    attachments: current
                        .attachments
                        .iter()
                        .filter(|attachment| !sent_attachment(attachment))
                        .cloned()
                        .collect(),
                    model: current.model.clone(),
                    effort: current.effort.clone(),
                    service_tier: current.service_tier.clone(),
                };
                Arc::make_mut(&mut snapshot.drafts).insert(thread_id.clone(), Arc::new(retained));
            }
        }
        if let Some(pending) =
            Arc::make_mut(&mut snapshot.pending_submissions).get_mut(&client_user_message_id)
        {
            let pending = Arc::make_mut(pending);
            pending.accepted = true;
            if pending.turn_id.is_none() {
                pending.turn_id = turn_id;
            }
        }
        reconcile_pending(snapshot, &thread_id);

        Vec::new()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadAttachment {
    pub draft_key: String,
    pub attachment: Attachment,
    pub directory: String,
}

impl Operation for UploadAttachment {
    type Output = String;
    const APPLY_WHEN_STALE: bool = true;
    fn prepare(&mut self, snapshot: &mut Snapshot) -> Result<(), String> {
        snapshot.error = None;
        Ok(())
    }
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        let session = context.session.ok_or_else(|| {
            PeerError::InvalidMessage("binary transfers require an iroh session".into())
        })?;
        let uploaded = crate::transfers::upload_file(
            context.peer,
            || async { session.open_stream().await.map_err(std::io::Error::other) },
            std::path::Path::new(&self.attachment.path),
            std::path::Path::new(&self.directory),
            &self.attachment.name,
        )
        .await
        .map_err(|error| PeerError::InvalidMessage(error.to_string()))?;
        #[derive(Deserialize)]
        struct Uploaded {
            path: String,
        }
        let uploaded: Uploaded = serde_json::from_value(uploaded)
            .map_err(|error| PeerError::InvalidMessage(error.to_string()))?;
        Ok(uploaded.path)
    }
    fn apply(mut self, snapshot: &mut Snapshot, path: Self::Output) -> Vec<Effect> {
        self.attachment.path = path;
        add_attachment(snapshot, self.draft_key, self.attachment);
        Vec::new()
    }
}

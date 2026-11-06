//! A pane borrows its Host's Store; it never owns another transport or save loop.
use super::*;
use crate::presentation::view::Presentation;

pub struct Pane {
    store: Arc<Store>,
    id: String,
    presentations: watch::Receiver<Arc<Presentation>>,
    stop: CancellationToken,
    side_chat_preparation: Arc<tokio::sync::Mutex<()>>,
}

impl Store {
    pub fn primary_pane(self: &Arc<Self>) -> Arc<Pane> {
        Arc::new(Pane {
            store: self.clone(),
            id: String::new(),
            presentations: self.subscribe_presentation(),
            stop: self.stop.child_token(),
            side_chat_preparation: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    pub fn open_pane(self: &Arc<Self>, cwd: String) -> Result<Arc<Pane>, PeerError> {
        let id = self.create_pane(cwd)?;
        let stop = self.stop.child_token();
        let presentations = self.subscribe_pane(id.clone(), stop.clone());
        Ok(Arc::new(Pane {
            store: self.clone(),
            id,
            presentations,
            stop,
            side_chat_preparation: Arc::new(tokio::sync::Mutex::new(())),
        }))
    }
}

impl Pane {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn snapshot(&self) -> Result<Arc<Snapshot>, PeerError> {
        if self.stop.is_cancelled() {
            return Err(PeerError::ConnectionClosed("pane is closed".into()));
        }
        self.store
            .snapshot()
            .for_pane(&self.id)
            .map(Arc::new)
            .ok_or_else(|| PeerError::ConnectionClosed("pane is closed".into()))
    }

    pub fn presentation(&self) -> Result<Arc<Presentation>, PeerError> {
        Ok(Presentation::current(
            self.snapshot()?,
            &self.presentations.borrow(),
        ))
    }

    pub fn subscribe(&self) -> watch::Receiver<Arc<Presentation>> {
        self.presentations.clone()
    }

    pub fn dispatch(
        &self,
        intent: Intent,
    ) -> impl Future<Output = Result<Outcome, PeerError>> + Send + use<> {
        if self.stop.is_cancelled() {
            futures_util::future::Either::Left(std::future::ready(Err(
                PeerError::ConnectionClosed("pane is closed".into()),
            )))
        } else {
            futures_util::future::Either::Right(self.store.dispatch_pane(&self.id, intent))
        }
    }

    pub fn prepare_dictation(&self) -> crate::client::DictationPreparation {
        self.store.prepare_dictation()
    }

    /// Creation and quote initialization are one operation on this pane. A retry
    /// or a second view task must never replace an already editable draft.
    pub async fn prepare_side_chat(
        self: &Arc<Self>,
        provider: Option<crate::session::ProviderKind>,
        text: String,
    ) -> Result<(), crate::client::AgentError> {
        use crate::client::error;
        let pane = Arc::downgrade(self);
        let preparation = self.side_chat_preparation.clone();
        // Finish an admitted preparation if its waiter is cancelled, but do not
        // keep a closed view alive while the Host is creating the session.
        tokio::spawn(async move {
            let _preparation = preparation.lock().await;
            let creation = {
                let pane = pane.upgrade().ok_or_else(|| error("pane is closed"))?;
                let snapshot = pane.snapshot().map_err(error)?;
                if snapshot.pane.navigation.thread_id.is_some() {
                    return Ok(());
                }
                let provider = provider
                    .or_else(|| {
                        snapshot
                            .model_provider_for_draft(snapshot.pane.navigation.draft_key.clone())
                    })
                    .ok_or_else(|| error("利用可能なエージェントを選択してください。"))?;
                pane.dispatch(Intent::CreateSession(op::CreateSession {
                    provider,
                    cwd: Some(snapshot.pane.navigation.cwd.clone()),
                    model: None,
                    initial_input: None,
                }))
            };
            let outcome = creation.await.map_err(error)?;
            let Outcome::StartedThread { id } = outcome else {
                return Err(error("サイドチャットを開始できませんでした。"));
            };
            let pane = pane.upgrade().ok_or_else(|| error("pane is closed"))?;
            pane.snapshot().map_err(error)?;
            if !text.is_empty() {
                pane.dispatch(Intent::SetDraftText {
                    thread_id: id.into(),
                    text: crate::composer::append_selection_to_draft("", &text),
                })
                .await
                .map_err(error)?;
            }
            Ok(())
        })
        .await
        .map_err(error)?
    }

    pub fn close(&self) {
        self.stop.cancel();
        self.store.close_pane(&self.id);
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        self.close();
    }
}

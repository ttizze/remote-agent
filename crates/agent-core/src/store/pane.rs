//! A pane borrows its Host's Store; it never owns another transport or save loop.
use super::*;
use crate::presentation::view::Presentation;

/// What a side chat starts with.
#[derive(Clone, Debug, PartialEq)]
pub enum SideChatStart {
    /// The selection quoted in its draft; nothing for an empty one.
    Quote(String),
    /// A request to explain the selection, sent at once.
    Explain(String),
}

pub struct Pane {
    store: Arc<Store>,
    id: String,
    presentations: watch::Receiver<Arc<Presentation>>,
    stop: CancellationToken,
    side_chat_preparation: Arc<tokio::sync::Mutex<()>>,
}

impl Store {
    pub fn open_pane(self: &Arc<Self>, cwd: String) -> Result<Arc<Pane>, PeerError> {
        let initial = self.create_pane(cwd)?;
        let id = initial.pane_id.clone();
        let stop = self.stop.child_token();
        let presentations = self.subscribe_pane(initial, stop.clone());
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
        let snapshot = self.store.snapshot();
        if snapshot.panes.get(&self.id).is_none() {
            return Err(PeerError::ConnectionClosed("pane is closed".into()));
        }
        Ok(snapshot)
    }

    pub fn presentation(&self) -> Result<Arc<Presentation>, PeerError> {
        Presentation::current(self.snapshot()?, &self.presentations.borrow())
            .ok_or_else(|| PeerError::ConnectionClosed("pane is closed".into()))
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

    /// Creation and its first input are one operation on this pane. A retry
    /// or a second view task must never replace an already editable draft:
    /// a quote goes only into a side chat this starts. A request to explain
    /// starts a conversation of its own each time, as the desktop client's
    /// "詳細を表示" did, so the draft of the one shown stays as it is.
    pub async fn start_side_chat(
        self: &Arc<Self>,
        provider: Option<crate::session::ProviderKind>,
        start: SideChatStart,
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
                let state = snapshot
                    .panes
                    .get(pane.id())
                    .ok_or_else(|| error("pane is closed"))?;
                if state.navigation.thread_id.is_some() && matches!(start, SideChatStart::Quote(_))
                {
                    return Ok(());
                }
                let provider = provider
                    .or_else(|| {
                        snapshot.model_provider_for_draft(state.navigation.draft_key.clone())
                    })
                    .ok_or_else(|| error("利用可能なエージェントを選択してください。"))?;
                pane.dispatch(Intent::CreateSession(op::CreateSession {
                    provider,
                    cwd: Some(state.navigation.cwd.clone()),
                }))
            };
            let outcome = creation.await.map_err(error)?;
            let Outcome::StartedThread { id } = outcome else {
                return Err(error("サイドチャットを開始できませんでした。"));
            };
            let pane = pane.upgrade().ok_or_else(|| error("pane is closed"))?;
            let snapshot = pane.snapshot().map_err(error)?;
            let (text, send) = match start {
                SideChatStart::Quote(text) if text.is_empty() => return Ok(()),
                SideChatStart::Quote(text) => {
                    (crate::composer::append_selection_to_draft("", &text), false)
                }
                SideChatStart::Explain(text) => (
                    crate::localization::localized_ui_format(
                        snapshot.catalog.ui_language,
                        "次の選択範囲について詳しく説明してください。\n\n{0}".into(),
                        vec![text],
                    ),
                    true,
                ),
            };
            pane.dispatch(Intent::SetDraftText {
                thread_id: id.clone().into(),
                text,
            })
            .await
            .map_err(error)?;
            if send {
                pane.dispatch(Intent::Submit {
                    thread_id: Some(id),
                    client_user_message_id: uuid::Uuid::new_v4().to_string().into(),
                    alternate: false,
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

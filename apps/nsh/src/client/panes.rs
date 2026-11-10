//! Core panes share their Host session; only a closed surface retires its pane.
use super::*;
use crate::l10n::tr;
use agent_core::client::native::AgentPane;
use agent_core::store::SideChatStart;
use agent_protocol::session::ProviderKind;
use tokio::sync::OnceCell;
use tokio_util::sync::CancellationToken;

pub(super) struct Pane {
    host: String,
    core: OnceCell<Arc<AgentPane>>,
    stop: CancellationToken,
    owned: bool,
}

impl Pane {
    fn close(&self) {
        self.stop.cancel();
        if let Some(core) = self.core.get().filter(|_| self.owned) {
            core.retire();
        }
    }
}

impl Client {
    pub(super) fn attach_pane(&self, host: String, pane: Arc<AgentPane>) {
        self.panes.lock().unwrap().insert(
            1,
            Arc::new(Pane {
                host,
                core: OnceCell::new_with(Some(pane)),
                stop: CancellationToken::new(),
                owned: false,
            }),
        );
    }
    pub fn prepare_side_chat(
        self: &Arc<Self>,
        id: u64,
        host: String,
        cwd: String,
        provider: Option<ProviderKind>,
        start: SideChatStart,
    ) {
        let pane = {
            let mut panes = self.panes.lock().unwrap();
            if self.closing.is_cancelled() {
                return;
            }
            panes
                .entry(id)
                .or_insert_with(|| {
                    Arc::new(Pane {
                        host: host.clone(),
                        core: OnceCell::new(),
                        stop: CancellationToken::new(),
                        owned: true,
                    })
                })
                .clone()
        };
        let owner = self.clone();
        self.spawn(async move {
            let result = tokio::select! {
                _ = pane.stop.cancelled() => return,
                result = async {
                    if pane.host != host { return Err(error(tr("サイドチャットの接続先が変わりました。"))); }
                    let core = pane.core.get_or_try_init(|| async {
                        let session = owner.sessions.session(host.clone()).ok_or_else(|| error(tr("接続先が選択されていません")))?;
                        let core = session.ready().await?.open_pane(cwd).await?;
                        owner.observe_pane(id, core.clone(), pane.stop.clone());
                        Ok::<_, AgentError>(core)
                    }).await?;
                    core.start_side_chat(provider, start).await?;
                    core.presentation()
                } => result.map_err(|reason| reason.to_string()),
            };
            let _ = owner.updates.send(Update::SideChatReady { id, result }).await;
        });
    }

    fn observe_pane(self: &Arc<Self>, id: u64, pane: Arc<AgentPane>, stop: CancellationToken) {
        let owner = self.clone();
        self.spawn(async move {
            let Ok(mut value) = pane.presentation() else { return };
            loop {
                if stop.is_cancelled() || owner.updates.send(Update::PanePresentation { id, value: value.clone() }).await.is_err() { break; }
                value = tokio::select! {
                    _ = stop.cancelled() => break,
                    next = pane.next_presentation(value) => match next { Ok(next) => next, Err(_) => break },
                };
            }
        });
    }

    pub fn close_pane(&self, id: u64) {
        let pane = self.panes.lock().unwrap().remove(&id);
        if let Some(pane) = pane {
            pane.close();
        }
    }

    pub(super) fn pane(&self, host: &str, id: u64) -> Result<Arc<AgentPane>, AgentError> {
        self.panes
            .lock()
            .unwrap()
            .get(&id)
            .filter(|pane| pane.host == host && !pane.stop.is_cancelled())
            .and_then(|pane| pane.core.get().cloned())
            .ok_or_else(|| error(tr("サイドチャットが開いていません。")))
    }

    pub(super) fn close_panes(&self) {
        let panes = std::mem::take(&mut *self.panes.lock().unwrap());
        for (_, pane) in panes {
            pane.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::tests::{publication, saved_host};
    use std::time::Duration;

    #[test]
    fn pane_input_uploads_and_closure_never_replace_another_conversations_state() {
        let directory = tempfile::tempdir().unwrap();
        let host = saved_host(directory.path());
        let (runtime, client, incoming) = Client::new(directory.path().into(), None).unwrap();
        let primary = runtime.block_on(publication(&incoming, ""));
        let root_key = primary.composer.controls.navigation.draft_key.clone();
        runtime.block_on(async {
            let mut keys = Vec::new();
            for id in [11, 22] {
                client.prepare_side_chat(
                    id,
                    host.id.clone(),
                    "/same".into(),
                    Some(ProviderKind::Codex),
                    SideChatStart::Quote(String::new()),
                );
                tokio::time::timeout(Duration::from_secs(10), async {
                    loop {
                        if let Update::SideChatReady { id: ready, result } =
                            incoming.recv().await.unwrap()
                            && ready == id
                        {
                            assert!(result.is_err(), "offline creation reports in its own view");
                            break;
                        }
                    }
                })
                .await
                .unwrap();
                keys.push(
                    client
                        .pane(&host.id, id)
                        .unwrap()
                        .presentation()
                        .unwrap()
                        .composer
                        .controls
                        .navigation
                        .draft_key
                        .clone(),
                );
            }
            assert_ne!(keys[0], keys[1]);
            assert_ne!(keys[0], root_key);
            for (pane, key, text) in [
                (None, root_key.clone(), "primary"),
                (Some(11), keys[0].clone(), "first"),
                (Some(22), keys[1].clone(), "second"),
            ] {
                client
                    .request(
                        &host.id,
                        pane,
                        Intent::SetDraftText {
                            thread_id: key,
                            text: text.into(),
                        },
                    )
                    .recv()
                    .await
                    .unwrap()
                    .unwrap();
            }
            assert_eq!(
                client
                    .sessions
                    .session(host.id.clone())
                    .unwrap()
                    .presentation()
                    .composer
                    .draft
                    .text,
                "primary"
            );
            assert_eq!(
                client
                    .pane(&host.id, 11)
                    .unwrap()
                    .presentation()
                    .unwrap()
                    .composer
                    .draft
                    .text,
                "first"
            );
            assert_eq!(
                client
                    .pane(&host.id, 22)
                    .unwrap()
                    .presentation()
                    .unwrap()
                    .composer
                    .draft
                    .text,
                "second"
            );
            let selected = directory.path().join("selected.txt");
            std::fs::write(&selected, "pane attachment").unwrap();
            let batch = uuid::Uuid::new_v4();
            client.attach_batch(
                batch,
                host.id.clone(),
                Some(22),
                keys[1].clone(),
                vec![AttachmentSource::File {
                    path: selected,
                    is_image: false,
                }],
            );
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if let Update::AttachmentsFinished(id) = incoming.recv().await.unwrap()
                        && id == batch
                    {
                        break;
                    }
                }
            })
            .await
            .unwrap();
            assert_eq!(
                client
                    .pane(&host.id, 22)
                    .unwrap()
                    .presentation()
                    .unwrap()
                    .composer
                    .draft
                    .text,
                "second"
            );
            assert!(client.pane("another-host", 11).is_err());
            client.close_pane(11);
            assert!(client.pane(&host.id, 11).is_err());
            assert!(client.pane(&host.id, 22).is_ok());
            assert_eq!(
                client
                    .sessions
                    .session(host.id.clone())
                    .unwrap()
                    .presentation()
                    .composer
                    .draft
                    .text,
                "primary"
            );
            client.shutdown().await.unwrap();
            client.prepare_side_chat(
                33,
                host.id,
                "/late".into(),
                None,
                SideChatStart::Quote(String::new()),
            );
            assert!(client.panes.lock().unwrap().is_empty());
        });
    }
}

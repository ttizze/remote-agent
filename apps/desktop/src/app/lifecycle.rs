//! Selected-host connection, epoch-scoped effects, and operation receipt delivery.
//!
//! All asynchronous UI updates enter through `receive`, which rejects results from
//! an old host before applying operation-specific revision or navigation guards.

use super::{Desktop, DetailLoad, Tab};
use crate::{platform, store_session::StoreSession};
use agent_core::{
    state::{Intent, Snapshot, operations as op},
    store::Outcome,
};
use agent_protocol::{
    ids::{ItemId, RequestId, TurnId},
    models::RemoteHost,
};
use gpui_kit::{Context, ImageSource, Window};
use std::{
    collections::HashSet,
    future::Future,
    path::{Path, PathBuf},
    sync::Arc,
};

pub(super) enum OperationCompletion {
    Refresh,
    Busy,
    Download,
    Composer(u64),
    Editor(u64),
    History {
        generation: u64,
    },
    Item {
        generation: u64,
        key: (TurnId, ItemId),
    },
    WorktreeSettings,
    Request(RequestId),
    Dictation(uuid::Uuid),
    RemoveWorktree,
    Account,
    Gallery(uuid::Uuid),
}
pub(super) enum Update {
    Connected(Result<(StoreSession, PathBuf), String>),
    Snapshot,
    Completed(OperationCompletion, Result<Outcome, String>),
    Folder(Result<Option<PathBuf>, String>),
    Image {
        key: String,
        result: Result<String, String>,
    },
    Recording(uuid::Uuid, platform::RecordingEvent),
    PersistenceError(String),
}
impl Desktop {
    pub(super) fn connect(&mut self) {
        self.epoch += 1;
        self.connecting = true;
        self.busy = 0;
        self.worktree_removal = None;
        self.worktree_busy = false;
        self.account_busy = false;
        self.account_polling = false;
        self.model_provider = None;
        self.account_sign_out = None;
        self.account_login_draft = None;
        self.error = self.runtime.logging_error.clone().unwrap_or_default();
        let epoch = self.epoch;
        let remote = self.remote.clone();
        let side = self.side_chat_mode;
        let initial_cwd = self.initial_cwd.take();
        let updates = self.updates.clone();
        let connections = self.runtime.connections.clone();
        let runtime = self.runtime.clone();
        self.runtime.handle.spawn(async move {
            let result = async {
                let host = if let Some(remote) = &remote {
                    remote
                        .ticket
                        .parse::<agent_transport::transport::Ticket>()
                        .map_err(|error| error.to_string())?
                        .node_id()
                        .to_string()
                } else {
                    "local".into()
                };
                let path = platform::state_dir()?.join(format!(
                    "desktop-{}-{host}.json",
                    if side { "side" } else { "main" }
                ));
                let mut snapshot: Snapshot = match tokio::fs::read(&path).await {
                    Ok(bytes) => agent_core::persistence::decode(&bytes)
                        .map_err(|error| format!("保存した入力状態を読み込めません: {error}"))?,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        Snapshot::default()
                    }
                    Err(error) => return Err(error.to_string()),
                };
                if let Some(cwd) = initial_cwd.or_else(|| {
                    snapshot
                        .navigation
                        .thread_id
                        .is_none()
                        .then(|| snapshot.navigation.cwd.clone())
                }) {
                    snapshot = agent_core::state::reduce(
                        &snapshot,
                        agent_core::state::Event::Intent(Intent::NewChat { cwd }),
                    )
                    .0;
                }
                let store = connections
                    .connect(
                        remote.as_ref().map(|remote| remote.ticket.as_str()),
                        snapshot,
                    )
                    .await
                    .map_err(|error| format!("{error:#}"))?;
                Ok::<_, String>((store, path))
            }
            .await;
            match result {
                Ok((store, path)) => {
                    StoreSession::publish(
                        Ok(store),
                        runtime,
                        updates,
                        move |session| {
                            (
                                epoch,
                                Update::Connected(session.map(|session| (session, path))),
                            )
                        },
                        move |_| (epoch, Update::Snapshot),
                    )
                    .await;
                }
                Err(error) => {
                    let _ = updates.send((epoch, Update::Connected(Err(error)))).await;
                }
            }
        });
    }
    pub(super) fn effect<T: Send + 'static>(
        &self,
        future: impl Future<Output = Result<T, String>> + Send + 'static,
        complete: impl FnOnce(Result<T, String>) -> Update + Send + 'static,
    ) {
        let epoch = self.epoch;
        let updates = self.updates.clone();
        self.runtime.handle.spawn(async move {
            let completion = complete(future.await);
            let _ = updates.send((epoch, completion)).await;
        });
    }
    pub(super) fn perform(&self, intent: Intent, completion: OperationCompletion) {
        let Some(store) = self.session.as_ref().map(|session| &session.store) else {
            return;
        };
        let receipt = store.dispatch(intent);
        self.effect(
            async move { receipt.await.map_err(|error| error.to_string()) },
            move |result| Update::Completed(completion, result),
        );
    }
    pub(super) fn dispatch(&self, intent: Intent) {
        self.perform(intent, OperationCompletion::Refresh);
    }
    pub(super) fn receive(
        &mut self,
        (epoch, update): (u64, Update),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if epoch != self.epoch {
            return;
        }
        match update {
            Update::Connected(result) => {
                self.connecting = false;
                match result {
                    Ok((mut session, path)) => {
                        session.persist(path, self.updates.clone(), move |error| {
                            (epoch, Update::PersistenceError(error))
                        });
                        self.session = Some(session);
                        self.accept_snapshot(window, cx);
                        if let Some(text) = self.pending_quote.take() {
                            self.quote_selection(&text, window, cx);
                        }
                        if let Some(text) = self.pending_explanation.take() {
                            self.explain_selection(&text, cx);
                        }
                        self.dispatch(Intent::ReadWorktreeSettings(op::ReadWorktreeSettings {}));
                        if self.tab == Tab::Settings {
                            self.dispatch(Intent::ListWorktrees(op::ListWorktrees {}));
                        }
                    }
                    Err(error) => self.set_error(error),
                }
            }
            Update::Snapshot => self.accept_snapshot(window, cx),
            Update::Completed(kind, result) => self.operation_completed(kind, result, window, cx),
            Update::Recording(id, event) => self.recording_update(id, event, window, cx),
            Update::PersistenceError(error) => self.set_error(error),
            Update::Folder(result) => {
                self.busy = self.busy.saturating_sub(1);
                match result {
                    Ok(Some(path)) => {
                        self.tab = Tab::Chat;
                        self.cancel_recording();
                        self.dispatch(Intent::AddProject(op::AddProject {
                            cwd: path.to_string_lossy().into_owned(),
                        }));
                    }
                    Ok(None) => {}
                    Err(error) => self.set_error(error),
                }
            }
            Update::Image { key, result } => {
                if let Some(image) = self.images.get_mut(&key) {
                    match result {
                        Ok(path) => {
                            image.path = Some(if Path::new(&path).is_absolute() {
                                PathBuf::from(path).into()
                            } else {
                                ImageSource::from(path)
                            })
                        }
                        Err(error) => {
                            tracing::error!(target: "bex", operation = "image.load", message = %error);
                            image.error = Some(error);
                        }
                    }
                }
                self.list.remeasure();
            }
        }
        cx.notify();
    }
    fn operation_completed(
        &mut self,
        kind: OperationCompletion,
        result: Result<Outcome, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match kind {
            OperationCompletion::Composer(revision) => {
                if self.composer_pending == Some(revision) {
                    self.composer_pending = None;
                }
            }
            OperationCompletion::Editor(revision) => {
                if self.editor_pending == Some(revision) {
                    self.editor_pending = None;
                }
            }
            OperationCompletion::Busy => {
                self.busy = self.busy.saturating_sub(1);
            }
            OperationCompletion::History { generation } => {
                if self.snapshot.epoch != generation {
                    return;
                }
                self.accept_snapshot(window, cx);
                self.history_loading = false;
                if let Err(error) = result {
                    self.history_error = error;
                }
                self.list.remeasure_items(0..1);
                return;
            }
            OperationCompletion::Item { generation, key } => {
                if self.snapshot.epoch != generation {
                    return;
                }
                self.item_details.insert(
                    key.clone(),
                    DetailLoad {
                        loading: false,
                        error: result.err(),
                    },
                );
                self.accept_snapshot(window, cx);
                self.remeasure_item(&key.0);
                return;
            }
            OperationCompletion::WorktreeSettings => {
                self.worktree_saving = false;
                if let Err(error) = result {
                    self.set_error(error);
                }
                self.accept_snapshot(window, cx);
                self.worktree_dirty = !self.settings_match_inputs(cx);
                self.worktree_saved = !self.worktree_dirty;
                if std::mem::take(&mut self.worktree_save_pending) {
                    self.save_worktree_settings(None, cx);
                }
                return;
            }
            OperationCompletion::Request(key) => {
                if let Err(error) = result {
                    self.set_error(error);
                    if let Some(inputs) = self.requests.get_mut(&key) {
                        inputs.sent = false;
                    }
                }
                self.accept_snapshot(window, cx);
                self.list.remeasure();
                return;
            }
            OperationCompletion::Download => {
                self.busy = self.busy.saturating_sub(1);
                if let Err(error) = result {
                    self.set_error(error);
                }
                return;
            }
            OperationCompletion::Account => {
                self.account_busy = false;
                if let Err(error) = result {
                    self.account_polling = false;
                    self.set_error(error);
                    self.dispatch(Intent::ListAccounts(op::ListAccounts {}));
                } else {
                    self.accept_snapshot(window, cx);
                    self.account_polling = self.snapshot.account.login.is_some();
                }
                if self.snapshot.account.login.is_none() {
                    self.account_login_draft = None;
                }
                return;
            }
            OperationCompletion::Refresh => {}
            OperationCompletion::RemoveWorktree => {
                self.worktree_busy = false;
                match result {
                    Ok(_) => self.worktree_removal = None,
                    Err(error) => {
                        self.set_error(error);
                        self.dispatch(Intent::ListWorktrees(op::ListWorktrees {}));
                    }
                }
                self.accept_snapshot(window, cx);
                return;
            }
            OperationCompletion::Dictation(id) => {
                if self.dictation.as_ref().is_some_and(|state| state.id == id) {
                    self.dictation = None;
                }
            }
            OperationCompletion::Gallery(id) => {
                let Some(gallery) = self
                    .image_gallery
                    .as_mut()
                    .filter(|gallery| gallery.id == id)
                else {
                    return;
                };
                gallery.loading = false;
                match result {
                    Ok(Outcome::SessionImages { images }) => {
                        let initial = gallery.current_image().clone();
                        let mut seen = HashSet::new();
                        let entries: Vec<_> = images
                            .into_iter()
                            .filter_map(|image| {
                                let entry = (Arc::new(image.source), image.encoded);
                                seen.insert(entry.clone()).then_some(entry)
                            })
                            .collect();
                        gallery.selected = entries.iter().position(|entry| entry == &initial);
                        gallery
                            .list
                            .splice(0..gallery.list.item_count(), entries.len());
                        gallery.entries = entries;
                        if let Some(index) = gallery.selected {
                            gallery.list.scroll_to_reveal_item(index);
                        }
                    }
                    Err(error) => {
                        tracing::error!(target: "bex", operation = "gallery.load", message = %error);
                        gallery.error = error;
                    }
                    _ => unreachable!("session image outcome"),
                }
                return;
            }
        }
        if let Err(error) = result {
            self.set_error(error);
        }
        self.accept_snapshot(window, cx);
    }
    pub(super) fn switch_host(
        &mut self,
        remote: Option<RemoteHost>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.remote == remote && self.snapshot.connected {
            return;
        }
        self.cancel_recording();
        self.dictation = None;
        self.session.take();
        self.remote = remote;
        self.snapshot = Arc::default();
        self.composer_pending = None;
        self.pending_quote = None;
        self.pending_explanation = None;
        self.editor_pending = None;
        self.editor_path = None;
        self.worktree_dirty = false;
        self.worktree_saving = false;
        self.worktree_save_pending = false;
        self.side_chat = None;
        self.terminal = None;
        self.images.clear();
        self.image_gallery = None;
        self.rows.clear();
        self.list.reset(0);
        self.rendered = None;
        self.item_details.clear();
        self.requests.clear();
        self.diffs.clear();
        self.markdown_cache.clear();
        self.composer_value = "".into();
        self.composer
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.connect();
    }
}

#[cfg(test)]
mod completion_tests {
    use super::{Arc, Context, Desktop, OperationCompletion, Outcome, Update, Window};
    use crate::app::Mode;
    use gpui_kit as gpui;
    use gpui_kit::TestAppContext;

    #[gpui::test]
    fn completion_messages_preserve_newer_host_navigation_and_input(cx: &mut TestAppContext) {
        // Do not drive this runtime: this test exercises UI result delivery, not provisioning.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(crate::Runtime {
                handle: runtime.handle().clone(),
                connections: Arc::new(crate::platform::Connections::default()),
                closing: tokio_util::task::TaskTracker::new(),
                logging_error: None,
            });
        });
        let view = cx.add_window(|window, cx| {
            Desktop::new(
                Mode::SideChat {
                    remote: None,
                    cwd: "/fixture".into(),
                },
                window,
                cx,
            )
        });
        view.update(cx, |view, window, cx| {
            view.epoch = 3;
            Arc::make_mut(&mut view.snapshot).epoch = 5;
            view.busy = 2;
            view.composer_pending = Some(9);
            view.editor_pending = Some(12);
            view.history_loading = true;
            let deliver = |view: &mut Desktop,
                           host,
                           kind,
                           result,
                           window: &mut Window,
                           cx: &mut Context<Desktop>| {
                view.receive((host, Update::Completed(kind, result)), window, cx);
            };
            deliver(
                view,
                2,
                OperationCompletion::Busy,
                Err("obsolete host".into()),
                window,
                cx,
            );
            assert_eq!(view.busy, 2);
            assert!(view.error.is_empty());
            deliver(
                view,
                3,
                OperationCompletion::Composer(8),
                Ok(Outcome::Applied),
                window,
                cx,
            );
            deliver(
                view,
                3,
                OperationCompletion::Editor(11),
                Ok(Outcome::Applied),
                window,
                cx,
            );
            assert_eq!(view.composer_pending, Some(9));
            assert_eq!(view.editor_pending, Some(12));
            deliver(
                view,
                3,
                OperationCompletion::History { generation: 4 },
                Err("obsolete history".into()),
                window,
                cx,
            );
            assert!(view.history_loading);
            assert!(view.history_error.is_empty());
            deliver(
                view,
                3,
                OperationCompletion::History { generation: 5 },
                Err("history failed".into()),
                window,
                cx,
            );
            assert!(!view.history_loading);
            assert_eq!(view.history_error, "history failed");
            deliver(
                view,
                3,
                OperationCompletion::Composer(9),
                Ok(Outcome::Applied),
                window,
                cx,
            );
            deliver(
                view,
                3,
                OperationCompletion::Editor(12),
                Ok(Outcome::Applied),
                window,
                cx,
            );
            assert_eq!(view.composer_pending, None);
            assert_eq!(view.editor_pending, None);
            deliver(
                view,
                3,
                OperationCompletion::Busy,
                Ok(Outcome::Applied),
                window,
                cx,
            );
            assert_eq!(view.busy, 1);
        })
        .unwrap();
    }
}

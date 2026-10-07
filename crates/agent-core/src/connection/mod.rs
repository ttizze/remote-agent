//! One Host connection: an owner task applies device changes, stream items and
//! completed requests in order and publishes immutable snapshots.
mod calls;
mod delivery;
mod device;
mod intents;
mod owner;
mod projects;
mod streams;
mod subscriptions;
mod terminals;

use crate::{peer::PeerError, protocol::Call, state::Snapshot, transport};
use agent_protocol::{models as m, operations as op};
use agent_transport::client::{Client, Updates};
use owner::{Event, FileTransfer, Owner};
use std::{sync::Arc, time::Duration};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

pub use owner::StoreOptions;

#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum Outcome {
    #[default]
    Applied,
    StartedThread {
        id: String,
    },
    RemoteHostPaired {
        id: String,
    },
    /// The composer text changed; the client moves its cursor here (UTF-16).
    ComposerEdited {
        cursor: u32,
    },
    /// The draft went to the stash; its images follow with
    /// `Intent::FinalizeStashImages`.
    Stashed {
        entry_id: String,
    },
    /// A restored stash entry's images, for the client to attach as files.
    StashRestored {
        images: Vec<crate::view::composer::stash::StashImage>,
        warning: Option<String>,
    },
    TerminalOpened {
        terminal_id: String,
    },
}
pub type Receipt = oneshot::Receiver<Result<Outcome, PeerError>>;

#[derive(Clone)]
pub struct Store {
    inner: Arc<Inner>,
}
struct Inner {
    sender: mpsc::Sender<Event>,
    intents: mpsc::UnboundedSender<Event>,
    snapshots: watch::Receiver<Arc<Snapshot>>,
    stop: CancellationToken,
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

pub(crate) fn invalid(error: impl std::fmt::Display) -> PeerError {
    PeerError::InvalidMessage(error.to_string())
}

impl Store {
    pub async fn connect(
        endpoint: &transport::Endpoint,
        ticket: &transport::Ticket,
        snapshot: Snapshot,
        options: StoreOptions,
        invitation: Option<uuid::Uuid>,
    ) -> Result<Self, PeerError> {
        let store = Self::offline(snapshot, options);
        store.reconnect(endpoint, ticket, invitation).await?;
        Ok(store)
    }

    /// Starts the owner with the restored device state and the disk cache.
    pub fn offline(snapshot: Snapshot, options: StoreOptions) -> Self {
        let (sender, receiver) = mpsc::channel(64);
        let (intents, input) = mpsc::unbounded_channel();
        let stop = CancellationToken::new();
        let (owner, snapshots) = Owner::new(snapshot, options, sender.clone());
        tokio::spawn(owner.run(input, receiver, stop.clone()));
        Self {
            inner: Arc::new(Inner {
                sender,
                intents,
                snapshots,
                stop,
            }),
        }
    }

    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.inner.snapshots.borrow().clone()
    }
    pub fn subscribe(&self) -> watch::Receiver<Arc<Snapshot>> {
        self.inner.snapshots.clone()
    }

    pub fn dispatch(&self, intent: crate::state::Intent) -> Receipt {
        let (sender, receiver) = oneshot::channel();
        if let Err(error) = self.inner.intents.send(Event::Intent(intent, sender)) {
            let reason = invalid(&error);
            if let Event::Intent(_, complete) = error.0 {
                let _ = complete.send(Err(reason));
            }
        }
        receiver
    }

    /// The app returned to the foreground: resume the subscriptions from their
    /// cursors.
    pub fn app_became_active(&self) {
        let _ = self.inner.intents.send(Event::AppActive);
    }

    pub async fn reconnect(
        &self,
        endpoint: &transport::Endpoint,
        ticket: &transport::Ticket,
        invitation: Option<uuid::Uuid>,
    ) -> Result<crate::diagnostics::ConnectionPerformance, PeerError> {
        let started = std::time::Instant::now();
        let session = endpoint.connect(ticket).await.map_err(invalid)?;
        let (peer, events) = session
            .open_peer(Duration::from_secs(30), 32)
            .await
            .map_err(invalid)?;
        let peer = Arc::new(peer);
        if let Some(invitation) = invitation {
            peer.call(&op::Pair { invitation }).await?;
        }
        let host_name = peer.request::<String>(&Call::HostName(m::Empty {})).await?;
        let (complete, receiver) = oneshot::channel();
        self.inner
            .sender
            .send(Event::Attach {
                peer: peer.clone(),
                host_name,
                ticket: ticket.clone(),
                session,
                events: Box::new(events),
                complete,
            })
            .await
            .map_err(invalid)?;
        receiver.await.map_err(invalid)?;
        Ok(crate::diagnostics::ConnectionPerformance {
            total_ms: started.elapsed().as_millis() as u64,
            connection_id: peer.diagnostic_id,
            ..Default::default()
        })
    }

    /// Reuses a responsive session on the same endpoint and ticket.
    pub async fn resume(
        &self,
        endpoint: &transport::Endpoint,
        ticket: &transport::Ticket,
    ) -> Result<crate::diagnostics::ConnectionPerformance, PeerError> {
        let (complete, result) = oneshot::channel();
        self.inner
            .sender
            .send(Event::Resume {
                endpoint: endpoint.clone(),
                ticket: ticket.clone(),
                complete,
            })
            .await
            .map_err(invalid)?;
        if let Some(performance) = result.await.map_err(invalid)? {
            return Ok(performance);
        }
        self.reconnect(endpoint, ticket, None).await
    }

    pub async fn close(&self) -> Result<(), PeerError> {
        let (sender, receiver) = oneshot::channel();
        self.inner
            .sender
            .send(Event::Close(sender))
            .await
            .map_err(invalid)?;
        receiver.await.map_err(invalid)
    }

    pub async fn browser(
        &self,
        request: crate::browser::BrowserRequest,
    ) -> Result<crate::browser::BrowserFrame, PeerError> {
        let (sender, receiver) = oneshot::channel();
        self.inner
            .sender
            .send(Event::Browser(request, sender))
            .await
            .map_err(invalid)?;
        receiver.await.map_err(invalid)?
    }

    pub fn prepare_dictation(&self) -> crate::client::DictationPreparation {
        let cancel = CancellationToken::new();
        let recording = format!("dictation:{}", uuid::Uuid::new_v4());
        let _ = self
            .inner
            .intents
            .send(Event::Dictation(recording.clone(), cancel.clone()));
        crate::client::DictationPreparation {
            id: recording,
            _cancel: cancel.drop_guard(),
        }
    }

    pub fn record_connection_performance(
        &self,
        performance: crate::diagnostics::ConnectionPerformance,
    ) {
        let _ = self.inner.sender.try_send(Event::Performance(performance));
    }

    pub async fn download_attachment(
        &self,
        id: String,
        destination: String,
    ) -> Result<(), PeerError> {
        self.transfer(FileTransfer::AttachmentDownload { id, destination })
            .await
            .map(|_| ())
    }
    pub async fn download_file(
        &self,
        source: String,
        destination: String,
    ) -> Result<(), PeerError> {
        self.transfer(FileTransfer::Download {
            source,
            destination,
        })
        .await
        .map(|_| ())
    }
    pub async fn upload_file(
        &self,
        source: String,
        directory: String,
        file_name: String,
    ) -> Result<String, PeerError> {
        self.transfer(FileTransfer::Upload {
            source,
            directory,
            file_name,
        })
        .await
    }
    async fn transfer(&self, transfer: FileTransfer) -> Result<String, PeerError> {
        let (sender, receive) = oneshot::channel();
        self.inner
            .sender
            .send(Event::Transfer(transfer, sender))
            .await
            .map_err(invalid)?;
        receive.await.map_err(invalid)?
    }
}

/// Forwards terminal notifications until the connection ends.
async fn notifications(mut events: Updates, epoch: u64, sender: mpsc::Sender<Event>) {
    loop {
        let event = match events.read::<crate::protocol::Notification>().await {
            Ok(Some(notification)) => Event::Notification(epoch, notification),
            Ok(None) => Event::Disconnected(epoch, "Host connection closed".into()),
            Err(error) => Event::Disconnected(epoch, error.to_string()),
        };
        let last = matches!(event, Event::Disconnected(..));
        if sender.send(event).await.is_err() || last {
            return;
        }
    }
}

type Peer = Arc<Client>;

#[cfg(test)]
mod tests;

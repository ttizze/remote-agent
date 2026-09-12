//! Real isolated Host startup shared by UI and transport tests.
use agent_core::{
    peer::RpcPeer,
    transport::{Endpoint, Identity, Relays, Session, Ticket},
};
use codex_app_server::{AppServerConfig, CodexAppServer};
use host_daemon::{
    CodexRpcService, CredentialStore, DesktopProjectStore, HostCredentials, HostRuntime,
};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::{CancellationToken, DropGuard};
use zeroize::Zeroizing;

#[derive(Default)]
pub struct Memory {
    bytes: Mutex<Option<Zeroizing<Vec<u8>>>>,
}
impl CredentialStore for Memory {
    fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
        Ok(self.bytes.lock().unwrap().clone())
    }
    fn save(&self, bytes: &[u8]) -> Result<(), String> {
        *self.bytes.lock().unwrap() = Some(Zeroizing::new(bytes.to_vec()));
        Ok(())
    }
}

pub struct HostFixture {
    pub server: Arc<CodexAppServer>,
    pub credentials: Arc<HostCredentials>,
    pub memory: Arc<Memory>,
    pub ticket: Ticket,
    pub stop: CancellationToken,
    pub running: tokio::task::JoinHandle<Result<(), String>>,
    _stop_on_drop: DropGuard,
}
impl HostFixture {
    pub async fn start(
        directory: &Path,
        config: AppServerConfig,
        memory: Arc<Memory>,
        name: &str,
        accounts: bool,
    ) -> Result<Self, String> {
        let credentials =
            Arc::new(HostCredentials::load(memory.clone(), directory.join("state")).await?);
        let endpoint = Endpoint::bind(credentials.host_identity().await, Relays::Disabled)
            .await
            .map_err(|error| error.to_string())?;
        let ticket = endpoint.ticket();
        let server = Arc::new(
            CodexAppServer::spawn(config.clone())
                .await
                .map_err(|error| error.to_string())?,
        );
        let service = CodexRpcService::new(
            server.clone(),
            DesktopProjectStore::new(directory.join("projects.json")),
        );
        if accounts {
            service
                .enable_accounts(directory.join("state/accounts"), config)
                .await?;
        }
        service.start();
        let runtime =
            Arc::new(HostRuntime::new(service, endpoint, credentials.clone(), name.into()).await);
        let stop = CancellationToken::new();
        let running = tokio::spawn(runtime.run(stop.clone()));
        Ok(Self {
            server,
            credentials,
            memory,
            ticket,
            _stop_on_drop: stop.clone().drop_guard(),
            stop,
            running,
        })
    }

    pub async fn connect(&self, identity: Identity) -> crate::Result<Connection> {
        Connection::open(&self.ticket, identity).await
    }

    pub async fn local(&self) -> crate::Result<Connection> {
        self.connect(self.credentials.local_identity().await).await
    }

    pub async fn close(self) -> Result<(), String> {
        self.stop.cancel();
        self.running.await.map_err(|error| error.to_string())??;
        Arc::try_unwrap(self.server)
            .map_err(|_| "Codex process retained")?
            .shutdown()
            .await
            .map_err(|error| error.to_string())
    }
}

pub struct Connection {
    pub endpoint: Endpoint,
    pub session: Session,
    pub peer: RpcPeer,
}

impl Connection {
    pub async fn open(ticket: &Ticket, identity: Identity) -> crate::Result<Self> {
        let endpoint = Endpoint::bind(identity, Relays::Disabled).await?;
        let opened: crate::Result<_> = async {
            let session = endpoint.connect(ticket).await?;
            let peer = session.open_peer(Duration::from_secs(10), 128).await?;
            Ok((session, peer))
        }
        .await;
        match opened {
            Ok((session, peer)) => Ok(Self {
                endpoint,
                session,
                peer,
            }),
            Err(error) => {
                endpoint.close().await;
                Err(error)
            }
        }
    }

    pub async fn close(self) -> crate::Result<()> {
        let closed = self.peer.close().await;
        self.session.close();
        self.endpoint.close().await;
        Ok(closed?)
    }
}

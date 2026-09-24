//! Real isolated Host startup shared by UI and transport tests.
use agent_transport::{
    client::Client,
    transport::{Endpoint, Identity, Relays, Session, Ticket},
};
use anyhow::{Context, Result};
use codex_app_server::{AppServerConfig, CodexAppServer};
use host_daemon::{CredentialStore, HostCredentials, HostRpcService, HostRuntime, ProjectStore};
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
    fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>> {
        Ok(self.bytes.lock().unwrap().clone())
    }
    fn save(&self, bytes: &[u8]) -> Result<()> {
        *self.bytes.lock().unwrap() = Some(Zeroizing::new(bytes.to_vec()));
        Ok(())
    }
}

pub struct HostFixture {
    server: Option<Arc<CodexAppServer>>,
    pub credentials: Arc<HostCredentials>,
    pub memory: Arc<Memory>,
    pub ticket: Ticket,
    pub stop: CancellationToken,
    pub running: tokio::task::JoinHandle<Result<()>>,
    _stop_on_drop: DropGuard,
}
impl HostFixture {
    pub async fn start(
        directory: &Path,
        config: AppServerConfig,
        memory: Arc<Memory>,
        name: &str,
        accounts: bool,
        claude: Option<&Path>,
    ) -> Result<Self> {
        #[cfg(unix)]
        {
            use rustix::process::{Resource, getrlimit, setrlimit};
            // The 80-client admission test alone exceeds macOS's default 256 FDs.
            // Leave room for the other isolated Hosts in the parallel test suite.
            let mut limit = getrlimit(Resource::Nofile);
            if limit.current.is_some_and(|current| current < 4096) {
                limit.current = Some(4096);
                setrlimit(Resource::Nofile, limit).with_context(|| {
                    format!(
                        "Host fixtures require 4096 file descriptors (hard limit {:?})",
                        limit.maximum
                    )
                })?;
            }
        }
        let credentials =
            Arc::new(HostCredentials::load(memory.clone(), directory.join("state")).await?);
        let endpoint = Endpoint::bind(credentials.host_identity().await, Relays::Disabled).await?;
        let ticket = endpoint.ticket();
        let server = CodexAppServer::spawn(config.clone())
            .await
            .map(Arc::new)
            .map_err(|error| error.to_string());
        let service = HostRpcService::new(
            server.clone(),
            ProjectStore::new(directory.join("bex-worktrees.json")),
        );
        #[cfg(unix)]
        service
            .enable_browser(directory.join("browser"))
            .await
            .map_err(anyhow::Error::msg)?;
        if let Some(program) = claude {
            service
                .enable_claude(
                    program.to_owned(),
                    directory.join("claude"),
                    Some(directory.join("claude-native")),
                )
                .await?;
        }
        if accounts && server.is_ok() {
            service
                .enable_accounts(directory.join("state/accounts"), config)
                .await
                .map_err(anyhow::Error::msg)?;
        }
        service.start();
        let runtime = Arc::new(
            HostRuntime::new(
                service,
                endpoint,
                credentials.clone(),
                name.into(),
                std::time::Duration::from_secs(7 * 24 * 60 * 60),
            )
            .await,
        );
        let stop = CancellationToken::new();
        let running = tokio::spawn(runtime.run(stop.clone()));
        Ok(Self {
            server: server.ok(),
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

    pub async fn close(self) -> Result<()> {
        self.stop.cancel();
        self.running.await??;
        if let Some(server) = self.server {
            server.shutdown().await?;
        }
        Ok(())
    }
}

pub struct Connection {
    pub endpoint: Endpoint,
    pub session: Session,
    pub peer: Client,
    pub events: agent_transport::framing::Reader,
}

impl Connection {
    pub async fn open(ticket: &Ticket, identity: Identity) -> crate::Result<Self> {
        let endpoint = Endpoint::bind(identity, Relays::Disabled).await?;
        let opened: crate::Result<_> = async {
            let session = endpoint.connect(ticket).await?;
            let (peer, events) = session.open_peer(Duration::from_secs(10), 128).await?;
            Ok((session, peer, events))
        }
        .await;
        match opened {
            Ok((session, peer, events)) => Ok(Self {
                endpoint,
                session,
                peer,
                events,
            }),
            Err(error) => {
                endpoint.close().await;
                Err(error)
            }
        }
    }

    pub async fn close(self) {
        self.peer.close().await;
        self.session.close();
        self.endpoint.close().await;
    }
}

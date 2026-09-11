//! Real isolated Host startup shared by UI and transport tests.
use agent_core::transport::{Endpoint, Relays, Ticket};
use codex_app_server::{AppServerConfig, CodexAppServer};
use host_daemon::{
    CodexRpcService, CredentialStore, DesktopProjectStore, HostCredentials, HostRuntime,
};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;
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
            stop,
            running,
        })
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

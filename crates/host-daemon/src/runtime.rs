use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use host_daemon::{
    CodexRpcService, DesktopProjectStore, DeviceAuthenticationState, MdnsAdvertisement,
    load_or_create_host_identity, load_settings,
};
use russh::{
    MethodKind, MethodSet,
    server::{self, Server},
};
use tokio::{net::TcpListener, sync::Mutex};

use crate::command_line::StartupConfig;

mod jsonl_session;
mod ssh;

const PAIRING_TICKET_TTL_MS: u64 = 10 * 60_000;
const SESSION_QUEUE_CAPACITY: usize = 128;
const MAX_IN_FLIGHT_REQUESTS: usize = 8;

#[cfg(target_os = "macos")]
pub(crate) async fn run(config: StartupConfig) -> Result<(), StartupError> {
    use host_daemon::MacOsKeychainHostIdentityStore;

    let settings = load_settings(&config.settings)
        .map_err(|error| StartupError::Settings(error.to_string()))?;
    let keychain =
        MacOsKeychainHostIdentityStore::new(config.keychain_service, config.keychain_account);
    let host_identity = Arc::new(
        load_or_create_host_identity(&keychain)
            .map_err(|error| StartupError::HostIdentity(error.to_string()))?,
    );
    let mut authentication_state = DeviceAuthenticationState::new(settings, config.settings);
    if !config.pair_addresses.is_empty() {
        let expires_at_ms = unix_time_millis()
            .checked_add(PAIRING_TICKET_TTL_MS)
            .ok_or(StartupError::PairingExpiryOverflow)?;
        let payload = authentication_state
            .issue_pairing_ticket(
                host_identity.public_key(),
                config.pair_addresses,
                expires_at_ms,
            )
            .map_err(|error| StartupError::Pairing(error.to_string()))?;
        let payload = serde_json::to_string(&payload)
            .map_err(|error| StartupError::PairingPayload(error.to_string()))?;
        eprintln!("Bex pairing payload: {payload}");
    }
    let authentication = Arc::new(Mutex::new(authentication_state));

    // Start the one shared Codex process before binding and accepting phones.
    let app_server = Arc::new(
        codex_app_server::CodexAppServer::spawn(codex_app_server::AppServerConfig {
            program: config.codex,
            ..codex_app_server::AppServerConfig::default()
        })
        .await
        .map_err(|error| StartupError::Codex(error.to_string()))?,
    );
    let desktop_projects = DesktopProjectStore::from_environment()
        .map_err(|error| StartupError::DesktopProjects(error.to_string()))?;
    eprintln!(
        "Bex Host reading Codex Desktop projects from {}",
        desktop_projects.path().display()
    );
    let service = CodexRpcService::new(app_server.clone(), desktop_projects);
    service.start();

    let listener = TcpListener::bind(config.listen)
        .await
        .map_err(StartupError::Listen)?;
    let listener_address = listener.local_addr().map_err(StartupError::Listen)?;
    let mdns = MdnsAdvertisement::register(host_identity.public_key(), listener_address.port())
        .map_err(|error| StartupError::Mdns(error.to_string()))?;

    let ssh_config = server::Config {
        methods: MethodSet::from(&[MethodKind::PublicKey][..]),
        keys: vec![host_identity.server_key()],
        inactivity_timeout: None,
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 3,
        channel_buffer_size: SESSION_QUEUE_CAPACITY,
        event_buffer_size: SESSION_QUEUE_CAPACITY,
        nodelay: true,
        ..server::Config::default()
    };

    let session_tasks = Arc::new(Mutex::new(Vec::new()));
    let mut ssh_server = ssh::GatewayServer::new(
        host_identity,
        authentication,
        service.clone(),
        session_tasks.clone(),
    );
    let mut running = ssh_server.run_on_socket(Arc::new(ssh_config), &listener);
    eprintln!("Bex Host listening on {listener_address}");

    let server_result = tokio::select! {
        result = &mut running => result.map_err(StartupError::Server),
        signal = tokio::signal::ctrl_c() => {
            if let Err(error) = signal {
                eprintln!("failed to wait for Ctrl-C: {error}");
            }
            running.handle().shutdown("host daemon shutting down".to_owned());
            (&mut running).await.map_err(StartupError::Server)
        }
    };
    drop(mdns);
    drop(running);

    // The JSONL channels run in detached tasks because russh's handler callback
    // must return immediately. Join them before releasing the service so no
    // phone can retain the shared Codex process during Host shutdown.
    drop(ssh_server);
    wait_for_session_tasks(session_tasks).await;
    server_result?;

    drop(service);
    let app_server = Arc::try_unwrap(app_server).map_err(|_| StartupError::ActiveConnection)?;
    app_server
        .shutdown()
        .await
        .map_err(|error| StartupError::CodexShutdown(error.to_string()))
}

#[cfg(not(target_os = "macos"))]
pub(crate) async fn run(_config: StartupConfig) -> Result<(), StartupError> {
    Err(StartupError::UnsupportedPlatform)
}

async fn wait_for_session_tasks(tasks: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>) {
    // Host shutdown is the cancellation boundary for every phone. Abort and
    // await each detached task so its service and Codex references are gone
    // before the shared App Server is stopped.
    loop {
        let handles = {
            let mut tasks = tasks.lock().await;
            std::mem::take(&mut *tasks)
        };
        if handles.is_empty() {
            return;
        }
        for handle in handles {
            handle.abort();
            let _ = handle.await;
        }
    }
}

#[cfg(target_os = "macos")]
fn unix_time_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(not(target_os = "macos"))]
fn unix_time_millis() -> u64 {
    0
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum StartupError {
    #[cfg(not(target_os = "macos"))]
    #[error("macOS Keychain-backed Host identity is not implemented on this platform")]
    UnsupportedPlatform,
    #[error("failed to load Host settings: {0}")]
    Settings(String),
    #[error("failed to load or create Host identity: {0}")]
    HostIdentity(String),
    #[error("failed to issue pairing ticket: {0}")]
    Pairing(String),
    #[error("pairing ticket expiry overflowed")]
    PairingExpiryOverflow,
    #[error("failed to serialize pairing payload: {0}")]
    PairingPayload(String),
    #[error("failed to bind SSH listener: {0}")]
    Listen(#[source] std::io::Error),
    #[error("SSH server failed: {0}")]
    Server(#[source] std::io::Error),
    #[error("failed to start Codex App Server: {0}")]
    Codex(String),
    #[error("failed to locate Codex Desktop project state: {0}")]
    DesktopProjects(String),
    #[error("failed to advertise Host over mDNS: {0}")]
    Mdns(String),
    #[error("an active SSH service retained the Codex App Server during shutdown")]
    ActiveConnection,
    #[error("failed to shut down Codex App Server: {0}")]
    CodexShutdown(String),
}

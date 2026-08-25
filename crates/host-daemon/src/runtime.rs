use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use codex_app_server::{AppServerConfig, CodexAppServer};
use host_daemon::{
    CodexRpcService, DesktopProjectStore, DeviceAuthenticationState, HOST_PROJECT_METHODS,
    HostIdentity, MdnsAdvertisement, RpcServerConfig, accept_rpc_channel,
    authenticate_incoming_channel, load_or_create_host_identity, load_settings,
    serve_gateway_messages,
};
use host_protocol::{
    ConnectionLimits, DEFAULT_MAX_FRAME_BYTES, ProtocolRange, TransportCertificateHash,
};
use quinn::{Endpoint, ServerConfig, VarInt};
use rcgen::generate_simple_self_signed;
use ring::digest::{SHA256, digest};
use rustls::pki_types::PrivatePkcs8KeyDer;
use tokio::{
    sync::{Mutex, mpsc},
    task::JoinSet,
    time::{Duration, timeout},
};

use crate::command_line::StartupConfig;

const CHALLENGE_TTL_MS: u64 = 60_000;
const PAIRING_TICKET_TTL_MS: u64 = 10 * 60_000;
const NOTIFICATION_QUEUE_CAPACITY: usize = 128;

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
    let mut authentication_state =
        DeviceAuthenticationState::new(settings, config.settings, CHALLENGE_TTL_MS);
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

    // The transport certificate is intentionally short-lived process state.
    // Mobile Clients authenticate the stable Host identity in the RPC hello,
    // not this self-signed certificate.
    let TransportEndpoint {
        endpoint,
        transport_certificate_hash,
    } = make_endpoint(config.listen)?;
    let app_server = Arc::new(
        CodexAppServer::spawn(AppServerConfig {
            program: config.codex,
            ..AppServerConfig::default()
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
    let service = Arc::new(CodexRpcService::new(app_server.clone(), desktop_projects));
    let mut supported_methods = app_server
        .supported_methods()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    supported_methods.extend(
        HOST_PROJECT_METHODS
            .iter()
            .map(|method| (*method).to_owned()),
    );
    let rpc_config = Arc::new(rpc_server_config(
        transport_certificate_hash,
        supported_methods,
    ));
    let listener_address = endpoint.local_addr().map_err(StartupError::Listen)?;
    let mdns = MdnsAdvertisement::register(host_identity.public_key(), listener_address.port())
        .map_err(|error| StartupError::Mdns(error.to_string()))?;

    eprintln!("Bex Host listening on {listener_address}");
    let mut connection_tasks = accept_loop(
        &endpoint,
        host_identity,
        authentication,
        rpc_config,
        service.clone(),
    )
    .await;

    endpoint.close(VarInt::from_u32(0), b"host daemon shutting down");
    drop(mdns);
    shutdown_connection_tasks(&mut connection_tasks).await;
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

#[cfg(target_os = "macos")]
async fn accept_loop(
    endpoint: &Endpoint,
    host_identity: Arc<HostIdentity>,
    authentication: Arc<Mutex<DeviceAuthenticationState>>,
    rpc_config: Arc<RpcServerConfig>,
    service: Arc<CodexRpcService>,
) -> JoinSet<()> {
    let mut connection_tasks = JoinSet::new();

    loop {
        while let Some(result) = connection_tasks.try_join_next() {
            report_connection_task(result);
        }

        tokio::select! {
            signal = tokio::signal::ctrl_c() => {
                if let Err(error) = signal {
                    eprintln!("failed to wait for Ctrl-C: {error}");
                }
                return connection_tasks;
            }
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else {
                    return connection_tasks;
                };
                connection_tasks.spawn(serve_connection(
                    incoming,
                    host_identity.clone(),
                    authentication.clone(),
                    rpc_config.clone(),
                    service.clone(),
                ));
            }
        }
    }
}

#[cfg(target_os = "macos")]
async fn serve_connection(
    incoming: quinn::Incoming,
    host_identity: Arc<HostIdentity>,
    authentication: Arc<Mutex<DeviceAuthenticationState>>,
    rpc_config: Arc<RpcServerConfig>,
    service: Arc<CodexRpcService>,
) {
    let result =
        serve_connection_inner(incoming, host_identity, authentication, rpc_config, service).await;
    if let Err(error) = result {
        eprintln!("closed Mobile Client connection: {error}");
    }
}

#[cfg(target_os = "macos")]
async fn serve_connection_inner(
    incoming: quinn::Incoming,
    host_identity: Arc<HostIdentity>,
    authentication: Arc<Mutex<DeviceAuthenticationState>>,
    rpc_config: Arc<RpcServerConfig>,
    service: Arc<CodexRpcService>,
) -> Result<(), StartupError> {
    let connection = incoming
        .await
        .map_err(|error| StartupError::Connection(error.to_string()))?;
    let (pending_channel, _) = accept_rpc_channel(&connection, &rpc_config, &host_identity)
        .await
        .map_err(|error| StartupError::Connection(error.to_string()))?;
    let (channel, _) = authenticate_incoming_channel(
        pending_channel,
        &host_identity,
        authentication,
        unix_time_millis,
    )
    .await
    .map_err(|error| StartupError::Connection(error.to_string()))?;

    let mut session = service.open_session(NOTIFICATION_QUEUE_CAPACITY);
    let session_id = session.id();
    let (outbound_sender, outbound_receiver) = mpsc::channel(NOTIFICATION_QUEUE_CAPACITY);
    let outbound_forwarder = tokio::spawn(async move {
        while let Some(message) = session.recv().await {
            if outbound_sender.send(message).await.is_err() {
                return;
            }
        }
    });

    let request_service = service.clone();
    let response_service = service.clone();
    let notification_service = service.clone();
    let result = serve_gateway_messages(
        channel,
        &rpc_config,
        move |request| {
            let service = request_service.clone();
            async move {
                let _ = service.dispatch_request(session_id, request).await;
            }
        },
        move |response| {
            let service = response_service.clone();
            async move {
                let _ = service.dispatch_response(session_id, response).await;
            }
        },
        move |notification| {
            let service = notification_service.clone();
            async move {
                let _ = service
                    .dispatch_notification(session_id, notification)
                    .await;
            }
        },
        outbound_receiver,
    )
    .await;
    service.close_session(session_id);
    let _ = outbound_forwarder.await;
    result.map_err(|error| StartupError::Connection(error.to_string()))?;
    connection.close(VarInt::from_u32(0), b"RPC stream closed");
    Ok(())
}

#[cfg(target_os = "macos")]
fn report_connection_task(result: Result<(), tokio::task::JoinError>) {
    if let Err(error) = result {
        eprintln!("Mobile Client connection task failed: {error}");
    }
}

#[cfg(target_os = "macos")]
async fn shutdown_connection_tasks(connection_tasks: &mut JoinSet<()>) {
    let completed = timeout(Duration::from_secs(5), async {
        while let Some(result) = connection_tasks.join_next().await {
            report_connection_task(result);
        }
    })
    .await;

    if completed.is_err() {
        connection_tasks.abort_all();
        while let Some(result) = connection_tasks.join_next().await {
            report_connection_task(result);
        }
    }
}

struct TransportEndpoint {
    endpoint: Endpoint,
    transport_certificate_hash: TransportCertificateHash,
}

fn make_endpoint(listen: std::net::SocketAddr) -> Result<TransportEndpoint, StartupError> {
    let certificate = generate_simple_self_signed(vec!["bex-host".to_owned()])
        .map_err(|error| StartupError::Certificate(error.to_string()))?;
    let certificate_der = certificate.cert.der().clone();
    let transport_certificate_hash = TransportCertificateHash::from_bytes(
        digest(&SHA256, certificate_der.as_ref())
            .as_ref()
            .try_into()
            .expect("SHA-256 digests always contain 32 bytes"),
    );
    let key = PrivatePkcs8KeyDer::from(certificate.key_pair.serialize_der());
    let server_config = ServerConfig::with_single_cert(vec![certificate_der], key.into())
        .map_err(|error| StartupError::Certificate(error.to_string()))?;
    Ok(TransportEndpoint {
        endpoint: Endpoint::server(server_config, listen).map_err(StartupError::Listen)?,
        transport_certificate_hash,
    })
}

fn rpc_server_config(
    transport_certificate_hash: TransportCertificateHash,
    supported_methods: Vec<String>,
) -> RpcServerConfig {
    RpcServerConfig {
        versions: ProtocolRange::CURRENT,
        limits: ConnectionLimits {
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
            max_in_flight_requests: 8,
            outbound_queue_messages: 128,
            request_timeout_ms: 30_000,
        },
        supported_methods,
        transport_certificate_hash,
    }
}

#[cfg(target_os = "macos")]
fn unix_time_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
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
    #[error("failed to create QUIC transport certificate: {0}")]
    Certificate(String),
    #[error("failed to bind QUIC listener: {0}")]
    Listen(#[source] std::io::Error),
    #[error("failed to start Codex App Server: {0}")]
    Codex(String),
    #[error("failed to locate Codex Desktop project state: {0}")]
    DesktopProjects(String),
    #[error("failed to advertise Host over mDNS: {0}")]
    Mdns(String),
    #[error("mobile connection failed: {0}")]
    Connection(String),
    #[error("an active RPC service retained the Codex App Server during shutdown")]
    ActiveConnection,
    #[error("failed to shut down Codex App Server: {0}")]
    CodexShutdown(String),
}

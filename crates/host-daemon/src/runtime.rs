use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use host_daemon::{
    CodexRpcService, DesktopProjectStore, DeviceAuthenticationState, HostIdentity,
    MdnsAdvertisement, load_or_create_host_identity, load_settings,
};
use host_protocol::{
    DEFAULT_MAX_MESSAGE_BYTES, Ed25519PublicKey, JsonlReader, JsonlWriter, RpcMessageKind,
    SSH_SUBSYSTEM, classify_message,
};
use russh::{
    Channel, ChannelId, MethodKind, MethodSet, Sig,
    keys::{Algorithm, PublicKey},
    server::{self, Auth, ChannelOpenHandle, Handler, Msg, Server, Session},
};
use tokio::{
    io::split,
    net::TcpListener,
    sync::{Mutex, Semaphore},
    task::JoinSet,
};

use crate::command_line::StartupConfig;

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
    let mut ssh_server = GatewayServer {
        host_identity,
        authentication,
        service: service.clone(),
        session_tasks: session_tasks.clone(),
    };
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

struct GatewayServer {
    host_identity: Arc<HostIdentity>,
    authentication: Arc<Mutex<DeviceAuthenticationState>>,
    service: CodexRpcService,
    session_tasks: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}

impl Server for GatewayServer {
    type Handler = GatewayHandler;

    fn new_client(&mut self, _peer_addr: Option<std::net::SocketAddr>) -> Self::Handler {
        GatewayHandler {
            host_identity: self.host_identity.clone(),
            authentication: self.authentication.clone(),
            service: self.service.clone(),
            session_tasks: self.session_tasks.clone(),
            identity: None,
            channels: HashMap::new(),
        }
    }

    fn handle_session_error(&mut self, error: <Self::Handler as Handler>::Error) {
        eprintln!("SSH session failed: {error}");
    }
}

struct GatewayHandler {
    host_identity: Arc<HostIdentity>,
    authentication: Arc<Mutex<DeviceAuthenticationState>>,
    service: CodexRpcService,
    session_tasks: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    identity: Option<Ed25519PublicKey>,
    channels: HashMap<ChannelId, Channel<Msg>>,
}

#[derive(Debug, thiserror::Error)]
enum HandlerError {
    #[error("SSH error: {0}")]
    Ssh(#[from] russh::Error),
}

impl Handler for GatewayHandler {
    type Error = HandlerError;

    async fn auth_publickey_offered(
        &mut self,
        _user: &str,
        public_key: &PublicKey,
    ) -> Result<Auth, Self::Error> {
        if public_key.key_data().algorithm() == Algorithm::Ed25519 {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        public_key: &PublicKey,
    ) -> Result<Auth, Self::Error> {
        let Some(key) = public_key.key_data().ed25519() else {
            return Ok(Auth::reject());
        };
        let identity = Ed25519PublicKey::from_bytes(*key.as_ref());
        let accepted = self.authentication.lock().await.authenticate(
            user,
            identity,
            self.host_identity.public_key(),
            unix_time_millis(),
        );
        match accepted {
            Ok(_) => {
                self.identity = Some(identity);
                Ok(Auth::Accept)
            }
            Err(_error) => {
                // Authentication failures are ordinary SSH rejects. Avoid
                // leaking whether a ticket, device, or settings path failed.
                Ok(Auth::reject())
            }
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let id = channel.id();
        reply.accept().await;
        self.channels.insert(id, channel);
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let Some(channel_stream) = self.channels.remove(&channel) else {
            session.channel_failure(channel)?;
            return Ok(());
        };
        if self.identity.is_none() {
            session.channel_failure(channel)?;
            return Ok(());
        }
        if !is_supported_subsystem(name) {
            session.channel_failure(channel)?;
            return Ok(());
        }
        session.channel_success(channel)?;
        let service = self.service.clone();
        let task = tokio::spawn(async move {
            if let Err(error) = serve_jsonl_session(channel_stream, service).await {
                eprintln!("SSH JSONL session closed: {error}");
            }
        });
        let mut tasks = self.session_tasks.lock().await;
        tasks.retain(|task| !task.is_finished());
        tasks.push(task);
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_failure(channel)?;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        _data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_failure(channel)?;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        _col_width: u32,
        _row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_failure(channel)?;
        Ok(())
    }

    async fn x11_request(
        &mut self,
        channel: ChannelId,
        _single_connection: bool,
        _x11_auth_protocol: &str,
        _x11_auth_cookie: &str,
        _x11_screen_number: u32,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_failure(channel)?;
        Ok(())
    }

    async fn env_request(
        &mut self,
        channel: ChannelId,
        _variable_name: &str,
        _variable_value: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_failure(channel)?;
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        channel: ChannelId,
        _col_width: u32,
        _row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_failure(channel)?;
        Ok(())
    }

    async fn signal(
        &mut self,
        channel: ChannelId,
        _signal: Sig,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_failure(channel)?;
        Ok(())
    }

    async fn agent_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<bool, Self::Error> {
        session.channel_failure(channel)?;
        Ok(false)
    }
}

async fn serve_jsonl_session(
    channel: Channel<Msg>,
    service: CodexRpcService,
) -> Result<(), String> {
    let session = service.open_session(SESSION_QUEUE_CAPACITY);
    let session_id = session.id();
    let stream = channel.into_stream();
    let (reader, writer) = split(stream);
    let mut reader = JsonlReader::with_max_message_bytes(reader, DEFAULT_MAX_MESSAGE_BYTES);
    let mut writer = JsonlWriter::with_max_message_bytes(writer, DEFAULT_MAX_MESSAGE_BYTES);
    let mut session = session;
    let permits = Arc::new(Semaphore::new(MAX_IN_FLIGHT_REQUESTS));
    let mut tasks = JoinSet::<Result<(), String>>::new();

    let result = loop {
        let mut task_error = None;
        while let Some(result) = tasks.try_join_next() {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    task_error = Some(error);
                    break;
                }
                Err(error) => {
                    task_error = Some(format!("request task failed: {error}"));
                    break;
                }
            }
        }
        if let Some(error) = task_error {
            break Err(error);
        }

        tokio::select! {
            incoming = reader.read_line() => {
                let Some(line) = incoming.map_err(|error| error.to_string())? else {
                    break Ok(());
                };
                let message = classify_message(&line)
                    .map_err(|error| format!("invalid JSONL message: {error}"))?;
                match message.kind() {
                    RpcMessageKind::Request => {
                        let Ok(permit) = permits.clone().try_acquire_owned() else {
                            break Err("maximum in-flight request count reached".to_owned());
                        };
                        let service = service.clone();
                        tasks.spawn(async move {
                            let _permit = permit;
                            service
                                .dispatch_request(session_id, line)
                                .await
                                .map_err(|error| error.to_string())
                        });
                    }
                    RpcMessageKind::Response => {
                        service
                            .dispatch_response(session_id, line)
                            .await
                            .map_err(|error| error.to_string())?;
                    }
                    RpcMessageKind::Notification => {
                        service
                            .dispatch_notification(session_id, line)
                            .await
                            .map_err(|error| error.to_string())?;
                    }
                }
            }
            outgoing = session.recv() => {
                let Some(line) = outgoing else {
                    break Err("session outbound queue closed".to_owned());
                };
                writer.write_line(&line).await.map_err(|error| error.to_string())?;
            }
        }
    };

    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    result
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

fn is_supported_subsystem(name: &str) -> bool {
    name == SSH_SUBSYSTEM
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_the_remote_agent_subsystem() {
        assert!(is_supported_subsystem(SSH_SUBSYSTEM));
        assert!(!is_supported_subsystem("sftp"));
        assert!(!is_supported_subsystem("shell"));
    }
}

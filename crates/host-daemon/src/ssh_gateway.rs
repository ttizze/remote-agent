use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use host_protocol::{BLOB_SUBSYSTEM, Ed25519PublicKey, SSH_SUBSYSTEM};
use relay_transport::IncomingConnection;
use russh::{
    Channel, ChannelId, MethodKind, MethodSet, Sig,
    keys::{Algorithm, PublicKey},
    server::{self, Auth, ChannelOpenHandle, Handler, Msg, Session},
};
use tokio::{
    sync::{Mutex, Notify},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

use crate::{
    CodexRpcService, DeviceAuthenticationState, HostIdentity, jsonl_session::serve_jsonl_session,
};

pub struct EncryptedGateway {
    config: Arc<server::Config>,
    authentication: Arc<Mutex<DeviceAuthenticationState>>,
    service: CodexRpcService,
}

impl EncryptedGateway {
    pub fn new(
        identity: &HostIdentity,
        authentication: Arc<Mutex<DeviceAuthenticationState>>,
        service: CodexRpcService,
    ) -> Self {
        let config = server::Config {
            methods: MethodSet::from(&[MethodKind::PublicKey][..]),
            keys: vec![identity.server_key()],
            inactivity_timeout: None,
            keepalive_interval: Some(Duration::from_secs(20)),
            keepalive_max: 3,
            channel_buffer_size: 32,
            event_buffer_size: 128,
            ..server::Config::default()
        };
        Self {
            config: Arc::new(config),
            authentication,
            service,
        }
    }

    pub async fn serve(&self, connection: IncomingConnection) -> Result<(), String> {
        let _disconnect = connection.disconnect.clone().drop_guard();
        let authenticated = Arc::new(Notify::new());
        let tasks = Arc::new(Mutex::new(JoinSet::new()));
        let handler = GatewayHandler {
            authentication: self.authentication.clone(),
            authenticated: authenticated.clone(),
            disconnect: connection.disconnect.clone(),
            service: self.service.clone(),
            identity: None,
            channels: HashMap::new(),
            tasks: tasks.clone(),
            rpc_session_id: None,
        };
        let result = async {
            let mut session = tokio::time::timeout(
                Duration::from_secs(20),
                server::run_stream(self.config.clone(), connection.stream, handler),
            )
            .await
            .map_err(|_| "encrypted handshake timed out")?
            .map_err(|error| error.to_string())?;
            tokio::select! {
                result = &mut session => return result.map_err(|error| error.to_string()),
                _ = authenticated.notified() => {},
                _ = tokio::time::sleep(Duration::from_secs(20)) => {
                    connection.disconnect.cancel();
                    return session.await.map_err(|error| error.to_string());
                }
            }
            session.await.map_err(|error| error.to_string())
        }
        .await;
        connection.disconnect.cancel();
        // The endpoint's cancellation lets each subsystem finish its own
        // request-task cleanup before releasing the shared Codex process.
        let mut tasks = tasks.lock().await;
        while tasks.join_next().await.is_some() {}
        result
    }
}

struct GatewayHandler {
    authentication: Arc<Mutex<DeviceAuthenticationState>>,
    authenticated: Arc<Notify>,
    disconnect: CancellationToken,
    service: CodexRpcService,
    identity: Option<Ed25519PublicKey>,
    channels: HashMap<ChannelId, Channel<Msg>>,
    tasks: Arc<Mutex<JoinSet<Result<(), String>>>>,
    rpc_session_id: Option<crate::SessionId>,
}

impl Handler for GatewayHandler {
    type Error = russh::Error;

    async fn auth_publickey_offered(
        &mut self,
        _user: &str,
        public_key: &PublicKey,
    ) -> Result<Auth, Self::Error> {
        Ok(if public_key.algorithm() == Algorithm::Ed25519 {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        public_key: &PublicKey,
    ) -> Result<Auth, Self::Error> {
        let Some(public_key) = public_key.key_data().ed25519() else {
            return Ok(Auth::reject());
        };
        let identity = Ed25519PublicKey::from_bytes(*public_key.as_ref());
        let now = unix_time_millis();
        let accepted = self
            .authentication
            .lock()
            .await
            .authenticate(user, identity, now, self.disconnect.clone())
            .is_ok();
        if accepted {
            self.identity = Some(identity);
            self.authenticated.notify_one();
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let mut tasks = self.tasks.lock().await;
        while tasks.try_join_next().is_some() {}
        if self.identity.is_none() || self.channels.len() + tasks.len() >= 8 {
            // Dropping ChannelOpenHandle is an explicit default rejection.
            return Ok(());
        }
        let id = channel.id();
        reply.accept().await;
        self.channels.insert(id, channel);
        Ok(())
    }

    async fn channel_close(
        &mut self,
        channel: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.remove(&channel);
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let Some(stream) = self.channels.remove(&channel) else {
            session.channel_failure(channel)?;
            return Ok(());
        };
        if self.identity.is_none() {
            session.channel_failure(channel)?;
            return Ok(());
        }
        let service = self.service.clone();
        let disconnect = self.disconnect.clone();
        match name {
            SSH_SUBSYSTEM if self.rpc_session_id.is_none() => {
                let rpc = service.open_session(128);
                self.rpc_session_id = Some(rpc.id());
                session.channel_success(channel)?;
                self.tasks.lock().await.spawn(serve_jsonl_session(
                    stream.into_stream(),
                    service,
                    disconnect,
                    rpc,
                ));
            }
            BLOB_SUBSYSTEM if self.rpc_session_id.is_some() => {
                let session_id = self.rpc_session_id.unwrap();
                session.channel_success(channel)?;
                self.tasks.lock().await.spawn(async move {
                    tokio::select! {
                        _ = disconnect.cancelled() => Ok(()),
                        result = service.files().transfer(session_id, stream.into_stream()) => result,
                    }
                });
            }
            _ => {
                session.channel_failure(channel)?;
            }
        }
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_failure(channel)
    }
    async fn exec_request(
        &mut self,
        channel: ChannelId,
        _data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_failure(channel)
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
        session.channel_failure(channel)
    }
    async fn x11_request(
        &mut self,
        channel: ChannelId,
        _single: bool,
        _protocol: &str,
        _cookie: &str,
        _screen: u32,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_failure(channel)
    }
    async fn env_request(
        &mut self,
        channel: ChannelId,
        _name: &str,
        _value: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_failure(channel)
    }
    async fn window_change_request(
        &mut self,
        channel: ChannelId,
        _cols: u32,
        _rows: u32,
        _width: u32,
        _height: u32,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_failure(channel)
    }
    async fn signal(
        &mut self,
        channel: ChannelId,
        _signal: Sig,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_failure(channel)
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

pub(crate) fn unix_time_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or(0)
}

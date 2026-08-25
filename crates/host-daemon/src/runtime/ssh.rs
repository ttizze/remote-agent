use std::{collections::HashMap, sync::Arc};

use host_daemon::{CodexRpcService, DeviceAuthenticationState, HostIdentity};
use host_protocol::{Ed25519PublicKey, SSH_SUBSYSTEM};
use russh::{
    Channel, ChannelId, Sig,
    keys::{Algorithm, PublicKey},
    server::{Auth, ChannelOpenHandle, Handler, Msg, Server, Session},
};
use tokio::sync::Mutex;

use super::{jsonl_session::serve_jsonl_session, unix_time_millis};

pub(super) struct GatewayServer {
    host_identity: Arc<HostIdentity>,
    authentication: Arc<Mutex<DeviceAuthenticationState>>,
    service: CodexRpcService,
    session_tasks: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}

impl GatewayServer {
    pub(super) fn new(
        host_identity: Arc<HostIdentity>,
        authentication: Arc<Mutex<DeviceAuthenticationState>>,
        service: CodexRpcService,
        session_tasks: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    ) -> Self {
        Self {
            host_identity,
            authentication,
            service,
            session_tasks,
        }
    }
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

pub(super) struct GatewayHandler {
    host_identity: Arc<HostIdentity>,
    authentication: Arc<Mutex<DeviceAuthenticationState>>,
    service: CodexRpcService,
    session_tasks: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    identity: Option<Ed25519PublicKey>,
    channels: HashMap<ChannelId, Channel<Msg>>,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum HandlerError {
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
            if let Err(error) = serve_jsonl_session(channel_stream.into_stream(), service).await {
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

fn is_supported_subsystem(name: &str) -> bool {
    name == SSH_SUBSYSTEM
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

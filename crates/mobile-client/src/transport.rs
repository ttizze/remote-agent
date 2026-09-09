use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use host_protocol::{Ed25519PublicKey, SSH_SUBSYSTEM};
use relay_transport::RelayTask;
use russh::{
    ChannelMsg, ChannelStream,
    client::{self, Handler},
    keys::{Algorithm, PrivateKey, PrivateKeyWithHashAlg, PublicKeyOrCertificate},
};

use crate::client::{MobileClientConfig, MobileClientError};

pub(crate) struct Connection {
    pub(crate) ssh: client::Handle<PinnedHostKeyHandler>,
    pub(crate) relay: RelayTask,
}

pub(crate) struct AuthenticatedChannel {
    pub(crate) session: Connection,
    pub(crate) stream: ChannelStream<client::Msg>,
}

pub(crate) struct PinnedHostKeyHandler {
    expected: Ed25519PublicKey,
}

impl Handler for PinnedHostKeyHandler {
    type Error = russh::Error;
    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let public = key.public_key();
        Ok(public.algorithm() == Algorithm::Ed25519
            && public
                .key_data()
                .ed25519()
                .is_some_and(|key| key.as_ref() == self.expected.as_bytes()))
    }
}

pub(crate) fn decode_device_key(bytes: &[u8]) -> Result<PrivateKey, MobileClientError> {
    let key = russh::keys::pkcs8::decode_pkcs8(bytes, None)
        .map_err(|_| MobileClientError::InvalidDeviceKey)?;
    if key.algorithm() != Algorithm::Ed25519 {
        return Err(MobileClientError::InvalidDeviceKey);
    }
    Ok(key)
}

pub(crate) async fn establish(
    config: &MobileClientConfig,
    device_key: PrivateKey,
) -> Result<AuthenticatedChannel, MobileClientError> {
    let (tunnel, relay) = relay_transport::connect_client(&config.relay).await?;
    let ssh_config = client::Config {
        inactivity_timeout: None,
        keepalive_interval: Some(std::time::Duration::from_secs(20)),
        keepalive_max: 3,
        ..client::Config::default()
    };
    let mut ssh = client::connect_stream(
        Arc::new(ssh_config),
        tunnel,
        PinnedHostKeyHandler {
            expected: config.host_identity,
        },
    )
    .await?;
    let username = match config.pairing_ticket {
        Some(ticket) => format!(
            "pair-v4.{}.{}",
            ticket.to_base64url(),
            URL_SAFE_NO_PAD.encode(&config.device_name)
        ),
        None => SSH_SUBSYSTEM.to_owned(),
    };
    if !ssh
        .authenticate_publickey(
            username,
            PrivateKeyWithHashAlg::new(Arc::new(device_key), None),
        )
        .await?
        .success()
    {
        return Err(MobileClientError::AuthenticationRejected);
    }
    let stream = open_subsystem(&ssh, SSH_SUBSYSTEM).await?;
    Ok(AuthenticatedChannel {
        session: Connection { ssh, relay },
        stream,
    })
}

pub(crate) async fn open_subsystem(
    ssh: &client::Handle<PinnedHostKeyHandler>,
    name: &str,
) -> Result<ChannelStream<client::Msg>, MobileClientError> {
    let mut channel = ssh.channel_open_session().await?;
    channel.request_subsystem(true, name).await?;
    loop {
        match channel.wait().await {
            Some(ChannelMsg::Success) => break,
            Some(ChannelMsg::Failure | ChannelMsg::Close | ChannelMsg::Eof) | None => {
                return Err(MobileClientError::SubsystemRejected);
            }
            Some(_) => {}
        }
    }
    Ok(channel.into_stream())
}

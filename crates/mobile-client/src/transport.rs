use std::{sync::Arc, time::Duration};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use host_protocol::{Ed25519PublicKey, SSH_SUBSYSTEM};
use russh::{
    ChannelMsg, ChannelStream,
    client::{self, Handler},
    keys::{Algorithm, PrivateKey, PrivateKeyWithHashAlg, PublicKeyOrCertificate},
};

use crate::{MobileClientConfig, MobileClientError};

const SSH_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);
const SSH_KEEPALIVE_MAX: usize = 3;

/// The concrete russh message type is deliberately kept behind this module.
/// Callers only see the authenticated stream and never a transport DTO.
type SessionHandle = client::Handle<PinnedHostKeyHandler>;
pub(crate) type SshSession = SessionHandle;

pub(crate) struct AuthenticatedChannel {
    pub(crate) session: SessionHandle,
    pub(crate) stream: ChannelStream<client::Msg>,
}

#[derive(Clone, Debug)]
pub(crate) struct PinnedHostKeyHandler {
    expected: Ed25519PublicKey,
}

impl Handler for PinnedHostKeyHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        Ok(host_key_matches(self.expected, server_public_key))
    }
}

/// Establishes SSH, pins the server host key, authenticates the device public
/// key, and opens the single allowed subsystem. No shell or exec request is
/// made: all application traffic is carried as Codex JSONL.
pub(crate) async fn establish(
    config: &MobileClientConfig,
    device_key: PrivateKey,
) -> Result<AuthenticatedChannel, MobileClientError> {
    let ssh_config = client::Config {
        inactivity_timeout: None,
        keepalive_interval: Some(SSH_KEEPALIVE_INTERVAL),
        keepalive_max: SSH_KEEPALIVE_MAX,
        nodelay: true,
        ..client::Config::default()
    };
    let mut session = client::connect(
        Arc::new(ssh_config),
        config.address,
        PinnedHostKeyHandler {
            expected: config.host_identity,
        },
    )
    .await
    .map_err(MobileClientError::Ssh)?;

    let username = pairing_username(config)?;
    let auth = session
        .authenticate_publickey(
            username,
            PrivateKeyWithHashAlg::new(Arc::new(device_key), None),
        )
        .await
        .map_err(MobileClientError::Ssh)?;
    if !auth.success() {
        return Err(MobileClientError::AuthenticationRejected);
    }

    let mut channel = session
        .channel_open_session()
        .await
        .map_err(MobileClientError::Ssh)?;
    channel
        .request_subsystem(true, SSH_SUBSYSTEM)
        .await
        .map_err(MobileClientError::Ssh)?;
    tokio::time::timeout(config.request_timeout, async {
        loop {
            match channel.wait().await {
                Some(ChannelMsg::Success) => return Ok(()),
                Some(ChannelMsg::Failure | ChannelMsg::Close | ChannelMsg::Eof) | None => {
                    return Err(MobileClientError::SubsystemRejected);
                }
                Some(_) => {}
            }
        }
    })
    .await
    .map_err(|_| MobileClientError::SubsystemTimeout)??;
    let stream = channel.into_stream();

    Ok(AuthenticatedChannel { session, stream })
}

pub(crate) fn decode_device_key(device_pkcs8: &[u8]) -> Result<PrivateKey, MobileClientError> {
    let key = russh::keys::pkcs8::decode_pkcs8(device_pkcs8, None)
        .map_err(|_| MobileClientError::InvalidDeviceKey)?;
    if key.algorithm() != Algorithm::Ed25519 {
        return Err(MobileClientError::InvalidDeviceKey);
    }
    Ok(key)
}

/// Maps the pairing state to the SSH username understood by the Host.
///
/// The ticket is not sent as an application frame. It is carried in the
/// username while the device private key signature proves possession of the
/// paired key. Reconnects use the stable subsystem username.
pub(crate) fn pairing_username(config: &MobileClientConfig) -> Result<String, MobileClientError> {
    if let Some(ticket) = config.pairing_ticket {
        let device_name = URL_SAFE_NO_PAD.encode(config.device_name.as_bytes());
        Ok(format!("pair-v1.{}.{}", ticket.to_base64url(), device_name))
    } else {
        Ok(SSH_SUBSYSTEM.to_owned())
    }
}

pub(crate) fn host_key_matches(
    expected: Ed25519PublicKey,
    observed: &PublicKeyOrCertificate,
) -> bool {
    let key = observed.public_key();
    key.algorithm() == Algorithm::Ed25519
        && key
            .key_data()
            .ed25519()
            .is_some_and(|public| public.as_ref() == expected.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use host_protocol::PairingToken;
    use ring::{rand::SystemRandom, signature::Ed25519KeyPair};
    use russh::keys::ssh_key::{PublicKey, public::KeyData};

    fn config(ticket: Option<PairingToken>) -> MobileClientConfig {
        MobileClientConfig {
            address: "127.0.0.1:22".parse().unwrap(),
            server_name: "host.local".to_owned(),
            host_identity: Ed25519PublicKey::from_bytes([7; 32]),
            device_name: "test phone".to_owned(),
            pairing_ticket: ticket,
            max_frame_bytes: 4096,
            request_timeout: Duration::from_secs(1),
        }
    }

    #[test]
    fn first_pairing_username_contains_only_url_safe_components() {
        let username = pairing_username(&config(Some(PairingToken::from_bytes([9; 32])))).unwrap();
        assert_eq!(
            username,
            "pair-v1.CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk.dGVzdCBwaG9uZQ"
        );
        assert!(!username.contains('='));
        assert!(!username.contains('/'));
    }

    #[test]
    fn reconnect_username_is_stable() {
        assert_eq!(pairing_username(&config(None)).unwrap(), SSH_SUBSYSTEM);
    }

    #[test]
    fn host_key_pin_accepts_only_the_expected_ed25519_key() {
        let expected = Ed25519PublicKey::from_bytes([7; 32]);
        let observed = PublicKey::from(KeyData::Ed25519(
            russh::keys::ssh_key::public::Ed25519PublicKey([7; 32]),
        ));
        assert!(host_key_matches(expected, &observed.into()));

        let wrong = PublicKey::from(KeyData::Ed25519(
            russh::keys::ssh_key::public::Ed25519PublicKey([8; 32]),
        ));
        assert!(!host_key_matches(expected, &wrong.into()));
    }

    #[test]
    fn ring_generated_pkcs8_device_keys_are_accepted_by_russh() {
        let key = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        let parsed = decode_device_key(key.as_ref()).unwrap();
        assert_eq!(parsed.algorithm(), Algorithm::Ed25519);
    }
}

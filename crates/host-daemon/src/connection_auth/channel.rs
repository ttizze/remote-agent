use std::sync::Arc;

use host_protocol::{
    AuthenticationProof, DeviceAuthenticationReply, DeviceAuthenticationStart, Ed25519PublicKey,
    Ed25519Signature, PairingRequest, authentication_proof_message,
};
use tokio::sync::Mutex;

use super::{ConnectionAuthenticationError, DeviceAuthenticationState};
use crate::{HostIdentity, PendingRpcChannel, RpcChannel};

pub async fn authenticate_incoming_channel(
    mut channel: PendingRpcChannel,
    host_identity: &HostIdentity,
    state: Arc<Mutex<DeviceAuthenticationState>>,
    mut now_ms: impl FnMut() -> u64 + Send,
) -> Result<(RpcChannel, Ed25519PublicKey), ConnectionAuthenticationError> {
    if channel.host_identity != host_identity.public_key() {
        return Err(ConnectionAuthenticationError::HostIdentityChanged);
    }

    match channel.receive().await? {
        DeviceAuthenticationStart::Pair { request } => {
            let accepted = {
                let mut state = state.lock().await;
                state.accept_pairing(request, now_ms())?
            };
            channel
                .send(&DeviceAuthenticationReply::Accepted {
                    device_identity: accepted.identity,
                })
                .await?;
            Ok((channel.authenticate(), accepted.identity))
        }
        DeviceAuthenticationStart::Authenticate { device_identity } => {
            let challenge = {
                let mut state = state.lock().await;
                state.issue_authentication_challenge(
                    host_identity.public_key(),
                    device_identity,
                    now_ms(),
                )?
            };
            channel
                .send(&DeviceAuthenticationReply::Challenge {
                    challenge: challenge.clone(),
                })
                .await?;
            let proof: AuthenticationProof = channel.receive().await?;
            let authenticated_identity = {
                let mut state = state.lock().await;
                state.verify_authentication(proof, now_ms())?
            };
            channel
                .send(&DeviceAuthenticationReply::Accepted {
                    device_identity: authenticated_identity,
                })
                .await?;
            Ok((channel.authenticate(), authenticated_identity))
        }
    }
}

pub async fn pair_outgoing_channel(
    mut channel: PendingRpcChannel,
    request: PairingRequest,
) -> Result<RpcChannel, ConnectionAuthenticationError> {
    let expected_identity = request.device_identity;
    channel
        .send(&DeviceAuthenticationStart::Pair { request })
        .await?;
    match channel.receive().await? {
        DeviceAuthenticationReply::Accepted { device_identity }
            if device_identity == expected_identity =>
        {
            Ok(channel.authenticate())
        }
        _ => Err(ConnectionAuthenticationError::UnexpectedReply),
    }
}

pub async fn authenticate_outgoing_channel(
    mut channel: PendingRpcChannel,
    device_identity: Ed25519PublicKey,
    sign: impl FnOnce(&[u8]) -> Ed25519Signature,
) -> Result<RpcChannel, ConnectionAuthenticationError> {
    channel
        .send(&DeviceAuthenticationStart::Authenticate { device_identity })
        .await?;
    let challenge = match channel.receive().await? {
        DeviceAuthenticationReply::Challenge { challenge }
            if challenge.host_identity == channel.host_identity =>
        {
            challenge
        }
        _ => return Err(ConnectionAuthenticationError::UnexpectedReply),
    };
    let message =
        authentication_proof_message(challenge.host_identity, challenge.token, device_identity);
    channel
        .send(&AuthenticationProof {
            token: challenge.token,
            signature: sign(&message),
        })
        .await?;
    match channel.receive().await? {
        DeviceAuthenticationReply::Accepted {
            device_identity: authenticated,
        } if authenticated == device_identity => Ok(channel.authenticate()),
        _ => Err(ConnectionAuthenticationError::UnexpectedReply),
    }
}

mod channel;
mod state;

pub use channel::{
    authenticate_incoming_channel, authenticate_outgoing_channel, pair_outgoing_channel,
};
pub use state::DeviceAuthenticationState;

use crate::{AuthenticationError, PairingError, SettingsError, TransportError};

#[derive(Debug, thiserror::Error)]
pub enum ConnectionAuthenticationError {
    #[error("connection transport failed during device authentication: {0}")]
    Transport(#[from] TransportError),
    #[error("pairing failed: {0}")]
    Pairing(#[from] PairingError),
    #[error("device authentication failed: {0}")]
    Authentication(#[from] AuthenticationError),
    #[error("failed to persist paired device: {0}")]
    Settings(#[from] SettingsError),
    #[error("device is not paired")]
    DeviceNotPaired,
    #[error("host identity changed during handshake")]
    HostIdentityChanged,
    #[error("authentication challenge expiry overflowed")]
    InvalidChallengeExpiry,
    #[error("peer sent an unexpected authentication reply")]
    UnexpectedReply,
}

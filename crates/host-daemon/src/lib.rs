mod codex_rpc;
mod connection_auth;
mod host_identity;
mod mdns;
mod pairing;
mod rpc_server;
mod settings;
mod transport;

pub use codex_rpc::{CodexRpcService, CodexSession, DispatchError, ResponseDisposition, SessionId};
pub use connection_auth::{
    ConnectionAuthenticationError, DeviceAuthenticationState, authenticate_incoming_channel,
    authenticate_outgoing_channel, pair_outgoing_channel,
};
#[cfg(target_os = "macos")]
pub use host_identity::MacOsKeychainHostIdentityStore;
pub use host_identity::{
    HostIdentity, HostIdentityError, HostIdentityKeyStore, KeyStoreError,
    load_or_create_host_identity,
};
pub use mdns::{MdnsAdvertisement, MdnsError, ServiceMetadata, service_metadata};
pub use pairing::{
    AcceptedDevice, AuthenticationChallenges, AuthenticationError, PairingError, PairingTickets,
};
pub use rpc_server::serve_gateway_messages;
pub use settings::{HostSettings, PairedDevice, SettingsError, load_settings, save_settings};
pub use transport::{
    PendingRpcChannel, RpcChannel, RpcServerConfig, TransportError, accept_rpc_channel,
    connect_rpc_channel,
};

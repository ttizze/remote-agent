mod codex_rpc;
mod connection_auth;
mod desktop_projects;
mod host_identity;
mod mdns;
mod pairing;
mod settings;

pub use codex_rpc::{CodexRpcService, CodexSession, DispatchError, ResponseDisposition, SessionId};
pub use connection_auth::{
    ConnectionAuthenticationError, DeviceAuthenticationState, PAIRING_USERNAME_PREFIX,
    RECONNECT_USERNAME,
};
pub use desktop_projects::{
    DesktopProjectError, DesktopProjectStore, HOST_PROJECT_LIST_METHOD, HOST_PROJECT_METHODS,
    HOST_THREAD_LIST_METHOD, HOST_THREAD_READ_METHOD, HOST_THREAD_START_METHOD,
};
#[cfg(target_os = "macos")]
pub use host_identity::MacOsKeychainHostIdentityStore;
pub use host_identity::{
    HostIdentity, HostIdentityError, HostIdentityKeyStore, KeyStoreError,
    load_or_create_host_identity,
};
pub use mdns::{MdnsAdvertisement, MdnsError, ServiceMetadata, service_metadata};
pub use pairing::{AcceptedDevice, PairingError, PairingTickets};
pub use settings::{HostSettings, PairedDevice, SettingsError, load_settings, save_settings};

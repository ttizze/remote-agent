//! Only OS storage. Core owns restoration, persistence scheduling and recovery.
use agent_core::client::{
    AgentError,
    persistence::{ClientStorage, PreferenceKey, PreferenceStorage},
    session::ClientPlatform,
};
use serde::{Deserialize, Serialize};
use std::{io, path::PathBuf, sync::Arc};

pub(crate) fn error(reason: impl std::fmt::Display) -> AgentError {
    AgentError::Failed {
        reason: reason.to_string(),
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Host {
    pub id: String,
    pub name: String,
    pub ticket: String,
    pub local_directory: Option<PathBuf>,
}

pub(crate) struct Storage {
    pub root: PathBuf,
    pub host: Host,
    pub handed: Arc<HandedKeys>,
}

/// The device keys a local process handed this client for its Host (a WSL
/// distro's bootstrap on Windows), by Host id. They live as long as the client.
#[derive(Default)]
pub(crate) struct HandedKeys(std::sync::Mutex<std::collections::BTreeMap<String, [u8; 32]>>);

impl HandedKeys {
    pub fn hand(&self, host: &str, key: [u8; 32]) {
        self.keys().insert(host.into(), key);
    }

    pub fn has(&self, host: &str) -> bool {
        self.keys().contains_key(host)
    }

    fn key(&self, host: &str) -> Option<[u8; 32]> {
        self.keys().get(host).copied()
    }

    fn keys(&self) -> std::sync::MutexGuard<'_, std::collections::BTreeMap<String, [u8; 32]>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Storage {
    fn host_directory(&self) -> PathBuf {
        self.root.join("hosts").join(
            uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, self.host.id.as_bytes()).to_string(),
        )
    }
    fn state(&self) -> PathBuf {
        self.host_directory().join("state.json")
    }

    /// Forgets a removed Host on this device: its state, conversations and
    /// diagnostics, and the device key it was paired with. Its session is closed.
    pub fn forget(&self) -> Result<(), AgentError> {
        match std::fs::remove_dir_all(self.host_directory()) {
            Ok(()) => {}
            Err(reason) if reason.kind() == io::ErrorKind::NotFound => {}
            Err(reason) => return Err(error(reason)),
        }
        forget_identity(&self.host.id)
    }
}

pub(crate) fn read(path: &std::path::Path) -> Result<Vec<u8>, AgentError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(reason) if reason.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(reason) => Err(error(reason)),
    }
}

impl ClientStorage for Storage {
    fn load(&self) -> Result<Vec<u8>, AgentError> {
        read(&self.state())
    }
    fn save(&self, bytes: Vec<u8>) -> Result<(), AgentError> {
        agent_local_host::save_private_bytes(&self.state(), &bytes).map_err(error)
    }
}

/// The device's own preferences, shared by its Hosts: models, the editor and
/// the UI language, each in a file of the app's data directory.
pub(crate) struct DeviceStorage {
    pub root: PathBuf,
}

impl DeviceStorage {
    fn path(&self, key: PreferenceKey) -> PathBuf {
        self.root.join(match key {
            PreferenceKey::Models => "model-preferences.json",
            PreferenceKey::Editor => "editor-preferences.json",
            PreferenceKey::Composer => "composer-preferences.json",
            PreferenceKey::Language => "language.json",
            PreferenceKey::Contrast => "contrast.json",
            PreferenceKey::WebLinks => "web-links.json",
        })
    }
}

impl PreferenceStorage for DeviceStorage {
    fn load(&self, key: PreferenceKey) -> Result<Vec<u8>, AgentError> {
        read(&self.path(key))
    }
    fn save(&self, key: PreferenceKey, bytes: Vec<u8>) -> Result<(), AgentError> {
        agent_local_host::save_private_bytes(&self.path(key), &bytes).map_err(error)
    }
}

impl ClientPlatform for Storage {
    fn identity(&self) -> Result<Vec<u8>, AgentError> {
        if let Some(key) = self.handed.key(&self.host.id) {
            return Ok(key.to_vec());
        }
        if let Some(directory) = &self.host.local_directory {
            return agent_local_host::load_local_identity(directory)
                .map(|identity| identity.to_bytes().to_vec())
                .map_err(error);
        }
        identity(&self.host.id)
    }
    fn diagnostics_directory(&self) -> Result<Option<String>, AgentError> {
        Ok(Some(
            self.host_directory()
                .join("diagnostics")
                .to_string_lossy()
                .into_owned(),
        ))
    }
    fn conversation_directory(&self) -> Result<Option<String>, AgentError> {
        Ok(Some(
            self.host_directory()
                .join("conversations")
                .to_string_lossy()
                .into_owned(),
        ))
    }
}

/// The Keychain service of paired Hosts' device keys.
#[cfg(any(target_os = "macos", target_os = "ios"))]
const SERVICE: &str = "com.bex.nsh-preview.identity";

/// The Keychain's status for an item it does not have.
#[cfg(any(target_os = "macos", target_os = "ios"))]
const ITEM_NOT_FOUND: i32 = -25300;

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn identity(host: &str) -> Result<Vec<u8>, AgentError> {
    use security_framework::passwords::{PasswordOptions, generic_password, set_generic_password};
    match generic_password(PasswordOptions::new_generic_password(SERVICE, host)) {
        Ok(bytes) => Ok(bytes),
        Err(reason) if reason.code() == ITEM_NOT_FOUND => {
            let bytes = agent_core::client::native::generate_identity();
            set_generic_password(SERVICE, host, &bytes).map_err(error)?;
            Ok(bytes)
        }
        Err(reason) => Err(error(reason)),
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn forget_identity(host: &str) -> Result<(), AgentError> {
    match security_framework::passwords::delete_generic_password(SERVICE, host) {
        Err(reason) if reason.code() != ITEM_NOT_FOUND => Err(error(reason)),
        _ => Ok(()),
    }
}

/// A paired Host's device key, in the user's Windows Credential Manager.
#[cfg(windows)]
fn identity(host: &str) -> Result<Vec<u8>, AgentError> {
    use windows::{
        Win32::{
            Foundation::ERROR_NOT_FOUND,
            Security::Credentials::{
                CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW,
                CredWriteW,
            },
        },
        core::PWSTR,
    };
    let target = credential_target(host);
    let mut stored: *mut CREDENTIALW = std::ptr::null_mut();
    // SAFETY: CredReadW fills `stored` with a buffer CredFree releases after
    // its blob is copied.
    match unsafe { CredReadW(&target, CRED_TYPE_GENERIC, None, &mut stored) } {
        Ok(()) => unsafe {
            let credential = &*stored;
            let bytes = std::slice::from_raw_parts(
                credential.CredentialBlob,
                credential.CredentialBlobSize as usize,
            )
            .to_vec();
            CredFree(stored.cast());
            Ok(bytes)
        },
        Err(reason) if reason.code() == ERROR_NOT_FOUND.to_hresult() => {
            let mut bytes = agent_core::client::native::generate_identity();
            let credential = CREDENTIALW {
                Type: CRED_TYPE_GENERIC,
                TargetName: PWSTR(target.as_ptr().cast_mut()),
                CredentialBlobSize: u32::try_from(bytes.len()).map_err(error)?,
                CredentialBlob: bytes.as_mut_ptr(),
                Persist: CRED_PERSIST_LOCAL_MACHINE,
                ..Default::default()
            };
            // SAFETY: the target and blob outlive the call, which copies them.
            unsafe { CredWriteW(&credential, 0) }.map_err(error)?;
            Ok(bytes)
        }
        Err(reason) => Err(error(reason)),
    }
}

#[cfg(windows)]
fn credential_target(host: &str) -> windows::core::HSTRING {
    windows::core::HSTRING::from(format!("com.bex.nsh.identity/{host}"))
}

#[cfg(windows)]
fn forget_identity(host: &str) -> Result<(), AgentError> {
    use windows::Win32::{
        Foundation::ERROR_NOT_FOUND,
        Security::Credentials::{CRED_TYPE_GENERIC, CredDeleteW},
    };
    // SAFETY: the target outlives the call.
    match unsafe { CredDeleteW(&credential_target(host), CRED_TYPE_GENERIC, None) } {
        Err(reason) if reason.code() != ERROR_NOT_FOUND.to_hresult() => Err(error(reason)),
        _ => Ok(()),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "ios", windows)))]
fn forget_identity(_: &str) -> Result<(), AgentError> {
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "ios", windows)))]
fn identity(_: &str) -> Result<Vec<u8>, AgentError> {
    Err(error(
        "This client requires Apple Keychain or Windows Credential Manager.",
    ))
}

/// The iPhone app's data. The desktop keeps its data in its local Host's scope.
pub(crate) fn directory() -> Result<PathBuf, AgentError> {
    if let Some(directory) = std::env::var_os("AGENT_STATE_DIR") {
        return Ok(PathBuf::from(directory).join("nsh-client"));
    }
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        use objc2_foundation::{
            NSSearchPathDirectory, NSSearchPathDomainMask, NSSearchPathForDirectoriesInDomains,
        };
        let paths = NSSearchPathForDirectoriesInDomains(
            NSSearchPathDirectory::ApplicationSupportDirectory,
            NSSearchPathDomainMask::UserDomainMask,
            true,
        );
        let path = paths
            .firstObject()
            .ok_or_else(|| error("Application Support directory is unavailable"))?;
        Ok(PathBuf::from(path.to_string()).join("com.bex.nsh-preview"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    {
        Err(error(
            "This client requires an Apple application data directory.",
        ))
    }
}

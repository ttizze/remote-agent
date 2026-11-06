//! Files the Host and its local clients share on one machine: private state
//! files, the local client identity, discovery of the running Host and the
//! application bundle layout.
#[cfg(unix)]
pub mod browser;
mod files;
mod registry;
pub mod update;
pub mod wsl;

/// Returned only over the local process pipe to a desktop on the owning machine.
/// Never log this value: it contains the preauthorized local client's secret.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct LocalClientAccess {
    pub ticket: agent_transport::transport::Ticket,
    pub secret: [u8; 32],
}

impl LocalClientAccess {
    /// A user's login shell may print a banner before the bootstrap reply.
    pub fn from_process_output(bytes: Vec<u8>) -> anyhow::Result<Self> {
        let bytes = zeroize::Zeroizing::new(bytes);
        let reply = bytes
            .rsplit(|byte| *byte == b'\n')
            .find(|line| !line.trim_ascii().is_empty())
            .ok_or_else(|| anyhow::anyhow!("WSL did not return local Host credentials"))?;
        Ok(serde_json::from_slice(reply)?)
    }
}

impl Drop for LocalClientAccess {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.secret);
    }
}

pub use files::{
    create_state_directory, private_file_options, save_private_bytes, save_private_json,
    state_directory,
};
pub use registry::{HostLease, LocalHost, LocalHostRegistry, LocalHostState};

use agent_transport::transport::Identity;
use anyhow::{Result, ensure};
use std::path::{Path, PathBuf};

/// The operating system's name for this machine, shared by Host and local clients.
pub fn machine_name() -> String {
    #[cfg(windows)]
    let name = std::env::var("COMPUTERNAME").ok();
    #[cfg(not(windows))]
    let name = std::process::Command::new("/bin/hostname")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok());
    name.map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "PC".into())
}

/// The Host provisions both identity keys here; local clients read only theirs.
pub fn identity_path(directory: &Path) -> PathBuf {
    directory.join("identity.keys")
}

/// Read only the local identity already provisioned by the Host.
/// File access must run on a blocking worker.
pub fn load_local_identity(directory: &Path) -> Result<Identity> {
    let bytes = match std::fs::read(identity_path(directory)) {
        Ok(bytes) => zeroize::Zeroizing::new(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            anyhow::bail!("local Host credentials are not provisioned")
        }
        Err(error) => return Err(error.into()),
    };
    ensure!(
        bytes.len() == 64,
        "saved Host keys must contain exactly 64 bytes"
    );
    Ok(Identity::from_bytes(bytes[32..].try_into().unwrap()))
}

/// The Host executable supplied beside a local client executable.
pub fn bundled_host_path(executable: impl AsRef<Path>) -> PathBuf {
    executable
        .as_ref()
        .with_file_name(format!("host-daemon{}", std::env::consts::EXE_SUFFIX))
}

/// The Mac runtime has its own background-only identity instead of inheriting
/// the desktop app's foreground identity when npm or an MCP tool uses it.
pub fn bundled_node_path(directory: &Path) -> Option<PathBuf> {
    directory.ends_with("Contents/MacOS").then(|| {
        directory
            .parent()
            .expect("Contents exists")
            .join("Helpers/Bex Node.app/Contents/MacOS/node")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_transport::transport::{Endpoint, Relays};

    #[tokio::test]
    async fn bootstrap_reply_ignores_login_banners_but_requires_complete_credentials() {
        let endpoint = Endpoint::bind(Identity::generate(), Relays::Loopback)
            .await
            .unwrap();
        let access = LocalClientAccess {
            ticket: endpoint.ticket(),
            secret: [7; 32],
        };
        let mut output = b"Welcome to Linux\n\n".to_vec();
        output.extend(serde_json::to_vec(&access).unwrap());
        output.extend(b"\n\n");
        let decoded = LocalClientAccess::from_process_output(output).unwrap();
        assert!(decoded.ticket == access.ticket);
        assert_eq!(decoded.secret, [7; 32]);
        for output in [b"".as_slice(), b"Welcome\n", b"{}\n", b"{\"secret\":[7]}\n"] {
            assert!(LocalClientAccess::from_process_output(output.to_vec()).is_err());
        }
        endpoint.close().await;
    }
}

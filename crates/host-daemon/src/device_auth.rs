use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use host_protocol::{
    CURRENT_PROTOCOL_VERSION, Ed25519PublicKey, PairingQrPayload, PairingToken, RelayEndpoint,
    SSH_SUBSYSTEM,
};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

const MAX_DEVICES: usize = 64;
const MAX_INVITATIONS: usize = 16;
const INVITATION_LIFETIME_MS: u64 = 10 * 60_000;
const SETTINGS_VERSION: u32 = 4;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairedDevice {
    pub identity: Ed25519PublicKey,
    pub name: String,
}

#[derive(Serialize, Deserialize)]
struct Settings {
    version: u32,
    devices: Vec<PairedDevice>,
}

/// Device authorization and invitations stay on the trusted PC. The relay
/// cannot mint invitations or add public keys to this state.
pub struct DeviceAuthenticationState {
    path: PathBuf,
    devices: Vec<PairedDevice>,
    invitations: HashMap<PairingToken, u64>,
    connections: HashMap<Ed25519PublicKey, Vec<CancellationToken>>,
}

impl DeviceAuthenticationState {
    pub fn load(path: PathBuf) -> Result<Self, String> {
        let devices = match fs::read(&path) {
            Ok(bytes) => {
                let settings: Settings =
                    serde_json::from_slice(&bytes).map_err(|_| "device settings are invalid")?;
                if settings.version != SETTINGS_VERSION {
                    return Err("device settings version is unsupported".into());
                }
                if settings.devices.len() > MAX_DEVICES {
                    return Err("too many paired devices".into());
                }
                settings.devices
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(format!("cannot read device settings: {error}")),
        };
        Ok(Self {
            path,
            devices,
            invitations: HashMap::new(),
            connections: HashMap::new(),
        })
    }

    pub fn devices(&self) -> &[PairedDevice] {
        &self.devices
    }

    pub fn invite(
        &mut self,
        host: Ed25519PublicKey,
        host_name: String,
        relay: RelayEndpoint,
        now_ms: u64,
    ) -> Result<PairingQrPayload, String> {
        relay.validate().map_err(|error| error.to_string())?;
        self.invitations.retain(|_, expiry| *expiry > now_ms);
        if self.invitations.len() >= MAX_INVITATIONS {
            return Err("too many unexpired invitations".into());
        }
        if self.devices.len() >= MAX_DEVICES {
            return Err("remove a device before inviting another".into());
        }
        let mut random = [0; 32];
        SystemRandom::new()
            .fill(&mut random)
            .map_err(|_| "secure random generation failed")?;
        let ticket = PairingToken::from_bytes(random);
        let expires_at_ms = now_ms
            .checked_add(INVITATION_LIFETIME_MS)
            .ok_or("invitation expiry overflow")?;
        self.invitations.insert(ticket, expires_at_ms);
        Ok(PairingQrPayload {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            host_identity: host,
            host_name,
            relay,
            ticket,
            expires_at_ms,
        })
    }

    /// Call only after russh verifies the device's proof of possession. The
    /// invitation is consumed only after atomic persistence succeeds.
    pub fn authenticate(
        &mut self,
        username: &str,
        identity: Ed25519PublicKey,
        now_ms: u64,
        connection: CancellationToken,
    ) -> Result<(), String> {
        if username == SSH_SUBSYSTEM {
            if !self
                .devices
                .iter()
                .any(|device| device.identity == identity)
            {
                return Err("device is not paired".into());
            }
        } else {
            let (ticket, name) = pairing_username(username)?;
            match self.invitations.get(&ticket) {
                Some(expiry) if now_ms < *expiry => {}
                _ => return Err("invitation is expired, unknown, or already used".into()),
            }
            let mut devices = self.devices.clone();
            if let Some(device) = devices
                .iter_mut()
                .find(|device| device.identity == identity)
            {
                device.name = name;
            } else {
                if devices.len() >= MAX_DEVICES {
                    return Err("device limit reached".into());
                }
                devices.push(PairedDevice { identity, name });
            }
            save_devices(&self.path, &devices)?;
            self.devices = devices;
            self.invitations.remove(&ticket);
        }
        let connections = self.connections.entry(identity).or_default();
        connections.retain(|connection| !connection.is_cancelled());
        connections.push(connection);
        Ok(())
    }

    pub fn revoke(&mut self, identity: Ed25519PublicKey) -> Result<(), String> {
        let devices: Vec<_> = self
            .devices
            .iter()
            .filter(|device| device.identity != identity)
            .cloned()
            .collect();
        save_devices(&self.path, &devices)?;
        self.devices = devices;
        if let Some(connections) = self.connections.remove(&identity) {
            for connection in connections {
                connection.cancel();
            }
        }
        Ok(())
    }
}

fn pairing_username(username: &str) -> Result<(PairingToken, String), String> {
    if username.len() > 512 {
        return Err("invalid pairing identity".into());
    }
    let rest = username
        .strip_prefix("pair-v4.")
        .ok_or("invalid pairing identity")?;
    let (ticket, name) = rest.split_once('.').ok_or("invalid pairing identity")?;
    let ticket = PairingToken::from_base64url(ticket).map_err(|_| "invalid pairing invitation")?;
    let name = URL_SAFE_NO_PAD
        .decode(name)
        .map_err(|_| "invalid device name")?;
    let name = String::from_utf8(name).map_err(|_| "invalid device name")?;
    if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
        return Err("invalid device name".into());
    }
    Ok((ticket, name))
}

fn save_devices(path: &Path, devices: &[PairedDevice]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or("device settings have no parent directory")?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    // NamedTempFile creates owner-only files. Persisting the replacement keeps
    // that mode and never exposes a partially written device authorization list.
    #[derive(Serialize)]
    struct BorrowedSettings<'a> {
        version: u32,
        devices: &'a [PairedDevice],
    }
    serde_json::to_writer(
        &mut file,
        &BorrowedSettings {
            version: SETTINGS_VERSION,
            devices,
        },
    )
    .map_err(|error| error.to_string())?;
    file.write_all(b"\n").map_err(|error| error.to_string())?;
    file.as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    file.persist(path)
        .map_err(|error| error.error.to_string())?;
    fs::File::open(parent)
        .and_then(|dir| dir.sync_all())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn endpoint() -> RelayEndpoint {
        RelayEndpoint {
            relay_url: "ws://localhost/socket/websocket".into(),
            relay_token: "fixture".into(),
            runner_id: "fixture".into(),
        }
    }
    fn username(ticket: PairingToken) -> String {
        format!(
            "pair-v4.{}.{}",
            ticket.to_base64url(),
            URL_SAFE_NO_PAD.encode("iPhone")
        )
    }

    #[test]
    fn one_time_invitation_persists_authorization_and_revocation_closes_live_connections() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("devices.json");
        let mut auth = DeviceAuthenticationState::load(path.clone()).unwrap();
        let host = Ed25519PublicKey::from_bytes([1; 32]);
        let device = Ed25519PublicKey::from_bytes([2; 32]);
        let ticket = auth
            .invite(host, "Mac".into(), endpoint(), 0)
            .unwrap()
            .ticket;
        let first = CancellationToken::new();
        auth.authenticate(&username(ticket), device, 1, first.clone())
            .unwrap();
        assert!(
            auth.authenticate(&username(ticket), device, 2, CancellationToken::new())
                .is_err()
        );
        let second = CancellationToken::new();
        auth.authenticate(SSH_SUBSYSTEM, device, 3, second.clone())
            .unwrap();
        let mut loaded = DeviceAuthenticationState::load(path.clone()).unwrap();
        loaded
            .authenticate(SSH_SUBSYSTEM, device, 4, CancellationToken::new())
            .unwrap();
        auth.revoke(device).unwrap();
        assert!(first.is_cancelled() && second.is_cancelled());
        assert!(
            auth.authenticate(SSH_SUBSYSTEM, device, 5, CancellationToken::new())
                .is_err()
        );
        assert!(
            DeviceAuthenticationState::load(path)
                .unwrap()
                .devices()
                .is_empty()
        );
    }

    #[test]
    fn expiry_and_failed_persistence_never_authorize_a_device() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("devices.json");
        let mut auth = DeviceAuthenticationState::load(path.clone()).unwrap();
        let host = Ed25519PublicKey::from_bytes([1; 32]);
        let device = Ed25519PublicKey::from_bytes([2; 32]);
        let invitation = auth.invite(host, "Mac".into(), endpoint(), 0).unwrap();
        assert!(
            auth.authenticate(
                &username(invitation.ticket),
                device,
                invitation.expires_at_ms,
                CancellationToken::new()
            )
            .is_err()
        );
        fs::create_dir(&path).unwrap();
        assert!(
            auth.authenticate(
                &username(invitation.ticket),
                device,
                1,
                CancellationToken::new()
            )
            .is_err()
        );
        fs::remove_dir(&path).unwrap();
        auth.authenticate(
            &username(invitation.ticket),
            device,
            2,
            CancellationToken::new(),
        )
        .unwrap();
    }
}

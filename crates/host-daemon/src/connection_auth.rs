use std::path::PathBuf;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use host_protocol::{Ed25519PublicKey, PairingQrPayload, PairingToken};

use crate::{
    AcceptedDevice, HostSettings, PairedDevice, PairingError, PairingTickets, SettingsError,
    save_settings,
};

pub const PAIRING_USERNAME_PREFIX: &str = "pair-v1.";
pub const RECONNECT_USERNAME: &str = "remote-agent-v3";

/// Mutable authentication state shared by all SSH handlers.
pub struct DeviceAuthenticationState {
    pairing_tickets: PairingTickets,
    settings: HostSettings,
    settings_path: PathBuf,
}

impl DeviceAuthenticationState {
    pub fn new(settings: HostSettings, settings_path: PathBuf) -> Self {
        Self {
            pairing_tickets: PairingTickets::new(),
            settings,
            settings_path,
        }
    }

    pub fn settings(&self) -> &HostSettings {
        &self.settings
    }

    pub fn issue_pairing_ticket(
        &mut self,
        host_identity: Ed25519PublicKey,
        addresses: Vec<String>,
        expires_at_ms: u64,
    ) -> Result<PairingQrPayload, PairingError> {
        self.pairing_tickets
            .issue(host_identity, addresses, expires_at_ms)
    }

    pub fn issue_pairing_ticket_with_token(
        &mut self,
        host_identity: Ed25519PublicKey,
        addresses: Vec<String>,
        expires_at_ms: u64,
        ticket: PairingToken,
    ) -> Result<PairingQrPayload, PairingError> {
        self.pairing_tickets
            .issue_with_token(host_identity, addresses, expires_at_ms, ticket)
    }

    /// Authenticate a public key after russh has verified its SSH signature.
    ///
    /// Pairing validates the ticket and persists the device before consuming
    /// it, so a settings write failure never burns the QR ticket.
    pub fn authenticate(
        &mut self,
        username: &str,
        device_identity: Ed25519PublicKey,
        host_identity: Ed25519PublicKey,
        now_ms: u64,
    ) -> Result<AcceptedDevice, ConnectionAuthenticationError> {
        if username == RECONNECT_USERNAME {
            if self
                .settings
                .paired_devices
                .iter()
                .any(|device| device.identity == device_identity)
            {
                let name = self
                    .settings
                    .paired_devices
                    .iter()
                    .find(|device| device.identity == device_identity)
                    .map(|device| device.name.clone())
                    .unwrap_or_else(|| "device".to_owned());
                return Ok(AcceptedDevice {
                    identity: device_identity,
                    name,
                });
            }
            return Err(ConnectionAuthenticationError::DeviceNotPaired);
        }

        let (ticket, name) = parse_pairing_username(username)?;
        self.pairing_tickets
            .validate(ticket, host_identity, now_ms)?;
        let mut candidate = self.settings.clone();
        candidate.upsert_paired_device(PairedDevice {
            identity: device_identity,
            name: name.clone(),
        });
        save_settings(&self.settings_path, &candidate)?;
        self.settings = candidate;
        self.pairing_tickets.consume(ticket)?;
        Ok(AcceptedDevice {
            identity: device_identity,
            name,
        })
    }
}

fn parse_pairing_username(
    username: &str,
) -> Result<(PairingToken, String), ConnectionAuthenticationError> {
    if username.len() > 512 {
        return Err(ConnectionAuthenticationError::InvalidUsername);
    }
    let Some(rest) = username.strip_prefix(PAIRING_USERNAME_PREFIX) else {
        return Err(ConnectionAuthenticationError::InvalidUsername);
    };
    let mut fields = rest.split('.');
    let ticket = fields
        .next()
        .ok_or(ConnectionAuthenticationError::InvalidUsername)
        .and_then(|value| {
            PairingToken::from_base64url(value)
                .map_err(|_| ConnectionAuthenticationError::InvalidUsername)
        })?;
    let encoded_name = fields
        .next()
        .ok_or(ConnectionAuthenticationError::InvalidUsername)?;
    if fields.next().is_some() {
        return Err(ConnectionAuthenticationError::InvalidUsername);
    }
    let name = URL_SAFE_NO_PAD
        .decode(encoded_name)
        .map_err(|_| ConnectionAuthenticationError::InvalidDeviceName)
        .and_then(|bytes| {
            String::from_utf8(bytes).map_err(|_| ConnectionAuthenticationError::InvalidDeviceName)
        })?;
    if name.is_empty() || name.len() > 128 || name.chars().any(|character| character.is_control()) {
        return Err(ConnectionAuthenticationError::InvalidDeviceName);
    }
    Ok((ticket, name))
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectionAuthenticationError {
    #[error("pairing failed: {0}")]
    Pairing(#[from] PairingError),
    #[error("failed to persist paired device: {0}")]
    Settings(#[from] SettingsError),
    #[error("device is not paired")]
    DeviceNotPaired,
    #[error("SSH username is invalid")]
    InvalidUsername,
    #[error("pairing device name is invalid")]
    InvalidDeviceName,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "remote-agent-auth-{}-{}.json",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn pairing_username_round_trip_and_reconnect() {
        let path = temp_path();
        let host = Ed25519PublicKey::from_bytes([1; 32]);
        let device = Ed25519PublicKey::from_bytes([2; 32]);
        let ticket = PairingToken::from_bytes([3; 32]);
        let name = URL_SAFE_NO_PAD.encode("my phone");
        let username = format!("{PAIRING_USERNAME_PREFIX}{}.{name}", ticket.to_base64url());
        let mut state = DeviceAuthenticationState::new(HostSettings::default(), path.clone());
        state
            .issue_pairing_ticket_with_token(host, vec![], 100, ticket)
            .unwrap();
        assert_eq!(
            state
                .authenticate(&username, device, host, 99)
                .unwrap()
                .name,
            "my phone"
        );
        assert_eq!(
            state
                .authenticate(RECONNECT_USERNAME, device, host, 99)
                .unwrap()
                .identity,
            device
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn settings_failure_does_not_consume_ticket() {
        let path = PathBuf::from("/dev/null/impossible/settings.json");
        let host = Ed25519PublicKey::from_bytes([1; 32]);
        let ticket = PairingToken::from_bytes([4; 32]);
        let username = format!(
            "{PAIRING_USERNAME_PREFIX}{}.{}",
            ticket.to_base64url(),
            URL_SAFE_NO_PAD.encode("phone")
        );
        let mut state = DeviceAuthenticationState::new(HostSettings::default(), path);
        state
            .issue_pairing_ticket_with_token(host, vec![], 100, ticket)
            .unwrap();
        assert!(matches!(
            state.authenticate(&username, Ed25519PublicKey::from_bytes([2; 32]), host, 99),
            Err(ConnectionAuthenticationError::Settings(_))
        ));
        assert_eq!(state.pairing_tickets.len(), 1);
    }
}

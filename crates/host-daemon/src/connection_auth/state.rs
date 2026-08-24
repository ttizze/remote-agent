use std::path::PathBuf;

use host_protocol::{
    AuthenticationChallenge, AuthenticationProof, Ed25519PublicKey, PairingQrPayload,
    PairingRequest, PairingToken,
};

use super::ConnectionAuthenticationError;
use crate::{
    AcceptedDevice, AuthenticationChallenges, HostSettings, PairedDevice, PairingError,
    PairingTickets, save_settings,
};

pub struct DeviceAuthenticationState {
    pairing_tickets: PairingTickets,
    challenges: AuthenticationChallenges,
    settings: HostSettings,
    settings_path: PathBuf,
    challenge_ttl_ms: u64,
}

impl DeviceAuthenticationState {
    pub fn new(settings: HostSettings, settings_path: PathBuf, challenge_ttl_ms: u64) -> Self {
        Self {
            pairing_tickets: PairingTickets::new(),
            challenges: AuthenticationChallenges::new(),
            settings,
            settings_path,
            challenge_ttl_ms,
        }
    }

    pub fn settings(&self) -> &HostSettings {
        &self.settings
    }

    pub fn settings_mut(&mut self) -> &mut HostSettings {
        &mut self.settings
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
        token: PairingToken,
    ) -> Result<PairingQrPayload, PairingError> {
        self.pairing_tickets
            .issue_with_token(host_identity, addresses, expires_at_ms, token)
    }

    pub(crate) fn accept_pairing(
        &mut self,
        request: PairingRequest,
        now_ms: u64,
    ) -> Result<AcceptedDevice, ConnectionAuthenticationError> {
        let accepted = self.pairing_tickets.consume(request, now_ms)?;
        let mut candidate = self.settings.clone();
        candidate.upsert_paired_device(PairedDevice {
            identity: accepted.identity,
            name: accepted.name.clone(),
        });
        save_settings(&self.settings_path, &candidate)?;
        self.settings = candidate;
        Ok(accepted)
    }

    pub(crate) fn issue_authentication_challenge(
        &mut self,
        host_identity: Ed25519PublicKey,
        device_identity: Ed25519PublicKey,
        now_ms: u64,
    ) -> Result<AuthenticationChallenge, ConnectionAuthenticationError> {
        if !self
            .settings
            .paired_devices
            .iter()
            .any(|device| device.identity == device_identity)
        {
            return Err(ConnectionAuthenticationError::DeviceNotPaired);
        }
        let expires_at_ms = now_ms
            .checked_add(self.challenge_ttl_ms)
            .ok_or(ConnectionAuthenticationError::InvalidChallengeExpiry)?;
        Ok(self
            .challenges
            .issue(host_identity, device_identity, expires_at_ms)?)
    }

    pub(crate) fn verify_authentication(
        &mut self,
        proof: AuthenticationProof,
        now_ms: u64,
    ) -> Result<Ed25519PublicKey, ConnectionAuthenticationError> {
        Ok(self.challenges.verify(proof, now_ms)?)
    }
}

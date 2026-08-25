use std::{collections::HashMap, collections::hash_map::Entry};

use host_protocol::{CURRENT_PROTOCOL_VERSION, Ed25519PublicKey, PairingQrPayload, PairingToken};
use ring::rand::{SecureRandom, SystemRandom};

pub struct PairingTickets {
    tickets: HashMap<PairingToken, PairingTicket>,
}

#[derive(Debug, Clone, Copy)]
struct PairingTicket {
    host_identity: Ed25519PublicKey,
    expires_at_ms: u64,
}

impl PairingTickets {
    pub fn new() -> Self {
        Self {
            tickets: HashMap::new(),
        }
    }

    pub fn issue(
        &mut self,
        host_identity: Ed25519PublicKey,
        addresses: Vec<String>,
        expires_at_ms: u64,
    ) -> Result<PairingQrPayload, PairingError> {
        let mut bytes = [0_u8; 32];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| PairingError::Random)?;
        self.issue_with_token(
            host_identity,
            addresses,
            expires_at_ms,
            PairingToken::from_bytes(bytes),
        )
    }

    pub fn issue_with_token(
        &mut self,
        host_identity: Ed25519PublicKey,
        addresses: Vec<String>,
        expires_at_ms: u64,
        ticket: PairingToken,
    ) -> Result<PairingQrPayload, PairingError> {
        match self.tickets.entry(ticket) {
            Entry::Vacant(entry) => {
                entry.insert(PairingTicket {
                    host_identity,
                    expires_at_ms,
                });
            }
            Entry::Occupied(_) => return Err(PairingError::DuplicateTicket),
        }
        Ok(PairingQrPayload {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            host_identity,
            addresses,
            ticket,
            expires_at_ms,
        })
    }

    pub fn validate(
        &self,
        ticket: PairingToken,
        host_identity: Ed25519PublicKey,
        now_ms: u64,
    ) -> Result<(), PairingError> {
        let Some(expected) = self.tickets.get(&ticket) else {
            return Err(PairingError::UnknownOrConsumedTicket);
        };
        if expected.host_identity != host_identity {
            return Err(PairingError::HostIdentityMismatch);
        }
        if now_ms >= expected.expires_at_ms {
            return Err(PairingError::ExpiredTicket);
        }
        Ok(())
    }

    /// Consume only after the paired-device settings have been persisted.
    pub fn consume(&mut self, ticket: PairingToken) -> Result<(), PairingError> {
        self.tickets
            .remove(&ticket)
            .map(|_| ())
            .ok_or(PairingError::UnknownOrConsumedTicket)
    }

    pub fn purge_expired(&mut self, now_ms: u64) {
        self.tickets
            .retain(|_, ticket| now_ms < ticket.expires_at_ms);
    }

    pub fn len(&self) -> usize {
        self.tickets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tickets.is_empty()
    }
}

impl Default for PairingTickets {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedDevice {
    pub identity: Ed25519PublicKey,
    pub name: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PairingError {
    #[error("secure random generation failed")]
    Random,
    #[error("pairing ticket already exists")]
    DuplicateTicket,
    #[error("pairing ticket is unknown or has already been consumed")]
    UnknownOrConsumedTicket,
    #[error("pairing ticket has expired")]
    ExpiredTicket,
    #[error("pairing ticket belongs to another Host identity")]
    HostIdentityMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: Ed25519PublicKey = Ed25519PublicKey::from_bytes([1; 32]);
    const TICKET: PairingToken = PairingToken::from_bytes([2; 32]);

    #[test]
    fn ticket_is_validated_then_consumed() {
        let mut tickets = PairingTickets::new();
        tickets.issue_with_token(HOST, vec![], 100, TICKET).unwrap();
        tickets.validate(TICKET, HOST, 99).unwrap();
        tickets.consume(TICKET).unwrap();
        assert_eq!(
            tickets.validate(TICKET, HOST, 99),
            Err(PairingError::UnknownOrConsumedTicket)
        );
    }

    #[test]
    fn expired_and_foreign_tickets_are_rejected_without_consuming_them() {
        let mut tickets = PairingTickets::new();
        tickets.issue_with_token(HOST, vec![], 100, TICKET).unwrap();
        assert_eq!(
            tickets.validate(TICKET, HOST, 100),
            Err(PairingError::ExpiredTicket)
        );
        assert_eq!(
            tickets.validate(TICKET, Ed25519PublicKey::from_bytes([3; 32]), 99),
            Err(PairingError::HostIdentityMismatch)
        );
        assert_eq!(tickets.len(), 1);
    }
}

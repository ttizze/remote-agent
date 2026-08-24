use std::collections::{HashMap, hash_map::Entry};

use host_protocol::{
    AuthenticationChallenge, AuthenticationChallengeToken, AuthenticationProof,
    CURRENT_PROTOCOL_VERSION, Ed25519PublicKey, PairingQrPayload, PairingRequest, PairingToken,
    authentication_proof_message, pairing_proof_message,
};
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{ED25519, UnparsedPublicKey};

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

    pub fn consume(
        &mut self,
        request: PairingRequest,
        now_ms: u64,
    ) -> Result<AcceptedDevice, PairingError> {
        let Some(ticket) = self.tickets.remove(&request.ticket) else {
            return Err(PairingError::UnknownOrConsumedTicket);
        };
        if now_ms >= ticket.expires_at_ms {
            return Err(PairingError::ExpiredTicket);
        }
        let message = pairing_proof_message(
            ticket.host_identity,
            request.ticket,
            request.device_identity,
        );
        verify_signature(
            request.device_identity,
            &message,
            request.signature.as_bytes(),
        )?;
        Ok(AcceptedDevice {
            identity: request.device_identity,
            name: request.device_name,
        })
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
    #[error("device signature is invalid")]
    InvalidSignature,
}

pub struct AuthenticationChallenges {
    challenges: HashMap<AuthenticationChallengeToken, AuthenticationAttempt>,
}

#[derive(Debug, Clone, Copy)]
struct AuthenticationAttempt {
    host_identity: Ed25519PublicKey,
    device_identity: Ed25519PublicKey,
    expires_at_ms: u64,
}

impl AuthenticationChallenges {
    pub fn new() -> Self {
        Self {
            challenges: HashMap::new(),
        }
    }

    pub fn issue(
        &mut self,
        host_identity: Ed25519PublicKey,
        device_identity: Ed25519PublicKey,
        expires_at_ms: u64,
    ) -> Result<AuthenticationChallenge, AuthenticationError> {
        let mut bytes = [0_u8; 32];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| AuthenticationError::Random)?;
        self.issue_with_token(
            host_identity,
            device_identity,
            expires_at_ms,
            AuthenticationChallengeToken::from_bytes(bytes),
        )
    }

    pub fn issue_with_token(
        &mut self,
        host_identity: Ed25519PublicKey,
        device_identity: Ed25519PublicKey,
        expires_at_ms: u64,
        token: AuthenticationChallengeToken,
    ) -> Result<AuthenticationChallenge, AuthenticationError> {
        match self.challenges.entry(token) {
            Entry::Vacant(entry) => {
                entry.insert(AuthenticationAttempt {
                    host_identity,
                    device_identity,
                    expires_at_ms,
                });
            }
            Entry::Occupied(_) => return Err(AuthenticationError::DuplicateChallenge),
        }
        Ok(AuthenticationChallenge {
            token,
            host_identity,
            expires_at_ms,
        })
    }

    pub fn verify(
        &mut self,
        proof: AuthenticationProof,
        now_ms: u64,
    ) -> Result<Ed25519PublicKey, AuthenticationError> {
        let Some(attempt) = self.challenges.remove(&proof.token) else {
            return Err(AuthenticationError::UnknownOrConsumedChallenge);
        };
        if now_ms >= attempt.expires_at_ms {
            return Err(AuthenticationError::ExpiredChallenge);
        }
        let message = authentication_proof_message(
            attempt.host_identity,
            proof.token,
            attempt.device_identity,
        );
        verify_signature(
            attempt.device_identity,
            &message,
            proof.signature.as_bytes(),
        )
        .map_err(|_| AuthenticationError::InvalidSignature)?;
        Ok(attempt.device_identity)
    }

    pub fn purge_expired(&mut self, now_ms: u64) {
        self.challenges
            .retain(|_, attempt| now_ms < attempt.expires_at_ms);
    }
}

impl Default for AuthenticationChallenges {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AuthenticationError {
    #[error("secure random generation failed")]
    Random,
    #[error("authentication challenge already exists")]
    DuplicateChallenge,
    #[error("authentication challenge is unknown or has already been consumed")]
    UnknownOrConsumedChallenge,
    #[error("authentication challenge has expired")]
    ExpiredChallenge,
    #[error("device signature is invalid")]
    InvalidSignature,
}

fn verify_signature(
    identity: Ed25519PublicKey,
    message: &[u8],
    signature: &[u8],
) -> Result<(), PairingError> {
    UnparsedPublicKey::new(&ED25519, identity.as_bytes())
        .verify(message, signature)
        .map_err(|_| PairingError::InvalidSignature)
}

#[cfg(test)]
mod tests {
    use ring::signature::{Ed25519KeyPair, KeyPair};

    use super::*;

    const HOST_KEY: Ed25519PublicKey = Ed25519PublicKey::from_bytes([1; 32]);

    fn key_pair() -> Ed25519KeyPair {
        let document = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        Ed25519KeyPair::from_pkcs8(document.as_ref()).unwrap()
    }

    fn public_key(key_pair: &Ed25519KeyPair) -> Ed25519PublicKey {
        Ed25519PublicKey::from_bytes(key_pair.public_key().as_ref().try_into().unwrap())
    }

    fn signature(key_pair: &Ed25519KeyPair, message: &[u8]) -> host_protocol::Ed25519Signature {
        host_protocol::Ed25519Signature::from_bytes(
            key_pair.sign(message).as_ref().try_into().unwrap(),
        )
    }

    fn pairing_request(key_pair: &Ed25519KeyPair, ticket: PairingToken) -> PairingRequest {
        let device_identity = public_key(key_pair);
        PairingRequest {
            ticket,
            device_identity,
            device_name: "phone".to_owned(),
            signature: signature(
                key_pair,
                &pairing_proof_message(HOST_KEY, ticket, device_identity),
            ),
        }
    }

    #[test]
    fn consumes_ticket_exactly_once() {
        let key_pair = key_pair();
        let device_key = public_key(&key_pair);
        let token = PairingToken::from_bytes([3; 32]);
        let mut tickets = PairingTickets::new();
        let payload = tickets
            .issue_with_token(HOST_KEY, vec!["127.0.0.1:49152".to_owned()], 200, token)
            .unwrap();
        assert_eq!(payload.ticket, token);

        let accepted = tickets
            .consume(pairing_request(&key_pair, token), 100)
            .unwrap();
        assert_eq!(accepted.identity, device_key);
        assert_eq!(accepted.name, "phone");

        assert_eq!(
            tickets.consume(pairing_request(&key_pair, token), 100),
            Err(PairingError::UnknownOrConsumedTicket)
        );
    }

    #[test]
    fn expiry_consumes_ticket_and_purge_removes_old_tickets() {
        let key_pair = key_pair();
        let expired = PairingToken::from_bytes([4; 32]);
        let purge = PairingToken::from_bytes([5; 32]);
        let mut tickets = PairingTickets::new();
        tickets
            .issue_with_token(HOST_KEY, vec![], 99, expired)
            .unwrap();
        tickets
            .issue_with_token(HOST_KEY, vec![], 99, purge)
            .unwrap();

        assert_eq!(
            tickets.consume(pairing_request(&key_pair, expired), 100),
            Err(PairingError::ExpiredTicket)
        );
        tickets.purge_expired(100);
        assert!(tickets.is_empty());
    }

    #[test]
    fn rejects_duplicate_ticket_without_replacing_original_expiry() {
        let key_pair = key_pair();
        let token = PairingToken::from_bytes([6; 32]);
        let mut tickets = PairingTickets::new();
        tickets
            .issue_with_token(HOST_KEY, vec![], 200, token)
            .unwrap();
        assert_eq!(
            tickets.issue_with_token(HOST_KEY, vec![], 999, token),
            Err(PairingError::DuplicateTicket)
        );
        assert!(
            tickets
                .consume(pairing_request(&key_pair, token), 300)
                .is_err()
        );
    }

    #[test]
    fn rejects_invalid_pairing_signature_and_consumes_ticket() {
        let legitimate = key_pair();
        let attacker = key_pair();
        let token = PairingToken::from_bytes([7; 32]);
        let mut tickets = PairingTickets::new();
        tickets
            .issue_with_token(HOST_KEY, vec![], 200, token)
            .unwrap();

        let device_identity = public_key(&legitimate);
        let request = PairingRequest {
            ticket: token,
            device_identity,
            device_name: "phone".to_owned(),
            signature: signature(
                &attacker,
                &pairing_proof_message(HOST_KEY, token, device_identity),
            ),
        };
        assert_eq!(
            tickets.consume(request, 100),
            Err(PairingError::InvalidSignature)
        );
        assert_eq!(
            tickets.consume(pairing_request(&legitimate, token), 100),
            Err(PairingError::UnknownOrConsumedTicket)
        );
    }

    #[test]
    fn verifies_authentication_challenge_exactly_once() {
        let key_pair = key_pair();
        let device_identity = public_key(&key_pair);
        let token = AuthenticationChallengeToken::from_bytes([8; 32]);
        let mut challenges = AuthenticationChallenges::new();
        challenges
            .issue_with_token(HOST_KEY, device_identity, 200, token)
            .unwrap();
        let proof = AuthenticationProof {
            token,
            signature: signature(
                &key_pair,
                &authentication_proof_message(HOST_KEY, token, device_identity),
            ),
        };

        assert_eq!(challenges.verify(proof.clone(), 100), Ok(device_identity));
        assert_eq!(
            challenges.verify(proof, 100),
            Err(AuthenticationError::UnknownOrConsumedChallenge)
        );
    }

    #[test]
    fn authentication_rejects_expired_or_wrong_key_proofs() {
        let legitimate = key_pair();
        let attacker = key_pair();
        let device_identity = public_key(&legitimate);
        let expired_token = AuthenticationChallengeToken::from_bytes([9; 32]);
        let invalid_token = AuthenticationChallengeToken::from_bytes([10; 32]);
        let mut challenges = AuthenticationChallenges::new();
        challenges
            .issue_with_token(HOST_KEY, device_identity, 100, expired_token)
            .unwrap();
        challenges
            .issue_with_token(HOST_KEY, device_identity, 200, invalid_token)
            .unwrap();

        let expired = AuthenticationProof {
            token: expired_token,
            signature: signature(
                &legitimate,
                &authentication_proof_message(HOST_KEY, expired_token, device_identity),
            ),
        };
        assert_eq!(
            challenges.verify(expired, 100),
            Err(AuthenticationError::ExpiredChallenge)
        );

        let invalid = AuthenticationProof {
            token: invalid_token,
            signature: signature(
                &attacker,
                &authentication_proof_message(HOST_KEY, invalid_token, device_identity),
            ),
        };
        assert_eq!(
            challenges.verify(invalid.clone(), 100),
            Err(AuthenticationError::InvalidSignature)
        );
        assert_eq!(
            challenges.verify(invalid, 100),
            Err(AuthenticationError::UnknownOrConsumedChallenge)
        );
    }

    #[test]
    fn authentication_proof_is_bound_to_host_identity() {
        let key_pair = key_pair();
        let device_identity = public_key(&key_pair);
        let other_host = Ed25519PublicKey::from_bytes([11; 32]);
        let token = AuthenticationChallengeToken::from_bytes([12; 32]);
        let mut challenges = AuthenticationChallenges::new();
        challenges
            .issue_with_token(HOST_KEY, device_identity, 200, token)
            .unwrap();
        let proof = AuthenticationProof {
            token,
            signature: signature(
                &key_pair,
                &authentication_proof_message(other_host, token, device_identity),
            ),
        };

        assert_eq!(
            challenges.verify(proof, 100),
            Err(AuthenticationError::InvalidSignature)
        );
    }
}

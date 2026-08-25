mod auth;
mod jsonl;
mod rpc;

pub use auth::{Ed25519PublicKey, PairingQrPayload, PairingToken};
pub use jsonl::{DEFAULT_MAX_MESSAGE_BYTES, JsonlError, JsonlReader, JsonlWriter};
pub use rpc::{
    RpcMessage, RpcMessageError, RpcMessageKind, classify_message, raw_object, rewrite_top_level_id,
};

pub const CURRENT_PROTOCOL_VERSION: u16 = 3;
pub const SSH_SUBSYSTEM: &str = "remote-agent-v3";

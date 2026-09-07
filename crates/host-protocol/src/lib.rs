mod auth;
mod jsonl;
mod relay;
mod rpc;

pub use jsonl::{DEFAULT_MAX_MESSAGE_BYTES, JsonlError, JsonlReader, JsonlWriter};
pub use rpc::{
    RpcMessage, RpcMessageError, RpcMessageKind, classify_message, raw_object, rewrite_top_level_id,
};

pub const CURRENT_PROTOCOL_VERSION: u16 = 4;
pub const SSH_SUBSYSTEM: &str = "remote-agent-v4";
pub const BLOB_SUBSYSTEM: &str = "remote-agent-blob-v4";
pub use auth::{Base64UrlError, Ed25519PublicKey, PairingQrPayload, PairingToken};
pub use relay::{RelayEndpoint, RelayEndpointError};

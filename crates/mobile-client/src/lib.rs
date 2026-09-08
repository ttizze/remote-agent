//! Phoenix relay client core for the Bex Mobile Client.
//!
//! The client establishes pinned-key SSH through an opaque Phoenix byte tunnel.
//! Codex JSONL is visible only inside the authenticated SSH subsystem. Agent
//! execution and permission semantics remain outside this crate.

mod client;
mod rpc;
mod transfers;
mod transport;

pub mod ffi;

#[cfg(feature = "jni")]
mod android_jni;

pub use client::{
    ConnectedHost, MobileClient, MobileClientConfig, MobileClientError, Notification, ServerRequest,
};

/// Proxy a local application's raw RPC stream through the same pinned,
/// authenticated transport used by MobileClient. No JSON fields or IDs change.
pub async fn forward_rpc<S>(
    config: MobileClientConfig,
    device_pkcs8: &[u8],
    mut local: S,
    shutdown: tokio_util::sync::CancellationToken,
) -> Result<(), MobileClientError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    config.validate()?;
    let key = transport::decode_device_key(device_pkcs8)?;
    let mut channel = tokio::select! {
        _ = shutdown.cancelled() => return Ok(()),
        result = tokio::time::timeout(config.request_timeout, transport::establish(&config, key)) => result.map_err(|_| MobileClientError::ConnectionTimeout)??,
    };
    tokio::select! {
        _ = shutdown.cancelled() => {},
        result = tokio::io::copy_bidirectional(&mut local, &mut channel.stream) => { result?; },
    }
    drop(channel);
    Ok(())
}

#[cfg(test)]
mod tests;

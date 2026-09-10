use std::{future::Future, path::Path};

use crate::models::TransferGrant;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::digest::{Context, SHA256};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt};

use crate::peer::{PeerError, RpcPeer};

#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    #[error("transfer I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid transfer JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Peer(#[from] PeerError),
    #[error("transfer protocol violation: {0}")]
    Protocol(String),
}

const LIMIT: u64 = 512 * 1024 * 1024;

impl TransferGrant {
    fn validate(self) -> Result<Self, TransferError> {
        let grant = self;
        let mut digest = [0; 32];
        let mut token = [0; 32];
        if grant.size > LIMIT
            || grant.token.len() != 43
            || grant.sha256.len() != 43
            || URL_SAFE_NO_PAD
                .decode_slice(&grant.sha256, &mut digest)
                .ok()
                != Some(32)
            || URL_SAFE_NO_PAD.decode_slice(&grant.token, &mut token).ok() != Some(32)
        {
            return Err(TransferError::Protocol("invalid transfer grant".into()));
        }
        Ok(grant)
    }
}

/// Uploads a picked local file to a directory on the paired Host. An empty
/// directory selects Host-owned attachment storage for chats without a workspace.
/// The Host assigns the actual filename and never trusts the display name
/// as a destination path. No file payload is embedded in JSON-RPC.
pub async fn upload_file<S, F, Fut>(
    peer: &RpcPeer,
    open_stream: F,
    source: &Path,
    directory: &Path,
    file_name: &str,
) -> Result<Value, TransferError>
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: FnOnce() -> Fut,
    Fut: Future<Output = std::io::Result<S>>,
{
    let mut file = tokio::fs::File::open(source).await?;
    if !file.metadata().await?.is_file() {
        return Err(TransferError::Protocol(
            "upload source is not a regular file".into(),
        ));
    }
    let mut digest = Context::new(&SHA256);
    let mut size = 0;
    let mut buffer = [0; 32768];
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        size += read as u64;
        if size > LIMIT {
            return Err(TransferError::Protocol("transfer exceeds 512 MiB".into()));
        }
        digest.update(&buffer[..read]);
    }
    let sha256 = URL_SAFE_NO_PAD.encode(digest.finish().as_ref());
    file.rewind().await?;
    let grant = peer
        .request::<_, TransferGrant>(
            "host/blob/upload",
            &json!({"directory":directory,"fileName":file_name,"size":size,"sha256":sha256}),
        )
        .await?
        .value
        .validate()?;
    if grant.size != size || grant.sha256 != sha256 {
        return Err(TransferError::Protocol(
            "upload grant changed content metadata".into(),
        ));
    }
    let mut stream = open_stream().await?;
    stream.write_u32(grant.token.len() as u32).await?;
    stream.write_all(grant.token.as_bytes()).await?;
    // Copy through EOF so growing input cannot be silently truncated.
    // The Host checks both the declared size and content digest.
    tokio::io::copy(&mut file.take(LIMIT + 1), &mut stream).await?;
    stream.shutdown().await?;
    let length = stream.read_u32().await?;
    if length > 65536 {
        return Err(TransferError::Protocol(
            "transfer response too large".into(),
        ));
    }
    let mut response = vec![0; length as usize];
    stream.read_exact(&mut response).await?;
    if stream.read(&mut buffer[..1]).await? != 0 {
        return Err(TransferError::Protocol(
            "upload acknowledgement has trailing data".into(),
        ));
    }
    let response: Value = serde_json::from_slice(&response)?;
    if response["sha256"] != sha256
        || response["size"] != size
        || !response["path"]
            .as_str()
            .is_some_and(|path| Path::new(path).is_absolute())
    {
        return Err(TransferError::Protocol(
            "upload acknowledgement changed content metadata".into(),
        ));
    }
    Ok(response)
}

/// Writes only a fully received, digest-checked download. A failed transfer
/// drops its temporary file; an existing destination is never overwritten.
pub async fn download_file<S, F, Fut>(
    peer: &RpcPeer,
    open_stream: F,
    source: &Path,
    destination: &Path,
) -> Result<(), TransferError>
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: FnOnce() -> Fut,
    Fut: Future<Output = std::io::Result<S>>,
{
    let grant = peer
        .request::<_, TransferGrant>("host/blob/download", &json!({"path":source}))
        .await?
        .value
        .validate()?;
    let parent = destination
        .parent()
        .ok_or_else(|| TransferError::Protocol("download destination has no parent".into()))?;
    let output = tempfile::NamedTempFile::new_in(parent)?;
    let mut file = tokio::fs::File::from_std(output.reopen()?);
    let mut stream = open_stream().await?;
    stream.write_u32(grant.token.len() as u32).await?;
    stream.write_all(grant.token.as_bytes()).await?;
    let mut digest = Context::new(&SHA256);
    let mut remaining = grant.size;
    let mut buffer = [0; 32768];
    while remaining > 0 {
        let length = remaining.min(buffer.len() as u64) as usize;
        stream.read_exact(&mut buffer[..length]).await?;
        digest.update(&buffer[..length]);
        file.write_all(&buffer[..length]).await?;
        remaining -= length as u64;
    }
    if stream.read(&mut buffer[..1]).await? != 0
        || URL_SAFE_NO_PAD.encode(digest.finish().as_ref()) != grant.sha256
    {
        return Err(TransferError::Protocol(
            "download content digest or length mismatch".into(),
        ));
    }
    file.sync_all().await?;
    drop(file);
    output
        .persist_noclobber(destination)
        .map_err(|error| TransferError::Io(error.error))?;
    Ok(())
}

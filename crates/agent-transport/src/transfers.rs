use std::{future::Future, path::Path};

use crate::models::TransferGrant;
use ring::digest::{Context, SHA256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt};

use crate::{client::Client, peer::PeerError};

#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    #[error("transfer I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Peer(#[from] PeerError),
    #[error("transfer protocol violation: {0}")]
    Protocol(String),
}

const LIMIT: u64 = 512 * 1024 * 1024;

/// Uploads a picked local file to a directory on the paired Host. An empty
/// directory selects Host-owned attachment storage for chats without a workspace.
/// The Host assigns the actual filename and never trusts the display name
/// as a destination path. No file payload is embedded in JSON-RPC.
pub async fn upload_file<S, F, Fut>(
    peer: &Client,
    open_stream: F,
    source: &Path,
    directory: &Path,
    file_name: &str,
) -> Result<crate::models::UploadedFile, TransferError>
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: FnOnce() -> Fut,
    Fut: Future<Output = std::io::Result<S>>,
{
    upload_with_purpose(peer, open_stream, source, directory, file_name, None, None).await
}
pub async fn upload_attachment<S, F, Fut>(
    peer: &Client,
    open_stream: F,
    source: &Path,
    name: &str,
    mime: &str,
) -> Result<crate::models::UploadedFile, TransferError>
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: FnOnce() -> Fut,
    Fut: Future<Output = std::io::Result<S>>,
{
    upload_attachment_with_source(peer, open_stream, source, name, mime, None).await
}

/// Uploads an attachment and preserves optional screenshot window metadata.
/// The metadata travels in the grant request; the image bytes remain a
/// digest-checked stream.
pub async fn upload_attachment_with_source<S, F, Fut>(
    peer: &Client,
    open_stream: F,
    source: &Path,
    name: &str,
    mime: &str,
    captured_window: Option<agent_domain::CapturedWindow>,
) -> Result<crate::models::UploadedFile, TransferError>
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: FnOnce() -> Fut,
    Fut: Future<Output = std::io::Result<S>>,
{
    upload_with_purpose(
        peer,
        open_stream,
        source,
        Path::new(""),
        name,
        Some(mime.to_ascii_lowercase()),
        captured_window,
    )
    .await
}
async fn upload_with_purpose<S, F, Fut>(
    peer: &Client,
    open_stream: F,
    source: &Path,
    directory: &Path,
    file_name: &str,
    attachment_mime_type: Option<String>,
    captured_window: Option<agent_domain::CapturedWindow>,
) -> Result<crate::models::UploadedFile, TransferError>
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
    let sha256: [u8; 32] = digest.finish().as_ref().try_into().expect("SHA-256 length");
    file.rewind().await?;
    let grant = peer
        .request::<TransferGrant>(&crate::protocol::Call::Upload(
            agent_protocol::operations::Upload {
                attachment_mime_type,
                source: captured_window,
                directory: directory
                    .to_str()
                    .ok_or_else(|| TransferError::Protocol("directory is not UTF-8".into()))?
                    .into(),
                file_name: file_name.into(),
                size,
                sha256,
            },
        ))
        .await?;
    if grant.size != size || grant.sha256 != sha256 {
        return Err(TransferError::Protocol(
            "upload grant changed content metadata".into(),
        ));
    }
    let mut stream = open_stream().await?;
    stream.write_all(&grant.token).await?;
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
    let response: crate::models::UploadedFile = crate::protocol::decode(&response)?;
    if response.sha256 != sha256
        || response.size != size
        || !Path::new(&response.path).is_absolute()
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
    peer: &Client,
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
        .request::<TransferGrant>(&crate::protocol::Call::Download(
            agent_protocol::operations::ListFiles {
                path: source
                    .to_str()
                    .ok_or_else(|| TransferError::Protocol("path is not UTF-8".into()))?
                    .into(),
            },
        ))
        .await?;
    let parent = destination
        .parent()
        .ok_or_else(|| TransferError::Protocol("download destination has no parent".into()))?;
    let output = tempfile::NamedTempFile::new_in(parent)?;
    let mut file = tokio::fs::File::from_std(output.reopen()?);
    receive_download(grant, open_stream().await?, &mut file).await?;
    file.sync_all().await?;
    drop(file);
    output
        .persist_noclobber(destination)
        .map_err(|error| TransferError::Io(error.error))?;
    Ok(())
}

async fn receive_download<S, W>(
    grant: TransferGrant,
    mut stream: S,
    output: &mut W,
) -> Result<(), TransferError>
where
    S: AsyncRead + AsyncWrite + Unpin,
    W: AsyncWrite + Unpin,
{
    if grant.size > LIMIT {
        return Err(TransferError::Protocol("transfer exceeds 512 MiB".into()));
    }
    stream.write_all(&grant.token).await?;
    let mut digest = Context::new(&SHA256);
    let mut remaining = grant.size;
    let mut buffer = [0; 32768];
    while remaining > 0 {
        let length = remaining.min(buffer.len() as u64) as usize;
        stream.read_exact(&mut buffer[..length]).await?;
        digest.update(&buffer[..length]);
        output.write_all(&buffer[..length]).await?;
        remaining -= length as u64;
    }
    if stream.read(&mut buffer[..1]).await? != 0 || digest.finish().as_ref() != grant.sha256 {
        return Err(TransferError::Protocol(
            "download content digest or length mismatch".into(),
        ));
    }
    Ok(())
}

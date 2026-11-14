use std::{future::Future, path::Path};

use crate::models::TransferGrant;
use agent_protocol::paths::is_absolute_host_path;
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

impl From<TransferError> for PeerError {
    fn from(error: TransferError) -> Self {
        match error {
            TransferError::Peer(error) => error,
            TransferError::Io(error) => Self::Transfer(error.to_string()),
            TransferError::Protocol(reason) => Self::Transfer(reason),
        }
    }
}

const LIMIT: u64 = 512 * 1024 * 1024;

/// Uploads a picked local file to attachment storage owned by the paired Host,
/// independently of the conversation's workspace.
/// The Host assigns the actual filename and never trusts the display name
/// as a destination path. No file payload is embedded in JSON-RPC.
pub async fn upload_file<S, F, Fut>(
    peer: &Client,
    open_stream: F,
    source: &Path,
    file_name: &str,
) -> Result<crate::models::UploadedFile, TransferError>
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: FnOnce() -> Fut,
    Fut: Future<Output = std::io::Result<S>>,
{
    let mut options = tokio::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NONBLOCK);
    let mut file = options.open(source).await?;
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
            return Err(TransferError::Protocol(
                "512 MiBを超えるファイルは転送できません。".into(),
            ));
        }
        digest.update(&buffer[..read]);
    }
    let sha256: [u8; 32] = digest.finish().as_ref().try_into().expect("SHA-256 length");
    file.rewind().await?;
    let grant = peer
        .request::<TransferGrant>(&crate::protocol::Call::Upload(
            agent_protocol::operations::Upload {
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
    validate_upload_acknowledgement(&response, size, sha256)?;
    Ok(response)
}

fn validate_upload_acknowledgement(
    response: &crate::models::UploadedFile,
    size: u64,
    sha256: [u8; 32],
) -> Result<(), TransferError> {
    if response.sha256 != sha256 || response.size != size {
        return Err(TransferError::Protocol(
            "upload acknowledgement changed content metadata".into(),
        ));
    }
    if !is_absolute_host_path(&response.path) {
        return Err(TransferError::Protocol(
            "upload acknowledgement has a relative Host path".into(),
        ));
    }
    Ok(())
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

pub async fn download_bytes<S, F, Fut>(
    grant: TransferGrant,
    open_stream: F,
) -> Result<Vec<u8>, TransferError>
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: FnOnce() -> Fut,
    Fut: Future<Output = std::io::Result<S>>,
{
    let mut bytes = Vec::new();
    receive_download(grant, open_stream().await?, &mut bytes).await?;
    Ok(bytes)
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
        return Err(TransferError::Protocol(
            "512 MiBを超えるファイルは転送できません。".into(),
        ));
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

pub async fn resolve_item(
    grant: TransferGrant,
    expected: &agent_protocol::ids::ItemId,
    session: Option<&crate::transport::Session>,
) -> Result<crate::models::Item, PeerError> {
    let session = session.ok_or_else(|| {
        PeerError::InvalidMessage("item transfer requires an iroh session".into())
    })?;
    let bytes = crate::transfers::download_bytes(grant, || async {
        session.open_stream().await.map_err(std::io::Error::other)
    })
    .await
    .map_err(PeerError::from)?;
    let item: crate::models::Item = crate::protocol::decode(&bytes)
        .map_err(|error| PeerError::InvalidMessage(error.to_string()))?;
    if item.id != *expected {
        return Err(PeerError::InvalidMessage(
            "transferred item ID does not match".into(),
        ));
    }
    Ok(item)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_conversion_preserves_remote_failure_evidence() {
        let remote = PeerError::Remote {
            error: r#"{"code":"file_not_found","message":"missing file"}"#.into(),
            delivery: agent_protocol::error::Delivery::NotSent,
            sequence: Some(42),
        };
        let converted = PeerError::from(TransferError::from(remote.clone()));
        assert_eq!(converted.to_string(), remote.to_string());
        assert!(matches!(
            converted,
            PeerError::Remote {
                delivery: agent_protocol::error::Delivery::NotSent,
                sequence: Some(42),
                ..
            }
        ));
        for (error, reason) in [
            (
                TransferError::Io(std::io::Error::other("disk full")),
                "disk full",
            ),
            (
                TransferError::Protocol("digest mismatch".into()),
                "digest mismatch",
            ),
        ] {
            assert_eq!(PeerError::from(error).to_string(), reason);
        }
    }

    #[test]
    fn upload_acknowledgement_accepts_host_paths_across_client_platforms() {
        for path in [
            "/home/user/project/image.png",
            "/Users/user/project/image.png",
            r"C:\Users\user\project\image.png",
            "D:/project/image.png",
            r"\\server\share\project\image.png",
            r"\\?\C:\Users\user\project\image.png",
            r"\\?\UNC\server\share\image.png",
        ] {
            let response = crate::models::UploadedFile {
                path: path.into(),
                size: 42,
                sha256: [7; 32],
            };
            validate_upload_acknowledgement(&response, 42, [7; 32])
                .unwrap_or_else(|error| panic!("{path}: {error}"));
            // The opaque Host path must survive unchanged for later submission.
            assert_eq!(response.path, path);
            assert!(validate_upload_acknowledgement(&response, 43, [7; 32]).is_err());
            assert!(validate_upload_acknowledgement(&response, 42, [8; 32]).is_err());
        }
    }

    #[test]
    fn upload_acknowledgement_rejects_relative_or_invalid_host_paths() {
        for path in [
            "",
            "image.png",
            "../image.png",
            r"C:image.png",
            r"\image.png",
            r"\\server",
            r"\\server\",
            "/image\0.png",
        ] {
            let response = crate::models::UploadedFile {
                path: path.into(),
                size: 42,
                sha256: [7; 32],
            };
            let error = validate_upload_acknowledgement(&response, 42, [7; 32]).expect_err(path);
            assert!(error.to_string().contains("relative Host path"), "{error}");
        }
    }
}

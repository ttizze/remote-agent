use std::{path::Path, time::Duration};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::digest::{Context, SHA256};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::{MobileClient, MobileClientError, transport};

const LIMIT: u64 = 512 * 1024 * 1024;

#[derive(Deserialize)]
struct Grant {
    token: String,
    size: u64,
    sha256: String,
}

impl Grant {
    fn validate(self) -> Result<Self, MobileClientError> {
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
            return Err(MobileClientError::Protocol("invalid transfer grant".into()));
        }
        Ok(grant)
    }
}

impl MobileClient {
    /// Uploads a picked local file to an explicit directory on the paired Host.
    /// The Host assigns the actual filename and never trusts the display name
    /// as a destination path. No file payload is embedded in JSON-RPC.
    pub async fn upload_file(
        &self,
        source: &Path,
        directory: &Path,
        file_name: &str,
    ) -> Result<Value, MobileClientError> {
        tokio::time::timeout(Duration::from_secs(120), async {
            let mut file = tokio::fs::File::open(source).await?;
            if !file.metadata().await?.is_file() {
                return Err(MobileClientError::Protocol(
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
                    return Err(MobileClientError::Protocol(
                        "transfer exceeds 512 MiB".into(),
                    ));
                }
                digest.update(&buffer[..read]);
            }
            let sha256 = URL_SAFE_NO_PAD.encode(digest.finish().as_ref());
            file.rewind().await?;
            let grant = self.agent().request::<_, Grant>(
                    "host/blob/upload",
                    &json!({"directory":directory,"fileName":file_name,"size":size,"sha256":sha256}),
                )
                .await?.validate()?;
            if grant.size != size || grant.sha256 != sha256 {
                return Err(MobileClientError::Protocol(
                    "upload grant changed content metadata".into(),
                ));
            }
            let connection = self.connection()?;
            let mut stream =
                transport::open_subsystem(&connection.ssh, host_protocol::BLOB_SUBSYSTEM).await?;
            stream.write_u32(grant.token.len() as u32).await?;
            stream.write_all(grant.token.as_bytes()).await?;
            // Copy through EOF so growing input cannot be silently truncated.
            // The Host checks both the declared size and content digest.
            tokio::io::copy(&mut file.take(LIMIT + 1), &mut stream).await?;
            stream.shutdown().await?;
            let length = stream.read_u32().await?;
            if length > 65536 {
                return Err(MobileClientError::Protocol(
                    "transfer response too large".into(),
                ));
            }
            let mut response = vec![0; length as usize];
            stream.read_exact(&mut response).await?;
            let response: Value = serde_json::from_slice(&response)?;
            if response["sha256"] != sha256
                || response["size"] != size
                || !response["path"]
                    .as_str()
                    .is_some_and(|path| Path::new(path).is_absolute())
            {
                return Err(MobileClientError::Protocol(
                    "upload acknowledgement changed content metadata".into(),
                ));
            }
            Ok(response)
        })
        .await
        .map_err(|_| MobileClientError::Protocol("upload timed out".into()))?
    }

    /// Writes only a fully received, digest-checked download. A failed transfer
    /// drops its temporary file; an existing destination is never overwritten.
    pub async fn download_file(
        &self,
        source: &Path,
        destination: &Path,
    ) -> Result<(), MobileClientError> {
        tokio::time::timeout(Duration::from_secs(120), async {
            let grant = self
                .agent()
                .request::<_, Grant>("host/blob/download", &json!({"path":source}))
                .await?
                .validate()?;
            let parent = destination.parent().ok_or_else(|| {
                MobileClientError::Protocol("download destination has no parent".into())
            })?;
            let output = tempfile::NamedTempFile::new_in(parent)?;
            let mut file = tokio::fs::File::from_std(output.reopen()?);
            let connection = self.connection()?;
            let mut stream =
                transport::open_subsystem(&connection.ssh, host_protocol::BLOB_SUBSYSTEM).await?;
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
                return Err(MobileClientError::Protocol(
                    "download content digest or length mismatch".into(),
                ));
            }
            file.sync_all().await?;
            drop(file);
            output
                .persist_noclobber(destination)
                .map_err(|error| MobileClientError::Io(error.error))?;
            Ok(())
        })
        .await
        .map_err(|_| MobileClientError::Protocol("download timed out".into()))?
    }
}

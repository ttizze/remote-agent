use std::{
    collections::HashMap,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use agent_core::models::{FileContent, FileEntry, FileList, TransferGrant};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::{
    digest::{self, Context, SHA256},
    rand::{SecureRandom, SystemRandom},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::SessionId;

const EDIT_LIMIT: u64 = 1024 * 1024;
pub(crate) const TRANSFER_LIMIT: u64 = 512 * 1024 * 1024;
const GRANT_LIFETIME: Duration = Duration::from_secs(120);

#[derive(Clone)]
pub(crate) struct WorkspaceFiles {
    upload_directory: Arc<Path>,
    // Serialize our compare-and-replace writes across all authenticated peers.
    writes: Arc<Mutex<()>>,
    grants: Arc<Mutex<HashMap<String, Grant>>>,
}

struct Grant {
    session: SessionId,
    expires: Instant,
    file: GrantFile,
    size: u64,
    digest: String,
}

enum GrantFile {
    Upload {
        directory: PathBuf,
        file_name: String,
    },
    Download(File),
}

#[derive(Deserialize)]
#[serde(tag = "method", content = "params")]
pub(crate) enum FileRequest {
    #[serde(rename = "host/file/list")]
    List(PathParams),
    #[serde(rename = "host/file/read")]
    Read(PathParams),
    #[serde(rename = "host/file/write")]
    Write(Save),
    #[serde(rename = "host/blob/upload")]
    Upload(Upload),
    #[serde(rename = "host/blob/download")]
    Download(PathParams),
}
#[derive(Deserialize)]
pub(crate) struct PathParams {
    path: PathBuf,
}
#[derive(Deserialize)]
pub(crate) struct Save {
    path: PathBuf,
    revision: String,
    text: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Upload {
    directory: PathBuf,
    file_name: String,
    size: u64,
    sha256: String,
}
#[derive(Serialize)]
#[serde(untagged)]
pub(crate) enum FileResponse {
    List(FileList),
    Content(FileContent),
    Grant(TransferGrant),
}

impl WorkspaceFiles {
    pub(crate) fn new(upload_directory: PathBuf) -> Self {
        Self {
            upload_directory: upload_directory.into(),
            writes: Default::default(),
            grants: Default::default(),
        }
    }

    pub(crate) async fn request(
        &self,
        session: SessionId,
        request: FileRequest,
    ) -> Result<FileResponse, String> {
        let files = self.clone();
        tokio::task::spawn_blocking(move || files.dispatch(session, request))
            .await
            .map_err(|error| error.to_string())?
    }

    pub(crate) fn clear_session(&self, session: SessionId) {
        self.grants
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, grant| grant.session != session);
    }

    fn dispatch(&self, session: SessionId, request: FileRequest) -> Result<FileResponse, String> {
        match request {
            FileRequest::List(params) => {
                let path = absolute_path(&params.path)?
                    .canonicalize()
                    .map_err(io_error)?;
                let mut entries = Vec::new();
                let mut truncated = false;
                for entry in fs::read_dir(&path).map_err(io_error)? {
                    let entry = entry.map_err(io_error)?;
                    if entries.len() == 2000 {
                        truncated = true;
                        break;
                    }
                    let metadata = entry.metadata().map_err(io_error)?;
                    entries.push(FileEntry {
                        name: entry.file_name().to_string_lossy().into_owned(),
                        path: entry
                            .path()
                            .to_str()
                            .ok_or("file path is not UTF-8")?
                            .into(),
                        directory: metadata.is_dir(),
                        size: metadata.len(),
                        extra: Default::default(),
                    });
                }
                entries.sort_by(|a, b| b.directory.cmp(&a.directory).then(a.name.cmp(&b.name)));
                Ok(FileResponse::List(FileList {
                    path: path.to_str().ok_or("directory path is not UTF-8")?.into(),
                    entries,
                    truncated,
                    extra: Default::default(),
                }))
            }
            FileRequest::Read(params) => {
                let path = absolute_path(&params.path)?
                    .canonicalize()
                    .map_err(io_error)?;
                read_editable(&path).map(FileResponse::Content)
            }
            FileRequest::Write(params) => {
                let path = absolute_path(&params.path)?
                    .canonicalize()
                    .map_err(io_error)?;
                let _lock = self.writes.lock().unwrap_or_else(|e| e.into_inner());
                let original = read_bounded(&path, EDIT_LIMIT)?;
                if hash(&original) != params.revision {
                    return Err("revision_conflict: file changed; reload before saving".into());
                }
                let bom = original.starts_with(&[0xef, 0xbb, 0xbf]);
                let original_text =
                    std::str::from_utf8(if bom { &original[3..] } else { &original })
                        .map_err(|_| "file is not UTF-8")?;
                let text = if line_ending(original_text) == "crlf" {
                    params.text.replace("\r\n", "\n").replace('\n', "\r\n")
                } else {
                    params.text
                };
                if text.len() as u64 + if bom { 3 } else { 0 } > EDIT_LIMIT {
                    return Err("file exceeds editor size limit".into());
                }
                atomicwrites::AtomicFile::new(&path, atomicwrites::AllowOverwrite)
                    .write_with_options(
                        |output| -> Result<(), String> {
                            output
                                .set_permissions(
                                    fs::metadata(&path).map_err(io_error)?.permissions(),
                                )
                                .map_err(io_error)?;
                            if bom {
                                output.write_all(&[0xef, 0xbb, 0xbf]).map_err(io_error)?;
                            }
                            output.write_all(text.as_bytes()).map_err(io_error)?;
                            // Other editors don't share our mutex; recheck before replacement.
                            if hash(&read_bounded(&path, EDIT_LIMIT)?) != params.revision {
                                return Err("revision_conflict: file changed while saving".into());
                            }
                            Ok(())
                        },
                        crate::platform::private_file_options(),
                    )
                    .map_err(|error| error.to_string())?;
                read_editable(&path).map(FileResponse::Content)
            }
            FileRequest::Upload(params) => {
                // New chats have no workspace yet. Keep their attachments in
                // Host-owned storage; explicit destinations remain absolute.
                let directory = if params.directory.as_os_str().is_empty() {
                    let directory = absolute_path(&self.upload_directory)?;
                    crate::platform::create_state_directory(directory).map_err(io_error)?;
                    directory
                } else {
                    absolute_path(&params.directory)?
                };
                let directory = directory.canonicalize().map_err(io_error)?;
                if !directory.is_dir() {
                    return Err("upload directory is unavailable".into());
                }
                if params.file_name.is_empty()
                    || params.file_name.len() > 200
                    || Path::new(&params.file_name)
                        .file_name()
                        .and_then(|s| s.to_str())
                        != Some(params.file_name.as_str())
                    || params.file_name.chars().any(char::is_control)
                {
                    return Err("invalid attachment display name".into());
                }
                validate_digest(&params.sha256)?;
                self.grant(Grant {
                    session,
                    expires: Instant::now() + GRANT_LIFETIME,
                    file: GrantFile::Upload {
                        directory,
                        file_name: params.file_name,
                    },
                    size: params.size,
                    digest: params.sha256,
                })
                .map(FileResponse::Grant)
            }
            FileRequest::Download(params) => {
                let path = absolute_path(&params.path)?;
                let mut file = File::open(path).map_err(io_error)?;
                if !file.metadata().map_err(io_error)?.is_file() {
                    return Err("download target is not a regular file".into());
                }
                let (size, digest) = digest_file(&mut file)?;
                use std::io::{Seek, SeekFrom};
                file.seek(SeekFrom::Start(0)).map_err(io_error)?;
                self.grant(Grant {
                    session,
                    expires: Instant::now() + GRANT_LIFETIME,
                    file: GrantFile::Download(file),
                    size,
                    digest,
                })
                .map(FileResponse::Grant)
            }
        }
    }

    fn grant(&self, grant: Grant) -> Result<TransferGrant, String> {
        if grant.size > TRANSFER_LIMIT {
            return Err("transfer exceeds 512 MiB".into());
        }
        let mut grants = self.grants.lock().unwrap_or_else(|e| e.into_inner());
        grants.retain(|_, grant| grant.expires > Instant::now());
        if grants.len() >= 64
            || grants
                .values()
                .filter(|existing| existing.session == grant.session)
                .count()
                >= 8
        {
            return Err("too many pending transfers".into());
        }
        let mut random = [0; 32];
        SystemRandom::new()
            .fill(&mut random)
            .map_err(|_| "secure random generation failed")?;
        let token = URL_SAFE_NO_PAD.encode(random);
        let response = TransferGrant {
            token: token.clone(),
            size: grant.size,
            sha256: grant.digest.clone(),
        };
        grants.insert(token, grant);
        Ok(response)
    }

    pub(crate) async fn transfer<S>(&self, session: SessionId, mut stream: S) -> Result<(), String>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        tokio::time::timeout(Duration::from_secs(120), async {
            let length = stream.read_u32().await.map_err(io_error)?;
            if length != 43 {
                return Err("invalid transfer token length".into());
            }
            let mut token = [0; 43];
            stream.read_exact(&mut token).await.map_err(io_error)?;
            let token = std::str::from_utf8(&token).map_err(|_| "invalid transfer token")?;
            let grant = {
                let mut grants = self.grants.lock().unwrap_or_else(|e| e.into_inner());
                let grant = grants.get(token).ok_or("unknown or consumed transfer")?;
                if grant.session != session || grant.expires <= Instant::now() {
                    return Err("transfer is not authorized for this session".into());
                }
                grants.remove(token).unwrap()
            };
            match grant.file {
                GrantFile::Upload {
                    directory,
                    file_name,
                } => {
                    let output = tempfile::NamedTempFile::new_in(&directory).map_err(io_error)?;
                    let async_file = output.reopen().map_err(io_error)?;
                    let mut writer = tokio::fs::File::from_std(async_file);
                    let mut digest = Context::new(&SHA256);
                    let mut remaining = grant.size;
                    let mut buffer = [0; 32768];
                    while remaining > 0 {
                        let amount = remaining.min(buffer.len() as u64) as usize;
                        stream
                            .read_exact(&mut buffer[..amount])
                            .await
                            .map_err(io_error)?;
                        digest.update(&buffer[..amount]);
                        writer
                            .write_all(&buffer[..amount])
                            .await
                            .map_err(io_error)?;
                        remaining -= amount as u64;
                    }
                    // Explicit end-of-upload prevents silently accepting extra
                    // bytes or a sender declaring a truncated object size.
                    let mut end = [0];
                    if stream.read(&mut end).await.map_err(io_error)? != 0 {
                        return Err("upload exceeds declared size".into());
                    }
                    if URL_SAFE_NO_PAD.encode(digest.finish().as_ref()) != grant.digest {
                        return Err("upload digest mismatch".into());
                    }
                    writer.sync_all().await.map_err(io_error)?;
                    drop(writer);
                    let random = output
                        .path()
                        .file_name()
                        .ok_or("temporary upload path missing")?
                        .to_string_lossy();
                    let path =
                        directory.join(format!("{}-{}", random.trim_start_matches('.'), file_name));
                    output
                        .persist_noclobber(&path)
                        .map_err(|error| io_error(error.error))?;
                    let response = serde_json::to_vec(
                        &json!({"path":path,"size":grant.size,"sha256":grant.digest}),
                    )
                    .map_err(io_error)?;
                    stream
                        .write_u32(response.len() as u32)
                        .await
                        .map_err(io_error)?;
                    stream.write_all(&response).await.map_err(io_error)?;
                    stream.shutdown().await.map_err(io_error)?;
                }
                GrantFile::Download(file) => {
                    let mut file = tokio::fs::File::from_std(file);
                    let mut remaining = grant.size;
                    let mut buffer = [0; 32768];
                    while remaining > 0 {
                        let amount = remaining.min(buffer.len() as u64) as usize;
                        file.read_exact(&mut buffer[..amount])
                            .await
                            .map_err(io_error)?;
                        stream
                            .write_all(&buffer[..amount])
                            .await
                            .map_err(io_error)?;
                        remaining -= amount as u64;
                    }
                    stream.shutdown().await.map_err(io_error)?;
                }
            }
            Ok(())
        })
        .await
        .map_err(|_| "transfer timed out")?
    }
}

fn io_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}
fn absolute_path(path: &Path) -> Result<&Path, String> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Err("an absolute filesystem path is required".into())
    }
}
fn validate_digest(value: &str) -> Result<(), String> {
    let mut bytes = [0; 32];
    if value.len() == 43
        && URL_SAFE_NO_PAD
            .decode_slice(value, &mut bytes)
            .is_ok_and(|n| n == 32)
    {
        Ok(())
    } else {
        Err("invalid SHA-256 digest".into())
    }
}
fn hash(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(digest::digest(&SHA256, bytes).as_ref())
}
fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(io_error)?;
    if !file.metadata().map_err(io_error)?.is_file() {
        return Err("path is not a regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() as u64 > limit {
        return Err("file exceeds editor size limit; download it instead".into());
    }
    Ok(bytes)
}
fn read_editable(path: &Path) -> Result<FileContent, String> {
    let bytes = read_bounded(path, EDIT_LIMIT)?;
    let bom = bytes.starts_with(&[0xef, 0xbb, 0xbf]);
    let text = std::str::from_utf8(if bom { &bytes[3..] } else { &bytes })
        .map_err(|_| "file is not UTF-8; download it instead")?;
    if text.contains('\0') {
        return Err("binary file; download it instead".into());
    }
    Ok(FileContent {
        path: path.to_str().ok_or("file path is not UTF-8")?.into(),
        revision: hash(&bytes),
        text: text.into(),
        bom,
        line_ending: line_ending(text).into(),
        size: bytes.len() as u64,
        extra: Default::default(),
    })
}
fn line_ending(text: &str) -> &'static str {
    let crlf = text
        .as_bytes()
        .windows(2)
        .filter(|bytes| *bytes == b"\r\n")
        .count();
    let lf = text
        .as_bytes()
        .iter()
        .filter(|&&byte| byte == b'\n')
        .count();
    if crlf > 0 && crlf == lf {
        "crlf"
    } else if crlf > 0 {
        "mixed"
    } else {
        "lf"
    }
}
fn digest_file(file: &mut File) -> Result<(u64, String), String> {
    let mut digest = Context::new(&SHA256);
    let mut buffer = [0; 32768];
    let mut size = 0;
    loop {
        let read = file.read(&mut buffer).map_err(io_error)?;
        if read == 0 {
            break;
        }
        size += read as u64;
        if size > TRANSFER_LIMIT {
            return Err("transfer exceeds 512 MiB".into());
        }
        digest.update(&buffer[..read]);
    }
    Ok((size, URL_SAFE_NO_PAD.encode(digest.finish().as_ref())))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn send_token(stream: &mut tokio::io::DuplexStream, grant: &FileResponse) {
        let FileResponse::Grant(grant) = grant else {
            panic!("expected transfer grant")
        };
        let token = &grant.token;
        stream.write_u32(token.len() as u32).await.unwrap();
        stream.write_all(token.as_bytes()).await.unwrap();
    }

    #[tokio::test]
    async fn grants_are_single_use_expiring_and_bound_to_the_issuing_session() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("content");
            fs::write(&path, b"private").unwrap();
            let files = WorkspaceFiles::new(dir.path().join("attachments"));
            let grant = files
                .dispatch(1, FileRequest::Download(PathParams { path: path.clone() }))
                .unwrap();
            let (mut client, server) = tokio::io::duplex(1024);
            send_token(&mut client, &grant).await;
            assert!(
                files
                    .transfer(2, server)
                    .await
                    .unwrap_err()
                    .contains("not authorized")
            );
            let (mut client, server) = tokio::io::duplex(1024);
            send_token(&mut client, &grant).await;
            files.transfer(1, server).await.unwrap();
            let mut content = Vec::new();
            client.read_to_end(&mut content).await.unwrap();
            assert_eq!(content, b"private");
            let (mut client, server) = tokio::io::duplex(1024);
            send_token(&mut client, &grant).await;
            assert!(
                files
                    .transfer(1, server)
                    .await
                    .unwrap_err()
                    .contains("consumed")
            );
            let grant = files
                .dispatch(1, FileRequest::Download(PathParams { path: path.clone() }))
                .unwrap();
            files
                .grants
                .lock()
                .unwrap()
                .get_mut(match &grant {
                    FileResponse::Grant(grant) => &grant.token,
                    _ => panic!("expected transfer grant"),
                })
                .unwrap()
                .expires = Instant::now() - Duration::from_secs(1);
            let (mut client, server) = tokio::io::duplex(1024);
            send_token(&mut client, &grant).await;
            assert!(files.transfer(1, server).await.is_err());
            files.clear_session(1);
            assert!(files.grants.lock().unwrap().is_empty());
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn corrupt_or_overlong_uploads_leave_no_destination_or_temporary_file() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let directory = tempfile::tempdir().unwrap();
            let files = WorkspaceFiles::new(directory.path().join("attachments"));
            for bytes in [&b"wrong"[..], &b"longer"[..]] {
                let grant = files
                    .dispatch(
                        1,
                        FileRequest::Upload(Upload {
                            directory: directory.path().into(),
                            file_name: "safe.txt".into(),
                            size: 5,
                            sha256: hash(b"valid"),
                        }),
                    )
                    .unwrap();
                let (mut client, server) = tokio::io::duplex(1024);
                send_token(&mut client, &grant).await;
                client.write_all(bytes).await.unwrap();
                client.shutdown().await.unwrap();
                assert!(files.transfer(1, server).await.is_err());
                assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
            }
            assert!(
                files
                    .dispatch(
                        1,
                        FileRequest::Upload(Upload {
                            directory: directory.path().into(),
                            file_name: "../escape".into(),
                            size: 0,
                            sha256: hash(b"")
                        })
                    )
                    .is_err()
            );
            for path in ["relative", ".", ".."] {
                assert!(matches!(
                    files.dispatch(1, FileRequest::Upload(Upload {
                        directory: path.into(),
                        file_name: "safe.txt".into(),
                        size: 0,
                        sha256: hash(b""),
                    })),
                    Err(error) if error == "an absolute filesystem path is required"
                ));
            }
            assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
        })
        .await
        .unwrap();
    }
}

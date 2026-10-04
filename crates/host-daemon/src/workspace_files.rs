use agent_protocol::protocol::{Body, Call};
use anyhow::{Context as _, Result, anyhow};
use ring::digest;
use std::fs;
use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use agent_protocol::models::{FileContent, FileEntry, FileList, TransferGrant};
#[cfg(test)]
use agent_protocol::operations::ListFiles as PathParams;
#[cfg(test)]
use agent_protocol::operations::Upload;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::{
    digest::{Context, SHA256},
    rand::{SecureRandom, SystemRandom},
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::SessionId;

const EDIT_LIMIT: u64 = 1024 * 1024;
pub(crate) const TRANSFER_LIMIT: u64 = 512 * 1024 * 1024;
const GRANT_LIFETIME: Duration = Duration::from_secs(120);

#[cfg(test)]
#[path = "workspace_files/item_read_tests.rs"]
mod item_read_tests;

#[derive(Clone)]
pub(crate) struct WorkspaceFiles {
    upload_directory: Arc<Path>,
    // Serialize our compare-and-replace writes across all authenticated peers.
    writes: Arc<Mutex<()>>,
    grants: Arc<Mutex<HashMap<[u8; 32], Grant>>>,
}

struct Grant {
    session: SessionId,
    expires: Instant,
    file: GrantFile,
    size: u64,
    digest: [u8; 32],
}

enum GrantFile {
    Upload {
        directory: PathBuf,
        file_name: String,
    },
    Download(File),
}

impl WorkspaceFiles {
    pub(crate) fn new(upload_directory: PathBuf) -> Self {
        Self {
            upload_directory: upload_directory.into(),
            writes: Default::default(),
            grants: Default::default(),
        }
    }

    pub(crate) async fn request(&self, session: SessionId, request: Call) -> Result<Body> {
        let files = self.clone();
        tokio::task::spawn_blocking(move || files.dispatch(session, request)).await?
    }

    pub(crate) fn clear_session(&self, session: SessionId) {
        self.grants
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, grant| grant.session != session);
    }

    /// An anonymous transfer file is closed on consumption, expiry or disconnect.
    /// It is never added to native history or the attachment directory.
    pub(crate) async fn download_bytes(
        &self,
        session: SessionId,
        bytes: Vec<u8>,
    ) -> Result<TransferGrant> {
        let files = self.clone();
        tokio::task::spawn_blocking(move || {
            use std::io::{Seek, SeekFrom};
            if bytes.len() as u64 > TRANSFER_LIMIT {
                return Err(anyhow!("transfer exceeds 512 MiB"));
            }
            let mut file = tempfile::tempfile()?;
            file.write_all(&bytes)?;
            file.seek(SeekFrom::Start(0))?;
            files.grant(Grant {
                session,
                expires: Instant::now() + GRANT_LIFETIME,
                file: GrantFile::Download(file),
                size: bytes.len() as u64,
                digest: digest::digest(&SHA256, &bytes)
                    .as_ref()
                    .try_into()
                    .expect("SHA-256 length"),
            })
        })
        .await?
    }

    fn dispatch(&self, session: SessionId, request: Call) -> Result<Body> {
        match request {
            Call::ReadVisualization(params) => {
                use crate::visualize::{visualization_document, visualization_path};
                let path =
                    visualization_path(&params.path, &params.cwd).map_err(anyhow::Error::msg)?;
                let directory = self.upload_directory.join("visualizations");
                let archive = directory.join(hash(path.to_string_lossy().as_bytes()));
                let _lock = self.writes.lock().unwrap_or_else(|e| e.into_inner());
                let bytes = if path.try_exists()? {
                    let bytes = read_bounded(&path, EDIT_LIMIT)?;
                    std::str::from_utf8(&bytes).context("visualize HTML is not UTF-8")?;
                    fs::create_dir_all(&directory)?;
                    atomicwrites::AtomicFile::new(&archive, atomicwrites::AllowOverwrite)
                        .write_with_options(
                            |file| file.write_all(&bytes),
                            crate::platform::private_file_options(),
                        )?;
                    bytes
                } else {
                    read_bounded(&archive, EDIT_LIMIT)
                        .context("表示ファイルが見つかりません。HostでHTMLを再作成してください。")?
                };
                let fragment =
                    std::str::from_utf8(&bytes).context("visualize HTML is not UTF-8")?;
                Ok(Body::from(visualization_document(fragment)))
            }
            Call::ListFiles(params) => {
                let path = dunce::canonicalize(absolute_path(&params.path)?)?;
                let mut entries = Vec::new();
                let mut truncated = false;
                for entry in fs::read_dir(&path)? {
                    let entry = entry?;
                    if entries.len() == 2000 {
                        truncated = true;
                        break;
                    }
                    let metadata = entry.metadata()?;
                    entries.push(FileEntry {
                        name: entry.file_name().to_string_lossy().into_owned(),
                        path: entry
                            .path()
                            .to_str()
                            .context("file path is not UTF-8")?
                            .into(),
                        directory: metadata.is_dir(),
                        size: metadata.len(),
                    });
                }
                entries.sort_by(|a, b| b.directory.cmp(&a.directory).then(a.name.cmp(&b.name)));
                Ok(Body::from(FileList {
                    path: path.to_str().context("directory path is not UTF-8")?.into(),
                    entries,
                    truncated,
                }))
            }
            Call::ReadFile(params) => {
                let path = dunce::canonicalize(absolute_path(&params.path)?)?;
                read_editable(&path).map(Body::from)
            }
            Call::WriteFile(params) => {
                let path = dunce::canonicalize(absolute_path(&params.path)?)?;
                let _lock = self.writes.lock().unwrap_or_else(|e| e.into_inner());
                let original =
                    read_bounded(&path, EDIT_LIMIT).context("read file before saving")?;
                if hash(&original) != params.revision {
                    return Err(anyhow!(
                        "revision_conflict: file changed; reload before saving"
                    ));
                }
                let bom = original.starts_with(&[0xef, 0xbb, 0xbf]);
                let original_text =
                    std::str::from_utf8(if bom { &original[3..] } else { &original })
                        .context("file is not UTF-8")?;
                let text = if line_ending(original_text) == "crlf" {
                    params.text.replace("\r\n", "\n").replace('\n', "\r\n")
                } else {
                    params.text
                };
                if text.len() as u64 + if bom { 3 } else { 0 } > EDIT_LIMIT {
                    return Err(anyhow!("file exceeds editor size limit"));
                }
                atomicwrites::AtomicFile::new(&path, atomicwrites::AllowOverwrite)
                    .write_with_options(
                        |output| -> Result<()> {
                            output
                                .set_permissions(
                                    fs::metadata(&path)
                                        .context("read file permissions before saving")?
                                        .permissions(),
                                )
                                .context("apply permissions to replacement file")?;
                            if bom {
                                output.write_all(&[0xef, 0xbb, 0xbf])?;
                            }
                            output
                                .write_all(text.as_bytes())
                                .context("write replacement file")?;
                            // Other editors don't share our mutex; recheck before replacement.
                            if hash(&read_bounded(&path, EDIT_LIMIT)?) != params.revision {
                                return Err(anyhow!(
                                    "revision_conflict: file changed while saving"
                                ));
                            }
                            Ok(())
                        },
                        crate::platform::private_file_options(),
                    )
                    .map_err(|error| match error {
                        atomicwrites::Error::Internal(error) => {
                            anyhow::Error::new(error).context("prepare or commit file replacement")
                        }
                        atomicwrites::Error::User(error) => error,
                    })?;
                read_editable(&path).map(Body::from)
            }
            Call::Upload(params) => {
                // New chats have no workspace yet. Keep their attachments in
                // Host-owned storage; explicit destinations remain absolute.
                let directory = if params.directory.is_empty() {
                    let directory = absolute_path(&self.upload_directory)?;
                    crate::platform::create_state_directory(directory)?;
                    directory
                } else {
                    absolute_path(&params.directory)?
                };
                let directory = dunce::canonicalize(directory)?;
                if !directory.is_dir() {
                    return Err(anyhow!("upload directory is unavailable"));
                }
                if params.file_name.is_empty()
                    || params.file_name.len() > 200
                    || Path::new(&params.file_name)
                        .file_name()
                        .and_then(|s| s.to_str())
                        != Some(params.file_name.as_str())
                    || params.file_name.chars().any(char::is_control)
                {
                    return Err(anyhow!("invalid attachment display name"));
                }
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
                .map(Body::from)
            }
            Call::Download(params) => {
                let path = absolute_path(&params.path)?;
                let mut file = File::open(path)?;
                if !file.metadata()?.is_file() {
                    return Err(anyhow!("download target is not a regular file"));
                }
                let (size, digest) = digest_file(&mut file)?;
                use std::io::{Seek, SeekFrom};
                file.seek(SeekFrom::Start(0))?;
                self.grant(Grant {
                    session,
                    expires: Instant::now() + GRANT_LIFETIME,
                    file: GrantFile::Download(file),
                    size,
                    digest,
                })
                .map(Body::from)
            }
            _ => Err(anyhow::anyhow!("not a file request")),
        }
    }

    fn grant(&self, grant: Grant) -> Result<TransferGrant> {
        if grant.size > TRANSFER_LIMIT {
            return Err(anyhow!("transfer exceeds 512 MiB"));
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
            return Err(anyhow!("too many pending transfers"));
        }
        let mut random = [0; 32];
        SystemRandom::new()
            .fill(&mut random)
            .map_err(|_| anyhow!("secure random generation failed"))?;
        let token = random;
        let response = TransferGrant {
            token,
            size: grant.size,
            sha256: grant.digest,
        };
        grants.insert(token, grant);
        Ok(response)
    }

    pub(crate) async fn transfer<S>(&self, session: SessionId, mut stream: S) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        tokio::time::timeout(Duration::from_secs(120), async {
            let mut token = [0; 32];
            stream.read_exact(&mut token).await?;
            let grant = {
                let mut grants = self.grants.lock().unwrap_or_else(|e| e.into_inner());
                let grant = grants.get(&token).context("unknown or consumed transfer")?;
                if grant.session != session || grant.expires <= Instant::now() {
                    return Err(anyhow!("transfer is not authorized for this session"));
                }
                grants.remove(&token).unwrap()
            };
            match grant.file {
                GrantFile::Upload {
                    directory,
                    file_name,
                } => {
                    let output = tempfile::NamedTempFile::new_in(&directory)?;
                    let async_file = output.reopen()?;
                    let mut writer = tokio::fs::File::from_std(async_file);
                    let mut digest = Context::new(&SHA256);
                    let mut remaining = grant.size;
                    let mut buffer = [0; 32768];
                    while remaining > 0 {
                        let amount = remaining.min(buffer.len() as u64) as usize;
                        stream.read_exact(&mut buffer[..amount]).await?;
                        digest.update(&buffer[..amount]);
                        writer.write_all(&buffer[..amount]).await?;
                        remaining -= amount as u64;
                    }
                    // Explicit end-of-upload prevents silently accepting extra
                    // bytes or a sender declaring a truncated object size.
                    let mut end = [0];
                    if stream.read(&mut end).await? != 0 {
                        return Err(anyhow!("upload exceeds declared size"));
                    }
                    if digest.finish().as_ref() != grant.digest {
                        return Err(anyhow!("upload digest mismatch"));
                    }
                    writer.sync_all().await?;
                    drop(writer);
                    let random = output
                        .path()
                        .file_name()
                        .context("temporary upload path missing")?
                        .to_string_lossy();
                    let path =
                        directory.join(format!("{}-{}", random.trim_start_matches('.'), file_name));
                    output.persist_noclobber(&path)?;
                    let response =
                        agent_protocol::protocol::encode(agent_protocol::models::UploadedFile {
                            path: path.to_str().context("upload path is not UTF-8")?.into(),
                            size: grant.size,
                            sha256: grant.digest,
                        })?;
                    stream.write_u32(response.len() as u32).await?;
                    stream.write_all(&response).await?;
                    stream.shutdown().await?;
                }
                GrantFile::Download(file) => {
                    let mut file = tokio::fs::File::from_std(file);
                    let mut remaining = grant.size;
                    let mut buffer = [0; 32768];
                    while remaining > 0 {
                        let amount = remaining.min(buffer.len() as u64) as usize;
                        file.read_exact(&mut buffer[..amount]).await?;
                        stream.write_all(&buffer[..amount]).await?;
                        remaining -= amount as u64;
                    }
                    stream.shutdown().await?;
                }
            }
            Ok(())
        })
        .await
        .context("transfer timed out")?
    }
}

fn absolute_path(path: &impl AsRef<Path>) -> Result<&Path> {
    let path = path.as_ref();
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(anyhow!("an absolute filesystem path is required"))
    }
}
fn hash(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(digest::digest(&SHA256, bytes).as_ref())
}
fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(anyhow!("path is not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(anyhow!(
            "file exceeds editor size limit; download it instead"
        ));
    }
    Ok(bytes)
}
fn read_editable(path: &Path) -> Result<FileContent> {
    let bytes = read_bounded(path, EDIT_LIMIT)?;
    let bom = bytes.starts_with(&[0xef, 0xbb, 0xbf]);
    let text = std::str::from_utf8(if bom { &bytes[3..] } else { &bytes })
        .context("file is not UTF-8; download it instead")?;
    if text.contains('\0') {
        return Err(anyhow!("binary file; download it instead"));
    }
    Ok(FileContent {
        path: path.to_str().context("file path is not UTF-8")?.into(),
        revision: hash(&bytes),
        text: text.into(),
        size: bytes.len() as u64,
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
fn digest_file(file: &mut File) -> Result<(u64, [u8; 32])> {
    let mut digest = Context::new(&SHA256);
    let mut buffer = [0; 32768];
    let mut size = 0;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        size += read as u64;
        if size > TRANSFER_LIMIT {
            return Err(anyhow!("transfer exceeds 512 MiB"));
        }
        digest.update(&buffer[..read]);
    }
    Ok((
        size,
        digest.finish().as_ref().try_into().expect("SHA-256 length"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn send_token(stream: &mut tokio::io::DuplexStream, grant: &Body) {
        let Body::Grant(grant) = grant else {
            panic!("expected transfer grant")
        };
        let token = &grant.token;
        stream.write_all(token).await.unwrap();
    }

    #[test]
    fn visualization_archive_survives_file_service_recreation_and_rejects_invalid_sources() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("comparison.html");
        fs::write(
            &source,
            include_str!("../../agent-core/tests/fixtures/visualize/icon-options.html"),
        )
        .unwrap();
        let request = || {
            Call::ReadVisualization(agent_protocol::operations::LoadVisualization {
                path: "comparison.html".into(),
                cwd: directory.path().to_str().unwrap().into(),
            })
        };
        let files = WorkspaceFiles::new(directory.path().join("attachments"));
        let Body::Text(original) = files.dispatch(1, request()).unwrap() else {
            panic!("expected HTML")
        };
        fs::remove_file(&source).unwrap();
        drop(files);
        let files = WorkspaceFiles::new(directory.path().join("attachments"));
        let Body::Text(reopened) = files.dispatch(2, request()).unwrap() else {
            panic!("expected archive")
        };
        assert_eq!(original, reopened);
        for bytes in [vec![b'x'; EDIT_LIMIT as usize + 1], vec![0xff]] {
            fs::write(&source, bytes).unwrap();
            assert!(
                files.dispatch(2, request()).is_err(),
                "invalid content must not silently reuse an older archive"
            );
        }
        fs::remove_file(&source).unwrap();
        fs::create_dir(&source).unwrap();
        assert!(files.dispatch(2, request()).is_err());
    }

    #[tokio::test]
    async fn grants_are_single_use_expiring_and_bound_to_the_issuing_session() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("content");
            fs::write(&path, b"private").unwrap();
            let files = WorkspaceFiles::new(dir.path().join("attachments"));
            let grant = files
                .dispatch(
                    1,
                    Call::Download(PathParams {
                        path: path.to_str().unwrap().into(),
                    }),
                )
                .unwrap();
            let (mut client, server) = tokio::io::duplex(1024);
            send_token(&mut client, &grant).await;
            assert!(
                files
                    .transfer(2, server)
                    .await
                    .unwrap_err()
                    .to_string()
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
                    .to_string()
                    .contains("consumed")
            );
            let grant = files
                .dispatch(
                    1,
                    Call::Download(PathParams {
                        path: path.to_str().unwrap().into(),
                    }),
                )
                .unwrap();
            files
                .grants
                .lock()
                .unwrap()
                .get_mut(match &grant {
                    Body::Grant(grant) => &grant.token,
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
                        Call::Upload(Upload {
                            directory: directory.path().to_str().unwrap().into(),
                            file_name: "safe.txt".into(),
                            size: 5,
                            sha256: digest::digest(&SHA256, b"valid")
                                .as_ref()
                                .try_into()
                                .unwrap(),
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
                        Call::Upload(Upload {
                            directory: directory.path().to_str().unwrap().into(),
                            file_name: "../escape".into(),
                            size: 0,
                            sha256: digest::digest(&SHA256, b"").as_ref().try_into().unwrap()
                        })
                    )
                    .is_err()
            );
            for path in ["relative", ".", ".."] {
                assert!(matches!(
                    files.dispatch(1, Call::Upload(Upload {
                        directory: path.into(),
                        file_name: "safe.txt".into(),
                        size: 0,
                        sha256: digest::digest(&SHA256, b"").as_ref().try_into().unwrap(),
                    })),
                    Err(error) if error.to_string() == "an absolute filesystem path is required"
                ));
            }
            assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
        })
        .await
        .unwrap();
    }
}

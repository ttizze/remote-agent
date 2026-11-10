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
mod metadata;
mod tree;

const EDIT_LIMIT: u64 = 1024 * 1024;
pub(crate) const TRANSFER_LIMIT: u64 = 512 * 1024 * 1024;
const GRANT_LIFETIME: Duration = Duration::from_secs(120);

#[cfg(test)]
#[path = "workspace_files/read_tests.rs"]
mod read_tests;
#[cfg(test)]
#[path = "workspace_files/transfer_tests.rs"]
mod transfer_tests;

#[derive(Clone)]
pub(crate) struct WorkspaceFiles {
    search_indexes: Arc<tree::SearchIndexes>,
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
            search_indexes: Arc::default(),
            upload_directory: upload_directory.into(),
            writes: Default::default(),
            grants: Default::default(),
        }
    }

    pub(crate) async fn request(
        &self,
        session: SessionId,
        request: Call,
    ) -> Result<Body, crate::providers::Failure> {
        if let Call::ReadFileMetadata(params) = request {
            return metadata::read(params)
                .await
                .map(Body::from)
                .map_err(file_failure);
        }
        if let Call::ListTreeEntries(params) = request {
            return tree::list(params)
                .await
                .map(Body::from)
                .map_err(file_failure);
        }
        if let Call::SearchTreeEntries(params) = request {
            return self
                .search_indexes
                .search(params)
                .await
                .map(Body::from)
                .map_err(file_failure);
        }
        let files = self.clone();
        tokio::task::spawn_blocking(move || files.dispatch(session, request))
            .await
            .map_err(|error| crate::providers::Failure::new("file_operation_failed", error))?
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
                return Err(anyhow!("512 MiBを超えるファイルは転送できません。"));
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

    fn dispatch(
        &self,
        session: SessionId,
        request: Call,
    ) -> Result<Body, crate::providers::Failure> {
        match request {
            Call::ReadVisualization(params) => self
                .read_visualization(&params.path, &params.cwd)
                .map_err(|error| crate::providers::Failure::new("file_operation_failed", error)),
            request => self.file_request(session, request).map_err(file_failure),
        }
    }

    fn read_visualization(&self, source: &str, cwd: &str) -> Result<Body> {
        use crate::visualize::{visualization_document, visualization_path};
        let path = visualization_path(source, cwd).map_err(anyhow::Error::msg)?;
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
                    agent_local_host::private_file_options(),
                )?;
            bytes
        } else {
            read_bounded(&archive, EDIT_LIMIT)
                .context("表示ファイルが見つかりません。ホストでHTMLを再作成してください。")?
        };
        let fragment = std::str::from_utf8(&bytes).context("visualize HTML is not UTF-8")?;
        Ok(Body::from(visualization_document(fragment)))
    }

    fn file_request(&self, session: SessionId, request: Call) -> Result<Body> {
        match request {
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
                    let metadata = if metadata.file_type().is_symlink() {
                        fs::metadata(entry.path()).unwrap_or(metadata)
                    } else {
                        metadata
                    };
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
            Call::ResolveFile(params) => {
                // Preserve the link's filename and extension, including symlinks;
                // clients use them to select their existing file presentation.
                let path = absolute_path(&params.path)?;
                let metadata = fs::metadata(&path)?;
                Ok(Body::from(FileEntry {
                    name: path
                        .file_name()
                        .unwrap_or(path.as_os_str())
                        .to_string_lossy()
                        .into_owned(),
                    path: path.to_str().context("file path is not UTF-8")?.into(),
                    directory: metadata.is_dir(),
                    size: metadata.len(),
                }))
            }
            Call::ReadFile(params) => {
                let requested = absolute_path(&params.path)?;
                let path = dunce::canonicalize(&requested)?;
                read_file(&path, &requested).map(Body::from)
            }
            Call::WriteFile(params) => {
                validate_text(&params.text)?;
                let requested = absolute_path(&params.path)?;
                let path = dunce::canonicalize(&requested)?;
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
                        .context("UTF-8以外のファイルは編集できません。")?;
                validate_text(original_text)?;
                let text = if line_ending(original_text) == "crlf" {
                    params.text.replace("\r\n", "\n").replace('\n', "\r\n")
                } else {
                    params.text
                };
                if text.len() as u64 + if bom { 3 } else { 0 } > EDIT_LIMIT {
                    return Err(anyhow!("ファイルが大きすぎるため編集できません。"));
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
                        agent_local_host::private_file_options(),
                    )
                    .map_err(|error| match error {
                        atomicwrites::Error::Internal(error) => {
                            anyhow::Error::new(error).context("prepare or commit file replacement")
                        }
                        atomicwrites::Error::User(error) => error,
                    })?;
                read_file(&path, &requested).map(Body::from)
            }
            Call::Upload(params) => {
                // Attachments outlive worktrees. Linked workspace files still
                // read their original path and are never archived implicitly.
                let directory = absolute_path(&self.upload_directory)?;
                agent_local_host::create_state_directory(&directory)?;
                let directory = dunce::canonicalize(directory)?;
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
                let (mut file, _) = open_regular_file(&path)?;
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
            Call::WriteWorkspaceFile(params) => {
                validate_text(&params.text)?;
                let _lock = self.writes.lock().unwrap_or_else(|e| e.into_inner());
                let target = workspace_target(&params.cwd, &params.relative_path)?;
                let temporary = format!(".bex-plan-{}.tmp", uuid::Uuid::new_v4());
                let mut options = cap_std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use cap_std::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let mut file = target.directory.open_with(&temporary, &options)?;
                let result = (|| {
                    file.write_all(params.text.as_bytes())?;
                    file.sync_all()?;
                    // Windows cannot rename an open file.
                    drop(file);
                    target
                        .directory
                        .rename(&temporary, &target.directory, &target.name)
                })();
                if result.is_err() {
                    let _ = target.directory.remove_file(&temporary);
                }
                result.context("write the file")?;
                Ok(Body::from(target.relative))
            }
            _ => Err(anyhow::anyhow!("not a file request")),
        }
    }

    fn grant(&self, grant: Grant) -> Result<TransferGrant> {
        if grant.size > TRANSFER_LIMIT {
            return Err(anyhow!("512 MiBを超えるファイルは転送できません。"));
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

pub(crate) fn absolute_path(path: &impl AsRef<Path>) -> Result<PathBuf> {
    let path = path.as_ref();
    if let Ok(relative) = path.strip_prefix("~") {
        let home = directories::BaseDirs::new().context("Host home directory is unavailable")?;
        // Rebuild from components so Windows Hosts do not keep the link's `/` separators.
        let mut resolved = home.home_dir().to_owned();
        resolved.extend(relative.components());
        return Ok(resolved);
    }
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        Err(anyhow!("an absolute filesystem path is required"))
    }
}

fn file_failure(error: anyhow::Error) -> crate::providers::Failure {
    let code = match error
        .downcast_ref::<std::io::Error>()
        .map(std::io::Error::kind)
    {
        Some(std::io::ErrorKind::NotFound) => "file_not_found",
        Some(std::io::ErrorKind::PermissionDenied) => "file_access_denied",
        _ => "file_operation_failed",
    };
    crate::providers::Failure::new(code, error)
}
fn hash(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(digest::digest(&SHA256, bytes).as_ref())
}
fn open_regular_file(path: &Path) -> Result<(File, u64)> {
    let mut options = File::options();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A FIFO must not block before its type can be checked.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(anyhow!("通常のファイルではありません。"));
    }
    Ok((file, metadata.len()))
}
fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let (file, _) = open_regular_file(path)?;
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(anyhow!(
            "ファイルが大きすぎるため表示できません。ダウンロードして開いてください。"
        ));
    }
    Ok(bytes)
}
fn read_file(path: &Path, requested: &Path) -> Result<FileContent> {
    let (file, file_size) = open_regular_file(path)?;
    let mut bytes = Vec::new();
    file.take(EDIT_LIMIT + 1).read_to_end(&mut bytes)?;
    let truncated = bytes.len() as u64 > EDIT_LIMIT;
    let size = if truncated {
        file_size.max(bytes.len() as u64)
    } else {
        bytes.len() as u64
    };
    bytes.truncate(EDIT_LIMIT as usize);
    let bom = bytes.starts_with(&[0xef, 0xbb, 0xbf]);
    let source = if bom { &bytes[3..] } else { &bytes };
    let text = match std::str::from_utf8(source) {
        Ok(text) => text,
        Err(error) if truncated && error.error_len().is_none() => {
            std::str::from_utf8(&source[..error.valid_up_to()])?
        }
        Err(error) => {
            return Err(anyhow!(error)
                .context("UTF-8以外のファイルは表示できません。ダウンロードして開いてください。"));
        }
    };
    validate_text(text)?;
    Ok(FileContent {
        // The editor/draft belongs to the clicked path, including symlinks and
        // lexical parent segments. Canonical paths only identify the I/O target.
        path: requested.to_str().context("file path is not UTF-8")?.into(),
        revision: hash(&bytes),
        text: text.into(),
        size,
        truncated,
    })
}
/// The opened parent directory and leaf name of a workspace-relative file.
struct WorkspaceTarget {
    directory: cap_std::fs::Dir,
    name: std::ffi::OsString,
    relative: String,
}

/// Resolve relative paths with directory capabilities so intermediate links
/// cannot redirect a workspace write outside its root.
fn workspace_target(cwd: &str, relative: &str) -> Result<WorkspaceTarget> {
    use std::path::Component;
    let root =
        cap_std::fs::Dir::open_ambient_dir(absolute_path(&cwd)?, cap_std::ambient_authority())
            .context("open the workspace")?;
    let mut parts = Vec::new();
    for component in Path::new(relative.trim()).components() {
        match component {
            Component::Normal(part) => parts.push(part.to_owned()),
            Component::CurDir => {}
            Component::ParentDir if parts.pop().is_some() => {}
            _ => return Err(anyhow!("the path must stay inside the workspace")),
        }
    }
    if parts.is_empty() {
        return Err(anyhow!("a file path is required"));
    }
    let path: PathBuf = parts.iter().collect();
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // Directory capabilities resolve links beneath this root and retain the
    // opened directory across the atomic replacement, including concurrent
    // path changes. A path check followed by ordinary I/O would race symlinks.
    root.create_dir_all(parent)
        .context("make the file's folders inside the workspace")?;
    let directory = root.open_dir(parent)?;
    let name = path
        .file_name()
        .context("a file path is required")?
        .to_owned();
    if directory
        .symlink_metadata(&name)
        .is_ok_and(|metadata| metadata.is_symlink())
    {
        return Err(anyhow!("the file must not be a symbolic link"));
    }
    Ok(WorkspaceTarget {
        directory,
        name,
        relative: parts
            .iter()
            .map(|part| part.to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
    })
}

fn validate_text(text: &str) -> Result<()> {
    if text.contains('\0') {
        return Err(anyhow!("binary file; download it instead"));
    }
    Ok(())
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
            return Err(anyhow!("512 MiBを超えるファイルは転送できません。"));
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

    #[test]
    fn a_workspace_file_is_written_inside_the_workspace_with_its_folders() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let files = WorkspaceFiles::new(workspace.path().join("attachments"));
        let cwd = workspace.path().to_str().unwrap().to_owned();
        let write = |relative: &str, text: &str| {
            files.dispatch(
                1,
                Call::WriteWorkspaceFile(agent_protocol::operations::WriteWorkspaceFile {
                    cwd: cwd.clone(),
                    relative_path: relative.into(),
                    text: text.into(),
                }),
            )
        };
        let Body::Text(written) = write(" plans/./ship-it.md ", "# Ship it\n").unwrap() else {
            panic!("a path")
        };
        assert_eq!(*written, "plans/ship-it.md");
        assert_eq!(
            fs::read_to_string(workspace.path().join("plans/ship-it.md")).unwrap(),
            "# Ship it\n"
        );
        // A plan saved again replaces the file.
        write("plans/ship-it.md", "# Shipped\n").unwrap();
        assert_eq!(
            fs::read_to_string(workspace.path().join("plans/ship-it.md")).unwrap(),
            "# Shipped\n"
        );
        write("root.md", "Root\n").unwrap();
        assert_eq!(
            fs::read_to_string(workspace.path().join("root.md")).unwrap(),
            "Root\n"
        );
        assert!(write("root.md", "binary\0text").is_err());
        assert_eq!(
            fs::read_to_string(workspace.path().join("root.md")).unwrap(),
            "Root\n"
        );
        assert!(write("plans", "cannot replace a directory").is_err());
        write("root.md", "Recovered\n").unwrap();
        for escaping in [
            "../escape.md",
            "/tmp/escape.md",
            "plans/../../escape.md",
            "",
        ] {
            assert!(write(escaping, "x").is_err(), "{escaping}");
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path(), workspace.path().join("link")).unwrap();
            assert!(write("link/escape.md", "x").is_err());
            assert!(!outside.path().join("escape.md").exists());
            std::os::unix::fs::symlink(
                outside.path().join("missing"),
                workspace.path().join("dangling"),
            )
            .unwrap();
            assert!(write("dangling/escape.md", "x").is_err());
            assert!(!outside.path().join("missing").exists());
            // Relative links within the root remain usable.
            std::os::unix::fs::symlink("plans", workspace.path().join("relative-link")).unwrap();
            write("relative-link/inside.md", "inside\n").unwrap();
            assert_eq!(
                fs::read_to_string(workspace.path().join("plans/inside.md")).unwrap(),
                "inside\n"
            );
            std::os::unix::fs::symlink(
                outside.path().join("leaf.md"),
                workspace.path().join("leaf-link.md"),
            )
            .unwrap();
            assert!(write("leaf-link.md", "x").is_err());
            assert!(!outside.path().join("leaf.md").exists());
        }
    }

    #[tokio::test]
    async fn home_links_resolve_list_read_write_and_download_on_the_host() {
        let home = directories::BaseDirs::new().unwrap();
        let directory = tempfile::tempdir_in(home.home_dir()).unwrap();
        let prefix = format!(
            "~/{}",
            directory.path().file_name().unwrap().to_str().unwrap()
        );
        let files = WorkspaceFiles::new(directory.path().join("attachments"));
        fs::create_dir(directory.path().join("Movies")).unwrap();
        fs::write(directory.path().join("Movies/video.mp4"), b"video").unwrap();
        fs::write(directory.path().join("notes.md"), "original").unwrap();
        let Body::FileEntry(folder) = files
            .dispatch(
                1,
                Call::ResolveFile(agent_protocol::operations::ResolveFile {
                    path: format!("{prefix}/Movies"),
                }),
            )
            .unwrap()
        else {
            panic!()
        };
        assert!(folder.directory);
        assert_eq!(
            folder.path,
            directory.path().join("Movies").to_str().unwrap()
        );
        let Body::Files(listing) = files
            .dispatch(
                1,
                Call::ListFiles(PathParams {
                    path: format!("{prefix}/Movies/"),
                }),
            )
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(
            Path::new(&listing.path),
            dunce::canonicalize(&folder.path).unwrap()
        );
        assert_eq!(listing.entries.len(), 1);
        assert_eq!(listing.entries[0].name, "video.mp4");
        assert!(!listing.entries[0].directory);
        let Body::FileEntry(video) = files
            .dispatch(
                1,
                Call::ResolveFile(agent_protocol::operations::ResolveFile {
                    path: format!("{prefix}/Movies/video.mp4"),
                }),
            )
            .unwrap()
        else {
            panic!()
        };
        assert!(!video.directory);
        assert_eq!(
            Path::new(&listing.entries[0].path),
            dunce::canonicalize(&video.path).unwrap()
        );
        let Body::File(content) = files
            .dispatch(
                1,
                Call::ReadFile(PathParams {
                    path: format!("{prefix}/notes.md"),
                }),
            )
            .unwrap()
        else {
            panic!()
        };
        files
            .dispatch(
                1,
                Call::WriteFile(agent_protocol::operations::WriteFile {
                    path: format!("{prefix}/notes.md"),
                    revision: content.revision,
                    text: "changed".into(),
                }),
            )
            .unwrap();
        assert_eq!(
            fs::read_to_string(directory.path().join("notes.md")).unwrap(),
            "changed"
        );
        let grant = files
            .dispatch(
                1,
                Call::Download(PathParams {
                    path: format!("{prefix}/Movies/video.mp4"),
                }),
            )
            .unwrap();
        let (mut client, server) = tokio::io::duplex(1024);
        send_token(&mut client, &grant).await;
        files.transfer(1, server).await.unwrap();
        let mut bytes = Vec::new();
        client.read_to_end(&mut bytes).await.unwrap();
        assert_eq!(bytes, b"video");
        assert_eq!(absolute_path(&"~").unwrap(), home.home_dir());
        assert_eq!(
            absolute_path(&"~/Movies/video.mp4").unwrap().to_str(),
            home.home_dir().join("Movies").join("video.mp4").to_str()
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolving_a_symlink_keeps_the_filename_used_for_file_presentation() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("content"), b"video").unwrap();
        let path = directory.path().join("video.mp4");
        std::os::unix::fs::symlink("content", &path).unwrap();
        let files = WorkspaceFiles::new(directory.path().join("attachments"));
        let Body::FileEntry(entry) = files
            .dispatch(
                1,
                Call::ResolveFile(agent_protocol::operations::ResolveFile {
                    path: path.to_str().unwrap().into(),
                }),
            )
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(entry.path, path.to_str().unwrap());
        assert_eq!(entry.name, "video.mp4");
        assert!(!entry.directory);
        assert_eq!(entry.size, 5);
    }

    #[cfg(unix)]
    #[test]
    fn alias_reads_and_saves_preserve_the_editor_path_and_leave_the_symlink_intact() {
        let directory = tempfile::tempdir().unwrap();
        let real = directory.path().join("content");
        fs::write(&real, "original").unwrap();
        fs::create_dir(directory.path().join("nested")).unwrap();
        let alias = directory.path().join("notes.md");
        std::os::unix::fs::symlink("content", &alias).unwrap();
        let files = WorkspaceFiles::new(directory.path().join("attachments"));
        for path in [alias.clone(), directory.path().join("nested/../notes.md")] {
            let requested = path.to_str().unwrap().to_owned();
            let Body::File(content) = files
                .dispatch(
                    1,
                    Call::ReadFile(PathParams {
                        path: requested.clone(),
                    }),
                )
                .unwrap()
            else {
                panic!()
            };
            assert_eq!(content.path, requested);
            let Body::File(saved) = files
                .dispatch(
                    1,
                    Call::WriteFile(agent_protocol::operations::WriteFile {
                        path: requested.clone(),
                        revision: content.revision,
                        text: "edited".into(),
                    }),
                )
                .unwrap()
            else {
                panic!()
            };
            assert_eq!(saved.path, requested);
            assert_eq!(saved.text, "edited");
            assert_eq!(fs::read_to_string(&real).unwrap(), "edited");
            assert!(
                fs::symlink_metadata(&alias)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }
    }

    #[test]
    fn relative_paths_and_other_users_are_not_home_links() {
        for path in ["relative", "~other/file", "other/~/file", "./file"] {
            assert!(absolute_path(&path).is_err(), "{path}");
        }
    }

    #[test]
    fn file_failures_classify_io_causes_and_preserve_other_host_reasons() {
        for (kind, code) in [
            (std::io::ErrorKind::NotFound, "file_not_found"),
            (std::io::ErrorKind::PermissionDenied, "file_access_denied"),
            (std::io::ErrorKind::Other, "file_operation_failed"),
        ] {
            let error =
                anyhow::Error::new(std::io::Error::new(kind, "Host reason")).context("read file");
            let failure = file_failure(error);
            assert_eq!(failure.code, code);
            assert!(failure.message.contains("Host reason"));
        }
        assert_eq!(
            file_failure(anyhow!("editor size limit")).message,
            "editor size limit"
        );
    }

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
            let attachments = directory.path().join("attachments");
            let files = WorkspaceFiles::new(attachments.clone());
            for bytes in [&b"wrong"[..], &b"longer"[..]] {
                let grant = files
                    .dispatch(
                        1,
                        Call::Upload(Upload {
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
                assert_eq!(fs::read_dir(&attachments).unwrap().count(), 0);
            }
            assert!(
                files
                    .dispatch(
                        1,
                        Call::Upload(Upload {
                            file_name: "../escape".into(),
                            size: 0,
                            sha256: digest::digest(&SHA256, b"").as_ref().try_into().unwrap()
                        })
                    )
                    .is_err()
            );
            for path in ["relative", ".", ".."] {
                let files = WorkspaceFiles::new(path.into());
                assert!(matches!(
                    files.dispatch(1, Call::Upload(Upload {
                        file_name: "safe.txt".into(),
                        size: 0,
                        sha256: digest::digest(&SHA256, b"").as_ref().try_into().unwrap(),
                    })),
                    Err(error) if error.to_string() == "an absolute filesystem path is required"
                ));
            }
            assert_eq!(fs::read_dir(&attachments).unwrap().count(), 0);
        })
        .await
        .unwrap();
    }
}

//! Uploaded attachments and their claim by a thread's message. Paths given to the
//! conversation come from this storage, never from the client.
use super::*;
use agent_domain::{Attachment, AttachmentKind};
use serde::{Deserialize, Serialize};

const MAX_ATTACHMENTS: usize = 100;
const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
const MAX_MESSAGE_IMAGE_BYTES: u64 = 80 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct Manifest {
    attachment: Attachment,
    sha256: [u8; 32],
}

fn native_image(mime: &str) -> bool {
    matches!(
        mime,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    )
}

/// What the limits check of one message attachment reads.
struct Limits<'a> {
    id: &'a str,
    image: bool,
    name: &'a str,
    mime: &'a str,
    size: u64,
}
impl<'a> From<&'a Attachment> for Limits<'a> {
    fn from(a: &'a Attachment) -> Self {
        Self {
            id: &a.id,
            image: a.kind == AttachmentKind::Image,
            name: &a.name,
            mime: &a.mime_type,
            size: a.size,
        }
    }
}
fn validate<'a>(attachments: impl IntoIterator<Item = Limits<'a>>) -> Result<()> {
    let mut ids = std::collections::BTreeSet::new();
    let mut image_bytes = 0u64;
    for (index, a) in attachments.into_iter().enumerate() {
        if index == MAX_ATTACHMENTS {
            return Err(anyhow!("You can attach up to 100 files per message."));
        }
        if !ids.insert(a.id) {
            return Err(anyhow!("Duplicate attachment ids are not allowed."));
        }
        if a.image != native_image(a.mime) {
            return Err(anyhow!("Attachment kind does not match its MIME type."));
        }
        if a.name.is_empty()
            || a.name.len() > 200
            || a.name.chars().any(char::is_control)
            || a.mime.is_empty()
            || a.mime.len() > 100
        {
            return Err(anyhow!("Invalid attachment metadata."));
        }
        let max = if a.image {
            MAX_IMAGE_BYTES
        } else {
            MAX_FILE_BYTES
        };
        if a.size == 0 || a.size > max {
            return Err(anyhow!("Attachment exceeds the size limit."));
        }
        if a.image {
            image_bytes = image_bytes.saturating_add(a.size);
        }
    }
    if image_bytes > MAX_MESSAGE_IMAGE_BYTES {
        return Err(anyhow!("Images can total up to 80 MiB per message."));
    }
    Ok(())
}
/// Whether the id names an upload no thread has claimed yet.
pub(crate) fn is_pending_upload(id: &str) -> bool {
    id.starts_with("pending-")
}
/// `pending-<token>` uploads and `chat-<thread hash>-<token>` claims. Both fit
/// the attachment ID schema of messages and context records:
/// `^[a-z0-9_-]{1,128}$`, ignoring case.
fn stored_path(root: &Path, id: &str) -> Option<PathBuf> {
    let safe = |s: &str| {
        !s.is_empty()
            && s.len() <= 128
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    };
    if let Some(token) = id.strip_prefix("pending-") {
        return safe(token).then(|| root.join("pending").join(token));
    }
    let claimed = id.strip_prefix("chat-")?;
    let (thread, token) = (claimed.get(..43)?, claimed.get(43..)?.strip_prefix('-')?);
    (safe(thread) && safe(token) && id.len() <= 128)
        .then(|| root.join("chat").join(thread).join(token))
}
fn claimed_id(thread: &str, token: &str) -> String {
    format!("chat-{}-{token}", hash(thread.as_bytes()))
}

struct NewClaims(Vec<PathBuf>);
impl Drop for NewClaims {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = fs::remove_file(path);
            let _ = fs::remove_file(path.with_extension("meta"));
        }
    }
}

impl WorkspaceFiles {
    pub(super) fn sweep_pending_attachments(&self) -> Result<()> {
        let mut last = self.pending_sweep.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|time| time.elapsed() < Duration::from_secs(3600)) {
            return Ok(());
        }
        let directory = self.upload_directory.join("pending");
        self.prepare_attachment_directory(&directory)?;
        let _lock = self.writes.lock().unwrap_or_else(|e| e.into_inner());
        for entry in fs::read_dir(&directory)?.take(10000) {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.len() != 32 || !name.bytes().all(|b| b.is_ascii_hexdigit()) {
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata
                    .modified()?
                    .elapsed()
                    .is_ok_and(|time| time > Duration::from_secs(24 * 3600))
            {
                fs::remove_file(entry.path())?;
                let _ = fs::remove_file(entry.path().with_extension("meta"));
            }
        }
        *last = Some(Instant::now());
        Ok(())
    }
    pub(super) fn prepare_attachment_directory(&self, directory: &Path) -> Result<()> {
        let relative = directory.strip_prefix(&self.upload_directory)?;
        let mut path = self.upload_directory.to_path_buf();
        for component in std::iter::once(None).chain(relative.components().map(Some)) {
            if let Some(component) = component {
                path.push(component);
            }
            match fs::symlink_metadata(&path) {
                Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
                    return Err(anyhow!("attachment directory is not private storage"));
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    crate::platform::create_state_directory(&path)?
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
    pub(super) fn attachment_metadata(
        id: String,
        path: &Path,
        name: &str,
        mime: &str,
        size: u64,
    ) -> Result<Attachment> {
        let mime = mime.to_ascii_lowercase();
        let attachment = Attachment {
            kind: if native_image(&mime) {
                AttachmentKind::Image
            } else {
                AttachmentKind::File
            },
            source: None,
            id,
            name: name.into(),
            mime_type: mime,
            path: path.to_string_lossy().into_owned(),
            size,
        };
        validate([Limits::from(&attachment)])?;
        Ok(attachment)
    }
    pub(super) fn save_attachment_upload(
        &self,
        path: &Path,
        id: String,
        name: &str,
        mime: &str,
        size: u64,
        sha256: [u8; 32],
    ) -> Result<Attachment> {
        let attachment = Self::attachment_metadata(id, path, name, mime, size)?;
        if attachment.kind == AttachmentKind::Image {
            let mut bytes = [0; 16];
            let mut file = File::open(path)?;
            let count = file.read(&mut bytes)?;
            let bytes = &bytes[..count];
            let valid = match attachment.mime_type.as_str() {
                "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
                "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
                "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
                "image/webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
                _ => false,
            };
            if !valid {
                return Err(anyhow!("image content does not match its MIME type"));
            }
        }
        atomicwrites::AtomicFile::new(path.with_extension("meta"), atomicwrites::DisallowOverwrite)
            .write_with_options(
                |file| {
                    file.write_all(&serde_json::to_vec(&Manifest {
                        attachment: attachment.clone(),
                        sha256,
                    })?)
                },
                crate::platform::private_file_options(),
            )?;
        Ok(attachment)
    }
    fn manifest(&self, id: &str) -> Result<(PathBuf, Manifest)> {
        let path = stored_path(&self.upload_directory, id)
            .ok_or_else(|| anyhow!("invalid attachment id"))?;
        self.prepare_attachment_directory(path.parent().context("attachment parent missing")?)?;
        for target in [&path, &path.with_extension("meta")] {
            let m = fs::symlink_metadata(target)?;
            if !m.is_file() || m.file_type().is_symlink() {
                return Err(anyhow!("attachment is not a regular owned file"));
            }
        }
        let manifest: Manifest =
            serde_json::from_slice(&read_bounded(&path.with_extension("meta"), 65536)?)?;
        if manifest.attachment.id != id {
            return Err(anyhow!("attachment identity mismatch"));
        }
        validate([Limits::from(&manifest.attachment)])?;
        Ok((path, manifest))
    }
    pub(super) fn attachment_path(&self, id: &str) -> Result<PathBuf> {
        self.manifest(id).map(|(path, _)| path)
    }

    /// Claims a message's attachments for `thread`: pending uploads are copied
    /// into the thread's storage and every path is the stored file's.
    pub(crate) fn claim(&self, thread: &str, input: &[Attachment]) -> Result<Vec<Attachment>> {
        if input.is_empty() {
            return Ok(vec![]);
        }
        validate(input.iter().map(Limits::from))?;
        let _lock = self.writes.lock().unwrap_or_else(|e| e.into_inner());
        let directory = self.thread_attachment_directory(thread);
        self.prepare_attachment_directory(&directory)?;
        let mut claimed = vec![];
        let mut created = NewClaims(vec![]);
        for attachment in input {
            let (source, manifest) = self.manifest(&attachment.id)?;
            let uploaded = &manifest.attachment;
            if uploaded.name != attachment.name
                || uploaded.mime_type != attachment.mime_type
                || uploaded.size != attachment.size
                || uploaded.kind != attachment.kind
            {
                return Err(anyhow!("attachment metadata changed"));
            }
            let mut attachment = attachment.clone();
            let target = match attachment.id.strip_prefix("pending-") {
                Some(token) => {
                    let mut file = File::open(&source)?;
                    let (size, digest) = digest_file(&mut file)?;
                    if size != attachment.size || digest != manifest.sha256 {
                        return Err(anyhow!("pending attachment content changed"));
                    }
                    let id = claimed_id(thread, token);
                    let target =
                        stored_path(&self.upload_directory, &id).context("invalid claim id")?;
                    attachment.id = id;
                    if target.exists() {
                        let (_, saved) = self.manifest(&attachment.id)?;
                        if saved.attachment.id != attachment.id
                            || saved.attachment.name != uploaded.name
                            || saved.attachment.size != uploaded.size
                            || saved.sha256 != digest
                        {
                            return Err(anyhow!("attachment claim changed"));
                        }
                    } else {
                        let output = tempfile::NamedTempFile::new_in(&directory)?;
                        fs::copy(&source, output.path())?;
                        output.as_file().sync_all()?;
                        output.persist_noclobber(&target)?;
                        created.0.push(target.clone());
                        if let Err(e) = self.save_attachment_upload(
                            &target,
                            attachment.id.clone(),
                            &attachment.name,
                            &attachment.mime_type,
                            size,
                            digest,
                        ) {
                            let _ = fs::remove_file(&target);
                            return Err(e);
                        }
                    }
                    target
                }
                None => source,
            };
            attachment.path = target.to_string_lossy().into_owned();
            claimed.push(attachment);
        }
        created.0.clear();
        Ok(claimed)
    }

    /// The stored bytes of a claimed image.
    pub(crate) fn image(&self, attachment: &Attachment) -> Result<Vec<u8>> {
        let (path, manifest) = self.manifest(&attachment.id)?;
        if manifest.attachment.kind != AttachmentKind::Image {
            return Err(anyhow!("attachment is not an image"));
        }
        read_bounded(&path, MAX_IMAGE_BYTES)
    }

    /// Deletes claimed attachment files; paths outside thread storage are ignored.
    pub(crate) async fn delete_claimed(&self, paths: Vec<String>) -> Result<()> {
        let files = self.clone();
        tokio::task::spawn_blocking(move || {
            let _lock = files.writes.lock().unwrap_or_else(|e| e.into_inner());
            let storage = files.upload_directory.join("chat");
            for path in paths {
                let path = PathBuf::from(path);
                let Ok(relative) = path.strip_prefix(&storage) else {
                    continue;
                };
                if relative.components().count() != 2
                    || !relative
                        .components()
                        .all(|part| matches!(part, std::path::Component::Normal(_)))
                {
                    continue;
                }
                files.prepare_attachment_directory(path.parent().context("attachment parent")?)?;
                for target in [path.clone(), path.with_extension("meta")] {
                    match fs::remove_file(&target) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error.into()),
                    }
                }
            }
            Ok(())
        })
        .await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upload(files: &WorkspaceFiles, token: &str, contents: &[u8]) -> Attachment {
        upload_as(
            files,
            token,
            contents,
            &format!("{token}.txt"),
            "text/plain",
        )
    }
    fn upload_as(
        files: &WorkspaceFiles,
        token: &str,
        contents: &[u8],
        name: &str,
        mime: &str,
    ) -> Attachment {
        let pending = files.attachment_root().join("pending");
        files.prepare_attachment_directory(&pending).unwrap();
        let path = pending.join(token);
        fs::write(&path, contents).unwrap();
        let digest = digest_file(&mut File::open(&path).unwrap()).unwrap().1;
        let uploaded = files
            .save_attachment_upload(
                &path,
                format!("pending-{token}"),
                name,
                mime,
                contents.len() as u64,
                digest,
            )
            .unwrap();
        Attachment {
            path: "/client/supplied/path".into(),
            ..uploaded
        }
    }

    #[tokio::test]
    async fn claims_copy_pending_uploads_into_thread_storage_and_delete_only_there() {
        let directory = tempfile::tempdir().unwrap();
        let files = WorkspaceFiles::new(directory.path().join("assets"));
        let pending = upload(&files, "token", b"original");
        let claimed = files
            .claim("thread", std::slice::from_ref(&pending))
            .unwrap();
        let stored = files.attachment_path(&claimed[0].id).unwrap();
        assert!(claimed[0].id.starts_with("chat-"));
        assert_eq!(claimed[0].path, stored.to_string_lossy());
        fs::write(&stored, b"edited").unwrap();
        // A resent claim keeps the stored copy and the original upload.
        assert_eq!(
            files
                .claim("thread", std::slice::from_ref(&pending))
                .unwrap(),
            claimed
        );
        assert_eq!(fs::read(&stored).unwrap(), b"edited");
        assert_eq!(
            files
                .claim("thread", std::slice::from_ref(&claimed[0]))
                .unwrap(),
            claimed
        );
        assert!(files.attachment_path("pending-../../checkout").is_err());
        let mut changed = pending.clone();
        changed.name = "renamed.txt".into();
        assert!(files.claim("thread", &[changed]).is_err());

        let outside = directory.path().join("outside.txt");
        fs::write(&outside, b"keep").unwrap();
        files
            .delete_claimed(vec![
                claimed[0].path.clone(),
                outside.to_string_lossy().into_owned(),
            ])
            .await
            .unwrap();
        assert!(!stored.exists());
        assert!(outside.exists());
    }

    #[test]
    fn a_failed_batch_removes_only_its_new_claims() {
        let directory = tempfile::tempdir().unwrap();
        let files = WorkspaceFiles::new(directory.path().join("assets"));
        let first = upload(&files, "one", b"original");
        let missing = Attachment {
            id: "pending-missing".into(),
            ..first.clone()
        };
        assert!(files.claim("thread", &[first.clone(), missing]).is_err());
        assert_eq!(
            fs::read_dir(files.thread_attachment_directory("thread"))
                .unwrap()
                .count(),
            0
        );
        assert!(files.claim("thread", &[first]).is_ok());
    }

    // Context records name uploads by attachment ID; a claim rebinds them to
    // the thread's copies, which the record schema must still accept.
    #[test]
    fn claimed_uploads_keep_their_context_records() {
        let directory = tempfile::tempdir().unwrap();
        let files = WorkspaceFiles::new(directory.path().join("assets"));
        let image = upload_as(
            &files,
            "0123456789abcdef0123456789abcdef",
            b"\x89PNG\r\n\x1a\nimage",
            "shot.png",
            "image/png",
        );
        let file = upload(&files, "fedcba9876543210fedcba9876543210", b"notes");
        let record = |kind: &str, context: &str, attachment: &Attachment| {
            agent_domain::Json(serde_json::json!({
                "version": 1, "contextId": context, "kind": kind, "label": attachment.name,
                "attachmentId": attachment.id, "name": attachment.name,
                "mimeType": attachment.mime_type, "sizeBytes": attachment.size,
            }))
        };
        let mut context = agent_domain::MessageContext {
            version: 1,
            records: vec![
                record("image", "ctx_image", &image),
                record("file", "ctx_file", &file),
            ],
        };
        assert_eq!(context.normalized().unwrap(), context);
        let claimed = files
            .claim("thread-1", &[image.clone(), file.clone()])
            .unwrap();
        context.remap_attachments(&std::collections::HashMap::from([
            (image.id.clone(), claimed[0].id.clone()),
            (file.id.clone(), claimed[1].id.clone()),
        ]));
        let normalized = context.normalized().unwrap();
        assert_eq!(
            normalized
                .records
                .iter()
                .map(|record| (
                    record.0["kind"].as_str().unwrap(),
                    record.0["attachmentId"].as_str().unwrap()
                ))
                .collect::<Vec<_>>(),
            [
                ("image", claimed[0].id.as_str()),
                ("file", claimed[1].id.as_str())
            ]
        );
        let provider = agent_domain::project_context_for_provider(
            "See ![shot.png](context://v1/image/ctx_image) and [notes](context://v1/file/ctx_file)",
            Some(&normalized),
        );
        assert!(provider.contains(&claimed[0].id), "{provider}");
        assert!(provider.contains(&claimed[1].id), "{provider}");
    }

    proptest::proptest! {
        // Upload and claim IDs satisfy `^[a-z0-9_-]{1,128}$` (ignoring case)
        // and name the storage they were made for.
        #[test]
        fn attachment_ids_fit_the_schema_and_resolve_to_their_storage(
            thread in proptest::prelude::any::<String>(),
            token in "[0-9a-f]{32}",
        ) {
            let schema = regex::Regex::new("^[A-Za-z0-9_-]{1,128}$").unwrap();
            let root = Path::new("/assets");
            let pending = format!("pending-{token}");
            proptest::prop_assert!(schema.is_match(&pending));
            proptest::prop_assert_eq!(stored_path(root, &pending), Some(root.join("pending").join(&token)));
            let claimed = claimed_id(&thread, &token);
            proptest::prop_assert!(schema.is_match(&claimed), "{}", claimed);
            proptest::prop_assert_eq!(
                stored_path(root, &claimed),
                Some(root.join("chat").join(hash(thread.as_bytes())).join(&token))
            );
        }
    }
}

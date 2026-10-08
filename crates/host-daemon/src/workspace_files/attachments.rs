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
/// The count and image budget of one message or question response.
pub(crate) fn limit_error(attachments: &[Attachment]) -> Option<&'static str> {
    if attachments.len() > MAX_ATTACHMENTS {
        return Some("You can attach up to 100 files per message or question response.");
    }
    let image_bytes = attachments
        .iter()
        .filter(|a| a.kind == AttachmentKind::Image || native_image(&a.mime_type.to_lowercase()))
        .fold(0u64, |total, a| total.saturating_add(a.size));
    (image_bytes > MAX_MESSAGE_IMAGE_BYTES).then_some(
        "Images can total up to 80 MiB per message or question response. Use smaller images or send fewer at once.",
    )
}
fn validate<'a>(attachments: impl IntoIterator<Item = Limits<'a>>) -> Result<()> {
    let mut ids = std::collections::BTreeSet::new();
    let mut image_bytes = 0u64;
    for (index, a) in attachments.into_iter().enumerate() {
        if index == MAX_ATTACHMENTS {
            return Err(anyhow!(
                "You can attach up to 100 files per message or question response."
            ));
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
        return Err(anyhow!(
            "Images can total up to 80 MiB per message or question response. Use smaller images or send fewer at once."
        ));
    }
    Ok(())
}

fn validate_capture_source(source: Option<&agent_domain::CapturedWindow>) -> Result<()> {
    let Some(source) = source else {
        return Ok(());
    };
    if source.app_name.len() > 512
        || source.window_title.len() > 2_048
        || source
            .accessible_text
            .as_ref()
            .is_some_and(|text| text.len() > 64 * 1024)
        || serde_json::to_vec(source)
            .map(|bytes| bytes.len() > 128 * 1024)
            .unwrap_or(true)
    {
        return Err(anyhow!("captured window metadata is too large"));
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

/// What a claim made: the claimed attachments and the thread copies it holds.
#[derive(Debug, Default)]
pub(crate) struct Claimed {
    pub(crate) attachments: Vec<Attachment>,
    pub(crate) copies: Copies,
}

/// The thread copies one claim holds until its command's outcome is known.
/// Claim ids are deterministic, so a resent or duplicate command's claim holds
/// the same copy as the first. Releasing removes a copy only when the claim
/// that made it is released, no other claim holds it and none was accepted.
/// Dropping the hold keeps the copies: the command was accepted, or its outcome
/// is unknown.
#[derive(Default)]
pub(crate) struct Copies {
    files: Option<WorkspaceFiles>,
    paths: Vec<PathBuf>,
}
impl std::fmt::Debug for Copies {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_list().entries(&self.paths).finish()
    }
}
impl Copies {
    /// The command was not accepted.
    pub(crate) fn release(mut self) {
        self.settle(false);
    }
    fn settle(&mut self, accepted: bool) {
        if let Some(files) = self.files.take() {
            let _lock = files.writes.lock().unwrap_or_else(|e| e.into_inner());
            files.settle_locked(std::mem::take(&mut self.paths), accepted);
        }
    }
    fn absorb(&mut self, mut other: Copies) {
        if let Some(files) = other.files.take() {
            self.files.get_or_insert(files);
        }
        self.paths.append(&mut other.paths);
    }
    #[cfg(test)]
    pub(crate) fn paths(&self) -> &[PathBuf] {
        &self.paths
    }
    /// Copies no storage tracks, for a backend that keeps none.
    #[cfg(test)]
    pub(crate) fn untracked(paths: Vec<PathBuf>) -> Self {
        Self { files: None, paths }
    }
}
impl Drop for Copies {
    fn drop(&mut self) {
        self.settle(true);
    }
}

/// The claims that hold one thread copy while their commands are in flight.
#[derive(Default)]
pub(super) struct Holds {
    claims: usize,
    /// One of them made the copy.
    made: bool,
    /// One of them was accepted.
    accepted: bool,
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
        source: Option<agent_domain::CapturedWindow>,
    ) -> Result<Attachment> {
        validate_capture_source(source.as_ref())?;
        let mime = mime.to_ascii_lowercase();
        let attachment = Attachment {
            kind: if native_image(&mime) {
                AttachmentKind::Image
            } else {
                AttachmentKind::File
            },
            source,
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
        source: Option<agent_domain::CapturedWindow>,
    ) -> Result<Attachment> {
        let attachment = Self::attachment_metadata(id, path, name, mime, size, source)?;
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
    /// into the thread's storage and every path is the stored file's. A
    /// failure leaves no copy behind and says why the attachment cannot be sent.
    pub(crate) fn claim(
        &self,
        thread: &str,
        input: &[Attachment],
    ) -> std::result::Result<Claimed, String> {
        if input.is_empty() {
            return Ok(Claimed::default());
        }
        if let Some(error) = limit_error(input) {
            return Err(error.into());
        }
        let mut ids = std::collections::BTreeSet::new();
        if !input.iter().all(|attachment| ids.insert(&attachment.id)) {
            return Err("Duplicate attachment ids are not allowed.".into());
        }
        validate(input.iter().map(Limits::from)).map_err(|error| error.to_string())?;
        let _lock = self.writes.lock().unwrap_or_else(|e| e.into_inner());
        let mut held = vec![];
        match self.claim_locked(thread, input, &mut held) {
            Ok(attachments) => Ok(Claimed {
                attachments,
                copies: Copies {
                    files: Some(self.clone()),
                    paths: held,
                },
            }),
            Err(error) => {
                self.settle_locked(held, false);
                Err(error)
            }
        }
    }

    fn hold(&self, held: &mut Vec<PathBuf>, path: &Path, made: bool) {
        let mut holds = self.claim_holds.lock().unwrap_or_else(|e| e.into_inner());
        let hold = holds.entry(path.to_path_buf()).or_default();
        hold.claims += 1;
        hold.made |= made;
        held.push(path.to_path_buf());
    }

    /// Ends the claims' holds; the caller holds the write lock.
    fn settle_locked(&self, paths: Vec<PathBuf>, accepted: bool) {
        let mut holds = self.claim_holds.lock().unwrap_or_else(|e| e.into_inner());
        for path in paths {
            let Some(hold) = holds.get_mut(&path) else {
                continue;
            };
            hold.claims -= 1;
            hold.accepted |= accepted;
            if hold.claims > 0 {
                continue;
            }
            let unused = hold.made && !hold.accepted;
            holds.remove(&path);
            if unused {
                let _ = fs::remove_file(&path);
                let _ = fs::remove_file(path.with_extension("meta"));
            }
        }
    }

    fn claim_locked(
        &self,
        thread: &str,
        input: &[Attachment],
        held: &mut Vec<PathBuf>,
    ) -> std::result::Result<Vec<Attachment>, String> {
        let directory = self.thread_attachment_directory(thread);
        let mut claimed = vec![];
        for attachment in input {
            let cannot = |reason: &str| {
                format!("Attachment '{}' cannot be sent: {reason}.", attachment.name)
            };
            let failed = || {
                format!(
                    "Failed to claim attachment '{}' for this thread.",
                    attachment.name
                )
            };
            let pending = attachment.id.strip_prefix("pending-");
            let (source, manifest) = self.manifest(&attachment.id).map_err(|_| {
                cannot(if pending.is_some() {
                    "attachment not found (removed or expired)"
                } else {
                    "attachment not found"
                })
            })?;
            let uploaded = &manifest.attachment;
            if uploaded.size != attachment.size {
                return Err(cannot("stored size does not match"));
            }
            if uploaded.kind != attachment.kind
                || !uploaded
                    .mime_type
                    .eq_ignore_ascii_case(&attachment.mime_type)
            {
                return Err(cannot("attachment type does not match the upload"));
            }
            let mut attachment = attachment.clone();
            let target = match pending {
                Some(token) => {
                    attachment.mime_type = attachment.mime_type.to_lowercase();
                    let (size, digest) = File::open(&source)
                        .map_err(anyhow::Error::from)
                        .and_then(|mut file| digest_file(&mut file))
                        .map_err(|_| failed())?;
                    if size != attachment.size {
                        return Err(cannot("stored size does not match"));
                    }
                    if digest != manifest.sha256 {
                        return Err(failed());
                    }
                    let id = claimed_id(thread, token);
                    let target = stored_path(&self.upload_directory, &id)
                        .ok_or_else(|| cannot("invalid attachment id"))?;
                    attachment.id = id;
                    if target.exists() {
                        let (_, saved) = self.manifest(&attachment.id).map_err(|_| failed())?;
                        if saved.attachment.id != attachment.id
                            || saved.attachment.size != uploaded.size
                            || saved.sha256 != digest
                        {
                            return Err(failed());
                        }
                        self.hold(held, &target, false);
                    } else {
                        self.prepare_attachment_directory(&directory)
                            .map_err(|_| failed())?;
                        let output =
                            tempfile::NamedTempFile::new_in(&directory).map_err(|_| failed())?;
                        fs::copy(&source, output.path()).map_err(|_| failed())?;
                        output.as_file().sync_all().map_err(|_| failed())?;
                        output.persist_noclobber(&target).map_err(|_| failed())?;
                        self.hold(held, &target, true);
                        self.save_attachment_upload(
                            &target,
                            attachment.id.clone(),
                            &attachment.name,
                            &attachment.mime_type,
                            size,
                            digest,
                            attachment.source.clone(),
                        )
                        .map_err(|_| failed())?;
                    }
                    target
                }
                None => {
                    self.hold(held, &source, false);
                    source
                }
            };
            attachment.path = target.to_string_lossy().into_owned();
            claimed.push(attachment);
        }
        Ok(claimed)
    }

    /// Claims a question response's attachments question by question. The
    /// limits hold across every question, and a failure releases every copy.
    pub(crate) fn claim_answers(
        &self,
        thread: &str,
        answers: &mut std::collections::BTreeMap<String, Vec<Attachment>>,
    ) -> std::result::Result<Copies, String> {
        let all: Vec<Attachment> = answers.values().flatten().cloned().collect();
        if let Some(error) = limit_error(&all) {
            return Err(error.into());
        }
        let mut copies = Copies::default();
        for attachments in answers.values_mut() {
            match self.claim(thread, attachments) {
                Ok(claimed) => {
                    *attachments = claimed.attachments;
                    copies.absorb(claimed.copies);
                }
                Err(error) => {
                    copies.release();
                    return Err(error);
                }
            }
        }
        Ok(copies)
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
                None,
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
        let first = files
            .claim("thread", std::slice::from_ref(&pending))
            .unwrap();
        let claimed = first.attachments;
        let stored = files.attachment_path(&claimed[0].id).unwrap();
        assert_eq!(first.copies.paths(), std::slice::from_ref(&stored));
        assert!(claimed[0].id.starts_with("chat-"));
        assert_eq!(claimed[0].path, stored.to_string_lossy());
        fs::write(&stored, b"edited").unwrap();
        // A resent claim holds the stored copy and keeps the original upload.
        let resent = files
            .claim("thread", std::slice::from_ref(&pending))
            .unwrap();
        assert_eq!(resent.attachments, claimed);
        assert_eq!(resent.copies.paths(), std::slice::from_ref(&stored));
        assert_eq!(fs::read(&stored).unwrap(), b"edited");
        assert_eq!(
            files
                .claim("thread", std::slice::from_ref(&claimed[0]))
                .unwrap()
                .attachments,
            claimed
        );
        assert!(files.attachment_path("pending-../../checkout").is_err());
        // The name is the message's own label of the upload.
        let renamed = files
            .claim(
                "thread",
                &[Attachment {
                    name: "renamed.txt".into(),
                    ..pending.clone()
                }],
            )
            .unwrap();
        assert_eq!(renamed.attachments[0].name, "renamed.txt");
        assert_eq!(renamed.attachments[0].path, claimed[0].path);

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
    fn screenshot_window_metadata_survives_pending_claim() {
        let directory = tempfile::tempdir().unwrap();
        let files = WorkspaceFiles::new(directory.path().join("assets"));
        let pending = files.attachment_root().join("pending");
        files.prepare_attachment_directory(&pending).unwrap();
        let path = pending.join("token");
        let contents = b"image-bytes";
        fs::write(&path, contents).unwrap();
        let digest = digest_file(&mut File::open(&path).unwrap()).unwrap().1;
        let source = agent_domain::CapturedWindow {
            app_name: "Editor".into(),
            window_title: "main.rs".into(),
            accessible_text: Some("fn main() {}".into()),
            accessibility: None,
        };
        // Use a text attachment to exercise metadata persistence without
        // weakening the Host's image content validation.
        let uploaded = files
            .save_attachment_upload(
                &path,
                "pending-token".into(),
                "shot.txt",
                "text/plain",
                contents.len() as u64,
                digest,
                Some(source.clone()),
            )
            .unwrap();
        let claimed = files.claim("thread", &[uploaded]).unwrap();
        assert_eq!(claimed.attachments[0].source, Some(source));
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

    // AttachmentClaims.ts claimPendingAttachments and getProviderAttachmentLimitError:
    // why an attachment cannot be sent, in the reference's words.
    #[test]
    fn claim_failures_say_why_in_the_reference_wording() {
        let directory = tempfile::tempdir().unwrap();
        let files = WorkspaceFiles::new(directory.path().join("assets"));
        let notes = upload(&files, "notes", b"original");
        let claim = |input: &[Attachment]| files.claim("thread", input).unwrap_err();
        assert_eq!(
            claim(&[Attachment {
                id: "pending-missing".into(),
                ..notes.clone()
            }]),
            "Attachment 'notes.txt' cannot be sent: attachment not found (removed or expired)."
        );
        assert_eq!(
            claim(&[Attachment {
                size: 3,
                ..notes.clone()
            }]),
            "Attachment 'notes.txt' cannot be sent: stored size does not match."
        );
        assert_eq!(
            claim(&[Attachment {
                mime_type: "text/markdown".into(),
                ..notes.clone()
            }]),
            "Attachment 'notes.txt' cannot be sent: attachment type does not match the upload."
        );
        assert_eq!(
            claim(&[notes.clone(), notes.clone()]),
            "Duplicate attachment ids are not allowed."
        );
        let many: Vec<Attachment> = (0..101)
            .map(|index| Attachment {
                id: format!("pending-{index}"),
                ..notes.clone()
            })
            .collect();
        assert_eq!(
            claim(&many),
            "You can attach up to 100 files per message or question response."
        );
        let images: Vec<Attachment> = (0..9)
            .map(|index| Attachment {
                kind: AttachmentKind::Image,
                id: format!("pending-image-{index}"),
                name: "shot.png".into(),
                mime_type: "image/png".into(),
                size: 10 * 1024 * 1024,
                ..notes.clone()
            })
            .collect();
        assert_eq!(
            claim(&images),
            "Images can total up to 80 MiB per message or question response. Use smaller images or send fewer at once."
        );
        // An upload's MIME type is matched without case and claimed lowercase.
        let upper = files
            .claim(
                "thread",
                &[Attachment {
                    mime_type: "TEXT/PLAIN".into(),
                    ..notes
                }],
            )
            .unwrap();
        assert_eq!(upper.attachments[0].mime_type, "text/plain");
    }

    // ThreadMessageIntake.ts dispatchCommand: the limits of a question response
    // hold across its questions, and a failure removes every copy made for it.
    #[test]
    fn a_question_response_is_bounded_and_released_as_a_whole() {
        let directory = tempfile::tempdir().unwrap();
        let files = WorkspaceFiles::new(directory.path().join("assets"));
        let notes = upload(&files, "notes", b"original");
        let mut over: std::collections::BTreeMap<String, Vec<Attachment>> = (0..2)
            .map(|question| {
                (
                    format!("q{question}"),
                    (0..60)
                        .map(|index| Attachment {
                            id: format!("pending-{question}-{index}"),
                            ..notes.clone()
                        })
                        .collect(),
                )
            })
            .collect();
        assert_eq!(
            files.claim_answers("thread", &mut over).unwrap_err(),
            "You can attach up to 100 files per message or question response."
        );
        let mut answers = std::collections::BTreeMap::from([
            ("a".to_owned(), vec![notes.clone()]),
            (
                "b".to_owned(),
                vec![Attachment {
                    id: "pending-missing".into(),
                    ..notes.clone()
                }],
            ),
        ]);
        assert!(files.claim_answers("thread", &mut answers).is_err());
        let copies = || {
            fs::read_dir(files.thread_attachment_directory("thread"))
                .map_or(0, |entries| entries.count())
        };
        assert_eq!(copies(), 0);
        let mut answers = std::collections::BTreeMap::from([("a".to_owned(), vec![notes])]);
        let created = files.claim_answers("thread", &mut answers).unwrap();
        assert_eq!(created.paths().len(), 1);
        assert!(answers["a"][0].id.starts_with("chat-"));
        created.release();
        assert_eq!(copies(), 0);
    }

    // AttachmentClaims.ts copies each claim under a new id, so releasing one
    // never touches another's copy. Here claim ids are deterministic and a
    // duplicate command's claim holds the copy the first one made: releasing
    // removes it only when no claim holding it was accepted.
    #[test]
    fn a_rejected_claim_never_removes_a_copy_an_accepted_claim_holds() {
        let directory = tempfile::tempdir().unwrap();
        let files = WorkspaceFiles::new(directory.path().join("assets"));
        let claim = |token: &str| {
            let pending = upload(&files, token, b"original");
            let made = files
                .claim("thread", std::slice::from_ref(&pending))
                .unwrap();
            let duplicate = files
                .claim("thread", std::slice::from_ref(&pending))
                .unwrap();
            let path = PathBuf::from(&made.attachments[0].path);
            assert_eq!(duplicate.attachments, made.attachments);
            (made.copies, duplicate.copies, path)
        };
        let kept = |path: &Path| path.exists() && path.with_extension("meta").exists();

        // The duplicate is accepted, then the claim that made the copy is rejected.
        let (made, duplicate, path) = claim("accepted-first");
        drop(duplicate);
        made.release();
        assert!(kept(&path));

        // The claim that made the copy is rejected while the duplicate is in
        // flight, and the duplicate is accepted.
        let (made, duplicate, path) = claim("rejected-first");
        made.release();
        assert!(kept(&path));
        drop(duplicate);
        assert!(kept(&path));

        // The claim that made the copy is accepted and the duplicate rejected.
        let (made, duplicate, path) = claim("duplicate-rejected");
        drop(made);
        duplicate.release();
        assert!(kept(&path));

        // Neither is accepted.
        let (made, duplicate, path) = claim("both-rejected");
        duplicate.release();
        assert!(kept(&path));
        made.release();
        assert!(!path.exists() && !path.with_extension("meta").exists());

        // A copy an earlier accepted claim made outlives a rejected resend.
        let pending = upload(&files, "resent", b"original");
        let first = files
            .claim("thread", std::slice::from_ref(&pending))
            .unwrap();
        let path = PathBuf::from(&first.attachments[0].path);
        drop(first);
        files
            .claim("thread", std::slice::from_ref(&pending))
            .unwrap()
            .copies
            .release();
        assert!(kept(&path));
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
            .unwrap()
            .attachments;
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

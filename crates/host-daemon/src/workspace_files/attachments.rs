use super::*;
use orchestration::{Attachment, AttachmentKind, Command, CommandBody, attachments};
use serde::{Deserialize, Serialize};
#[derive(Serialize, Deserialize)]
struct Manifest {
    attachment: Attachment,
    sha256: [u8; 32],
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
    pub(crate) async fn cleanup_unreferenced_attachments(
        &self,
        thread: &str,
        references: std::collections::BTreeSet<String>,
    ) -> Result<()> {
        let prefix = format!("chat:{}:", hash(thread.as_bytes()));
        let retained: Vec<_> = references
            .iter()
            .filter_map(|id| id.strip_prefix(&prefix).map(str::to_owned))
            .collect();
        if retained.is_empty() {
            return self.cleanup_thread_attachments(thread).await;
        }
        let files = self.clone();
        let thread = thread.to_owned();
        tokio::task::spawn_blocking(move || {
            let _lock = files.writes.lock().unwrap_or_else(|e| e.into_inner());
            let directory = files.thread_attachment_directory(&thread);
            if !directory.try_exists()? {
                return Ok(());
            }
            files.prepare_attachment_directory(&directory)?;
            for entry in fs::read_dir(&directory)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if !retained
                    .iter()
                    .any(|id| name == *id || name == format!("{id}.meta"))
                {
                    fs::remove_file(entry.path())?;
                }
            }
            Ok(())
        })
        .await?
    }
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
    pub(crate) fn attachment_root(&self) -> &Path {
        &self.upload_directory
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
        name: &str,
        mime: &str,
        size: u64,
    ) -> Result<Attachment> {
        let mime = mime.to_ascii_lowercase();
        let attachment = Attachment {
            id,
            kind: if attachments::native_image(&mime) {
                AttachmentKind::Image
            } else {
                AttachmentKind::File
            },
            name: name.into(),
            mime_type: mime,
            size_bytes: size,
        };
        attachments::validate(std::slice::from_ref(&attachment)).map_err(anyhow::Error::msg)?;
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
        let attachment = Self::attachment_metadata(id, name, mime, size)?;
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
        let path = attachments::path(&self.upload_directory, id)
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
        attachments::validate(std::slice::from_ref(&manifest.attachment))
            .map_err(anyhow::Error::msg)?;
        Ok((path, manifest))
    }
    pub(super) fn attachment_path(&self, id: &str) -> Result<PathBuf> {
        self.manifest(id).map(|(path, _)| path)
    }
    pub(crate) fn claim_command_attachments(&self, command: &Command) -> Result<Command> {
        let mut command = command.clone();
        let attachments = match &mut command.body {
            CommandBody::MessageDispatch(m) => Some(&mut m.attachments),
            CommandBody::QueuedRunEdit { attachments, .. } => attachments.as_mut(),
            _ => None,
        };
        if let Some(attachments) = attachments {
            *attachments = self.claim_attachments(&command.thread_id.to_string(), attachments)?;
        }
        Ok(command)
    }
    pub(crate) fn claim_attachments(
        &self,
        thread_id: &str,
        input: &[Attachment],
    ) -> Result<Vec<Attachment>> {
        if input.is_empty() {
            return Ok(vec![]);
        }
        attachments::validate(input).map_err(anyhow::Error::msg)?;
        let _lock = self.writes.lock().unwrap_or_else(|e| e.into_inner());
        let directory = self.thread_attachment_directory(thread_id);
        self.prepare_attachment_directory(&directory)?;
        let mut claimed = vec![];
        let mut created = NewClaims(vec![]);
        for attachment in input {
            let (source, manifest) = self.manifest(&attachment.id)?;
            if manifest.attachment != *attachment {
                return Err(anyhow!("attachment metadata changed"));
            }
            if let Some(token) = attachment.id.strip_prefix("pending:") {
                let mut file = File::open(&source)?;
                let (size, digest) = digest_file(&mut file)?;
                if size != attachment.size_bytes || digest != manifest.sha256 {
                    return Err(anyhow!("pending attachment content changed"));
                }
                let id = format!("chat:{}:{token}", hash(thread_id.as_bytes()));
                let target =
                    attachments::path(&self.upload_directory, &id).context("invalid claim id")?;
                let mut attachment = attachment.clone();
                attachment.id = id;
                if target.exists() {
                    let (_, saved) = self.manifest(&attachment.id)?;
                    if saved.attachment != attachment || saved.sha256 != digest {
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
                        let _ = fs::remove_file(target);
                        return Err(e);
                    }
                }
                claimed.push(attachment);
            } else {
                claimed.push(attachment.clone());
            }
        }
        created.0.clear();
        Ok(claimed)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn attachment_uploads_use_verified_binary_grants_and_keep_referenced_files() {
        let directory = tempfile::tempdir().unwrap();
        let files = WorkspaceFiles::new(directory.path().join("assets"));
        let bytes = b"content";
        let Body::Grant(grant) = files
            .dispatch(
                7,
                Call::Upload(agent_protocol::operations::Upload {
                    attachment_mime_type: Some("text/plain".into()),
                    directory: "/ignored-checkout".into(),
                    file_name: "test.txt".into(),
                    size: bytes.len() as u64,
                    sha256: digest::digest(&SHA256, bytes).as_ref().try_into().unwrap(),
                }),
            )
            .unwrap()
        else {
            panic!("grant");
        };
        let (mut client, server) = tokio::io::duplex(1024);
        let receiver = files.clone();
        let task = tokio::spawn(async move { receiver.transfer(7, server).await });
        client.write_all(&grant.token).await.unwrap();
        client.write_all(bytes).await.unwrap();
        client.shutdown().await.unwrap();
        let length = client.read_u32().await.unwrap();
        let mut response = vec![0; length as usize];
        client.read_exact(&mut response).await.unwrap();
        task.await.unwrap().unwrap();
        let response: agent_protocol::models::UploadedFile =
            agent_protocol::protocol::decode(&response).unwrap();
        let attachment = response.attachment.unwrap();
        assert!(attachment.id.starts_with("pending:"));
        let claimed = files.claim_attachments("thread", &[attachment]).unwrap();
        let path = files.attachment_path(&claimed[0].id).unwrap();
        files
            .cleanup_unreferenced_attachments(
                "thread",
                std::collections::BTreeSet::from([claimed[0].id.clone()]),
            )
            .await
            .unwrap();
        assert!(path.exists());
        files
            .cleanup_unreferenced_attachments("thread", Default::default())
            .await
            .unwrap();
        assert!(!path.exists());
    }
    #[test]
    fn claims_copy_pending_assets_without_overwriting_retries_or_following_paths() {
        let directory = tempfile::tempdir().unwrap();
        let files = WorkspaceFiles::new(directory.path().join("assets"));
        let pending = files.attachment_root().join("pending");
        files.prepare_attachment_directory(&pending).unwrap();
        let path = pending.join("token");
        fs::write(&path, b"original").unwrap();
        let digest = digest_file(&mut File::open(&path).unwrap()).unwrap().1;
        let a = files
            .save_attachment_upload(
                &path,
                "pending:token".into(),
                "a.txt",
                "text/plain",
                8,
                digest,
            )
            .unwrap();
        let result = files
            .claim_attachments("thread", std::slice::from_ref(&a))
            .unwrap();
        let target = files.attachment_path(&result[0].id).unwrap();
        fs::write(&target, b"edited").unwrap();
        assert_eq!(
            files
                .claim_attachments("thread", std::slice::from_ref(&a))
                .unwrap(),
            result
        );
        assert_eq!(fs::read(&target).unwrap(), b"edited");
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert!(files.attachment_path("pending:../../checkout").is_err());
        fs::write(&path, b"modified").unwrap();
        assert!(files.claim_attachments("other", &[a]).is_err());
    }
    #[test]
    fn failed_attachment_batch_removes_only_new_claims() {
        let directory = tempfile::tempdir().unwrap();
        let files = WorkspaceFiles::new(directory.path().join("assets"));
        let pending = files.attachment_root().join("pending");
        files.prepare_attachment_directory(&pending).unwrap();
        let path = pending.join("one");
        fs::write(&path, b"original").unwrap();
        let digest = digest_file(&mut File::open(&path).unwrap()).unwrap().1;
        let first = files
            .save_attachment_upload(
                &path,
                "pending:one".into(),
                "one.txt",
                "text/plain",
                8,
                digest,
            )
            .unwrap();
        let missing = Attachment {
            id: "pending:missing".into(),
            ..first.clone()
        };
        assert!(
            files
                .claim_attachments("thread", &[first.clone(), missing])
                .is_err()
        );
        assert!(path.exists());
        assert_eq!(
            fs::read_dir(files.thread_attachment_directory("thread"))
                .unwrap()
                .count(),
            0
        );
        assert!(files.claim_attachments("thread", &[first]).is_ok());
    }
}

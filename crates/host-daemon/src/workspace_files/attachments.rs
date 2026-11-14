//! T3's attachment IDs address files in the attachment directory. Display names
//! belong to ChatAttachment metadata; linked workspace files retain their paths.
use super::*;

pub(super) use agent_protocol::paths::ATTACHMENT_SCHEME as SCHEME;

fn extension(name: &str) -> String {
    agent_protocol::paths::attachment_extension(name, None)
}

pub(super) fn pending_file_name(file: &File, name: &str) -> Result<String> {
    let mut header = [0; 32];
    // The reopened descriptor starts at the beginning; the upload writer is
    // already synced and closed before this function is called.
    let length = file.take(header.len() as u64).read(&mut header)?;
    let extension = match image::guess_format(&header[..length]) {
        Ok(image::ImageFormat::Png) => "png".into(),
        Ok(image::ImageFormat::Jpeg) => "jpg".into(),
        Ok(image::ImageFormat::Gif) => "gif".into(),
        Ok(image::ImageFormat::WebP) => "webp".into(),
        _ => extension(name),
    };
    Ok(format!(
        "pending-{}-{extension}.{extension}",
        uuid::Uuid::new_v4()
    ))
}

impl WorkspaceFiles {
    pub(crate) fn persist_attachment(
        &self,
        thread_id: &str,
        message_id: &str,
        index: usize,
        path: &str,
        name: &str,
        is_image: bool,
    ) -> Result<serde_json::Value> {
        let source = self.read_path(path)?;
        let limit = if is_image {
            10 * 1024 * 1024
        } else {
            50 * 1024 * 1024
        };
        let bytes = read_bounded(&source, limit)?;
        let (extension, mime) = if is_image {
            match image::guess_format(&bytes)? {
                image::ImageFormat::Png => ("png".to_owned(), "image/png"),
                image::ImageFormat::Jpeg => ("jpg".to_owned(), "image/jpeg"),
                image::ImageFormat::Gif => ("gif".to_owned(), "image/gif"),
                image::ImageFormat::WebP => ("webp".to_owned(), "image/webp"),
                _ => return Err(anyhow!("unsupported image attachment format")),
            }
        } else {
            (extension(name), "application/octet-stream")
        };
        let digest = hex(digest::digest(&SHA256, &bytes).as_ref());
        let stable_key = format!("{message_id}:{index}:{digest}");
        let identity = serde_json::to_vec(&[thread_id, &stable_key])?;
        let identity = digest::digest(&SHA256, &identity);
        let uuid = uuid::Uuid::from_bytes(
            identity.as_ref()[..16]
                .try_into()
                .expect("SHA-256 has at least 16 bytes"),
        );
        let segment =
            thread_segment(thread_id).ok_or_else(|| anyhow!("invalid attachment owner"))?;
        let id = format!("{segment}-{uuid}-{extension}");
        if id.len() > 128
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(anyhow!("invalid attachment owner"));
        }
        agent_local_host::create_state_directory(&self.upload_directory)?;
        let target = self.upload_directory.join(format!("{id}.{extension}"));
        if target.try_exists()? {
            if read_bounded(&target, limit)? != bytes {
                return Err(anyhow!(
                    "attachment identity already contains different bytes"
                ));
            }
        } else {
            let mut temporary = tempfile::NamedTempFile::new_in(&self.upload_directory)?;
            temporary.write_all(&bytes)?;
            temporary.as_file().sync_all()?;
            match temporary.persist_noclobber(&target) {
                Ok(_) => {}
                Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if read_bounded(&target, limit)? != bytes {
                        return Err(anyhow!(
                            "attachment identity already contains different bytes"
                        ));
                    }
                }
                Err(error) => return Err(error.error.into()),
            }
        }
        Ok(
            serde_json::json!({"type":if is_image {"image"} else {"file"},"id":id,"name":name,"mimeType":mime,"sizeBytes":bytes.len()}),
        )
    }

    pub(crate) fn is_uploaded_attachment(&self, path: &str) -> bool {
        self.read_path(path)
            .ok()
            .and_then(|path| dunce::canonicalize(path).ok())
            .zip(dunce::canonicalize(&self.upload_directory).ok())
            .is_some_and(|(path, root)| path.parent() == Some(root.as_path()))
    }

    pub(crate) fn attachment_path(&self, id: &str) -> Result<PathBuf> {
        if id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(anyhow!("invalid attachment ID"));
        }
        // T3 IDs with an extension suffix name exactly one file. Native
        // image IDs without it use T3's small list of image extensions and .bin.
        let suffix = id
            .rsplit_once('-')
            .map(|(_, suffix)| suffix)
            .filter(|suffix| {
                !suffix.is_empty()
                    && suffix.len() <= 10
                    && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
                    && id
                        .get(..id.len() - suffix.len() - 1)
                        .and_then(|prefix| prefix.get(prefix.len().checked_sub(36)?..))
                        .is_some_and(|uuid| uuid::Uuid::parse_str(uuid).is_ok())
            });
        let extensions = suffix
            .map(|suffix| vec![suffix.to_ascii_lowercase()])
            .unwrap_or_else(|| {
                ["png", "jpg", "jpeg", "gif", "webp", "bin"]
                    .map(str::to_owned)
                    .to_vec()
            });
        let root = dunce::canonicalize(absolute_path(&self.upload_directory)?)?;
        for extension in extensions {
            let candidate = root.join(format!("{id}.{extension}"));
            if !candidate.try_exists()? {
                continue;
            }
            let path = dunce::canonicalize(&candidate)?;
            if path.parent() != Some(root.as_path()) || !path.is_file() {
                return Err(anyhow!(
                    "attachment is not a regular file in the attachment directory"
                ));
            }
            return Ok(path);
        }
        Err(std::io::Error::from(std::io::ErrorKind::NotFound).into())
    }

    pub(super) fn read_path(&self, value: &str) -> Result<PathBuf> {
        match value.strip_prefix(SCHEME) {
            Some(_) => {
                let name = agent_protocol::paths::attachment_file_name(value)
                    .ok_or_else(|| anyhow!("invalid attachment reference"))?;
                let (id, _) = name
                    .rsplit_once('.')
                    .expect("attachment filename has an extension");
                self.attachment_path(id)
            }
            None => absolute_path(&value),
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn thread_segment(thread_id: &str) -> Option<String> {
    let mut segment = String::new();
    for ch in thread_id.trim().chars().flat_map(char::to_lowercase) {
        let ch = if ch.is_ascii_alphanumeric() || ch == '_' {
            ch
        } else {
            '-'
        };
        if ch == '-' && segment.ends_with('-') {
            continue;
        }
        segment.push(ch);
    }
    let segment = segment.trim_matches(['-', '_']);
    let segment = &segment[..segment.len().min(80)];
    let segment = segment.trim_end_matches(['-', '_']);
    (!segment.is_empty()).then(|| {
        if segment == "pending" {
            "_pending".into()
        } else {
            segment.into()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_ids_resolve_without_a_path_in_message_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let files = WorkspaceFiles::new(directory.path().into());
        let id = "pending-3a7b8f9c-1952-4280-baf1-85985d194d37-png";
        let path = directory.path().join(format!("{id}.png"));
        fs::write(&path, b"bytes").unwrap();
        assert_eq!(
            files.read_path(&format!("{SCHEME}{id}.png")).unwrap(),
            dunce::canonicalize(&path).unwrap()
        );
        assert_eq!(files.read_path(path.to_str().unwrap()).unwrap(), path);
        for id in [
            "",
            "../outside",
            "id.png",
            "id/name",
            "%2e%2e",
            "id?key=value",
        ] {
            assert!(files.attachment_path(id).is_err(), "{id}");
        }
        assert!(files.attachment_path("missing").is_err());
    }

    #[test]
    fn uploads_use_t3_pending_ids_and_safe_extensions() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let name = pending_file_name(&file.reopen().unwrap(), "Original name.TXT").unwrap();
        assert!(name.starts_with("pending-"));
        assert!(name.ends_with("-txt.txt"));
        let uuid = &name[8..44];
        assert!(uuid::Uuid::parse_str(uuid).is_ok());
        assert_eq!(extension("upload.PART"), "bin");
        assert_eq!(extension("no extension"), "bin");
    }
}

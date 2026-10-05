//! Attachment limits and opaque asset paths shared by the Host and clients.
use crate::{Attachment, AttachmentKind};
use std::path::{Path, PathBuf};
pub fn native_image(mime: &str) -> bool {
    matches!(
        mime,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    )
}
pub fn validate(attachments: &[Attachment]) -> Result<(), String> {
    if attachments.len() > 100 {
        return Err("You can attach up to 100 files per message.".into());
    }
    let mut ids = std::collections::BTreeSet::new();
    let mut image_bytes = 0u64;
    for a in attachments {
        if !ids.insert(&a.id) {
            return Err("Duplicate attachment ids are not allowed.".into());
        }
        if (a.kind == AttachmentKind::Image) != native_image(&a.mime_type) {
            return Err("Attachment kind does not match its MIME type.".into());
        }
        if a.name.is_empty()
            || a.name.len() > 200
            || a.name.chars().any(char::is_control)
            || a.mime_type.is_empty()
            || a.mime_type.len() > 100
        {
            return Err("Invalid attachment metadata.".into());
        }
        let max = if a.kind == AttachmentKind::Image {
            10 * 1024 * 1024
        } else {
            50 * 1024 * 1024
        };
        if a.size_bytes == 0 || a.size_bytes > max {
            return Err("Attachment exceeds the size limit.".into());
        }
        if a.kind == AttachmentKind::Image || native_image(&a.mime_type) {
            image_bytes = image_bytes.saturating_add(a.size_bytes);
        }
    }
    if image_bytes > 80 * 1024 * 1024 {
        return Err("Images can total up to 80 MiB per message.".into());
    }
    Ok(())
}
pub fn path(root: &Path, id: &str) -> Option<PathBuf> {
    let parts: Vec<_> = id.split(':').collect();
    let safe = |s: &str| {
        !s.is_empty()
            && s.len() <= 128
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    };
    match parts.as_slice() {
        ["pending", token] if safe(token) => Some(root.join("pending").join(token)),
        ["chat", thread, token] if thread.len() == 43 && safe(thread) && safe(token) => {
            Some(root.join("chat").join(thread).join(token))
        }
        _ => None,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn limits_and_paths_reject_forged_or_oversized_assets() {
        let a = Attachment {
            id: "pending:abc".into(),
            kind: AttachmentKind::Image,
            name: "image.png".into(),
            mime_type: "image/png".into(),
            size_bytes: 10 * 1024 * 1024,
        };
        assert!(validate(std::slice::from_ref(&a)).is_ok());
        assert!(validate(&[a.clone(), a.clone()]).is_err());
        assert!(
            validate(&[Attachment {
                size_bytes: a.size_bytes + 1,
                ..a
            }])
            .is_err()
        );
        for id in [
            "/tmp/file",
            "pending:../../checkout",
            "chat:abc:file",
            "pending:.hidden",
            "pending:abc:extra",
        ] {
            assert!(path(Path::new("/assets"), id).is_none());
        }
    }
}

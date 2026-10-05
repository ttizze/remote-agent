use crate::error;
use orchestration::{AdapterError, Attachment, attachments};
use serde_json::{Value, json};
use std::path::Path;
pub(crate) fn text(text: &str, files: &[Attachment], root: &Path) -> Result<String, AdapterError> {
    attachments::validate(files).map_err(error)?;
    let mut result = text.to_owned();
    for a in files {
        let path = attachments::path(root, &a.id)
            .filter(|_| a.id.starts_with("chat:"))
            .ok_or_else(|| error("attachment has not been claimed"))?;
        let info =
            std::fs::symlink_metadata(&path).map_err(|_| error("attachment is unavailable"))?;
        if !info.is_file() || info.file_type().is_symlink() {
            return Err(error("attachment is not an owned regular file"));
        }
        let maximum = if attachments::native_image(&a.mime_type) {
            10
        } else {
            50
        } * 1024
            * 1024;
        if info.len() == 0 || info.len() > maximum {
            return Err(error("attachment exceeds the size limit"));
        }
        let addition = format!(
            "[Attached {} \"{}\" is saved at: {}]",
            if a.kind == orchestration::AttachmentKind::Image {
                "image"
            } else {
                "file"
            },
            a.name,
            path.display()
        );
        if result.chars().count() + addition.chars().count() + 2 <= 120000 {
            if !result.is_empty() {
                result.push_str("\n\n");
            }
            result.push_str(&addition);
        }
    }
    Ok(result)
}
pub(crate) fn codex_input(
    text: &str,
    files: &[Attachment],
    root: &Path,
) -> Result<Value, AdapterError> {
    let mut input =
        vec![json!({"type":"text","text":self::text(text,files,root)?,"text_elements":[]})];
    for a in files
        .iter()
        .filter(|a| attachments::native_image(&a.mime_type))
    {
        input.push(json!({"type":"localImage","path":attachments::path(root,&a.id).expect("validated attachment path")}));
    }
    Ok(json!(input))
}
pub(crate) async fn claude_content(
    text: &str,
    files: &[Attachment],
    root: &Path,
) -> Result<Value, AdapterError> {
    use base64::Engine as _;
    let mut input = vec![json!({"type":"text","text":self::text(text,files,root)?})];
    for a in files
        .iter()
        .filter(|a| attachments::native_image(&a.mime_type))
    {
        let path = attachments::path(root, &a.id).expect("validated attachment path");
        use tokio::io::AsyncReadExt as _;
        let mut data = Vec::new();
        tokio::fs::File::open(path)
            .await
            .map_err(|_| error("image attachment is unavailable"))?
            .take(10 * 1024 * 1024 + 1)
            .read_to_end(&mut data)
            .await
            .map_err(|_| error("image attachment is unavailable"))?;
        if data.len() > 10 * 1024 * 1024 {
            return Err(error("image attachment exceeds 10 MiB"));
        }
        input.push(json!({"type":"image","source":{"type":"base64","media_type":a.mime_type,"data":base64::engine::general_purpose::STANDARD.encode(data)}}));
    }
    Ok(json!(input))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn images_use_native_blocks_and_files_use_owned_paths() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("chat").join("a".repeat(43));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("image"), b"image").unwrap();
        std::fs::write(directory.join("file"), b"file").unwrap();
        let image = Attachment {
            id: format!("chat:{}:image", "a".repeat(43)),
            kind: orchestration::AttachmentKind::Image,
            name: "image.png".into(),
            mime_type: "image/png".into(),
            size_bytes: 5,
        };
        let file = Attachment {
            id: format!("chat:{}:file", "a".repeat(43)),
            kind: orchestration::AttachmentKind::File,
            name: "file.txt".into(),
            mime_type: "text/plain".into(),
            size_bytes: 4,
        };
        let files = [image, file];
        let codex = codex_input("inspect", &files, root.path()).unwrap();
        assert_eq!(codex.as_array().unwrap().len(), 2);
        assert_eq!(codex[1]["type"], "localImage");
        assert!(codex[0]["text"].as_str().unwrap().contains("file.txt"));
        let claude = claude_content("inspect", &files, root.path())
            .await
            .unwrap();
        assert_eq!(claude[1]["source"]["media_type"], "image/png");
        assert_eq!(claude[1]["source"]["data"], "aW1hZ2U=");
        let forged = Attachment {
            id: "/tmp/secret".into(),
            ..files[0].clone()
        };
        assert!(codex_input("", &[forged], root.path()).is_err());
    }
}

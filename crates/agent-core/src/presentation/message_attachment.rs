//! Typed user files keep their reference identity separate from display labels.
use agent_protocol::attribution::Author;
use serde::Serialize;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AttachmentKind {
    File,
    Video,
    Audio,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageAttachment {
    pub href: String,
    pub name: String,
    pub path: String,
    pub kind: AttachmentKind,
    pub extension: String,
    pub size_label: Option<String>,
    pub available: bool,
    pub icon: &'static str,
}

pub(crate) fn attachment_href(index: usize) -> String {
    // Context references use the v1 authority, irrespective of their kind or ID.
    format!("bex-context://attachment-v1/{index}")
}

pub(crate) fn attachment_reference(index: usize, name: &str) -> String {
    format!(
        "[{}]({})",
        crate::composer::context::literal(name),
        attachment_href(index)
    )
}

/// The pinned formatBytes rounds before selecting its 1024-based unit.
/// ECMAScript toFixed rounds positive ties upward, unlike Rust's formatting.
fn scaled(value: f64) -> (f64, usize) {
    let precision = if value >= 100.0 {
        0
    } else if value >= 10.0 {
        1
    } else {
        2
    };
    let factor = 10_f64.powi(precision as i32);
    ((value * factor).round() / factor, precision)
}

fn attachment_size(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let units = ["KB", "MB", "GB", "TB"];
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while scaled(value).0 >= 1024.0 && unit < units.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    let (value, precision) = scaled(value);
    format!("{value:.precision$} {}", units[unit])
}

pub(crate) fn attachment(
    index: usize,
    name: &str,
    path: &str,
    byte_length: Option<u64>,
    mime_type: Option<&str>,
) -> MessageAttachment {
    let guessed = super::files::file_preview_mime_type(name.into());
    let normalized = mime_type.map(|mime| {
        mime.split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
    });
    // The source treats OS-reported MPEG transport types as generic: .ts is
    // commonly TypeScript, and must not become a video player from that MIME.
    let mime = normalized
        .as_deref()
        .filter(|mime| {
            !matches!(
                *mime,
                "" | "application/octet-stream"
                    | "binary/octet-stream"
                    | "application/unknown"
                    | "video/mp2t"
                    | "video/vnd.dlna.mpeg-tts"
                    | "text/plain"
            )
        })
        .or(guessed.as_deref())
        .unwrap_or_default();
    let kind = if mime.starts_with("video/") {
        AttachmentKind::Video
    } else if mime.starts_with("audio/") {
        AttachmentKind::Audio
    } else {
        AttachmentKind::File
    };
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension)
        .filter(|extension| {
            (1..=8).contains(&extension.len())
                && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
        .map(str::to_ascii_uppercase)
        .unwrap_or_else(|| "File".into());
    MessageAttachment {
        href: attachment_href(index),
        name: name.into(),
        path: path.into(),
        kind,
        extension,
        size_label: byte_length.map(attachment_size),
        available: !crate::text::trim(path).is_empty(),
        icon: super::files::desktop_metadata_icon(name, mime_type),
    }
}

pub(crate) fn attachments_only_kind(
    author: Option<&Author>,
    text: &str,
    has_images: bool,
    has_contexts: bool,
    attachments: &[MessageAttachment],
) -> Option<AttachmentKind> {
    if matches!(author, Some(Author::Agent { .. }))
        || !crate::text::trim(text).is_empty()
        || has_contexts
    {
        return None;
    }
    let kind = attachments.first()?.kind;
    if has_images && kind != AttachmentKind::Video {
        return None;
    }
    attachments
        .iter()
        .all(|file| file.kind == kind)
        .then_some(kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_follow_the_pinned_javascript_rounding_and_unit_boundaries() {
        // Golden values from vendor/t3code/apps/web/src/lib/formatBytes.ts.
        for (bytes, expected) in [
            (0, "0 B"),
            (1023, "1023 B"),
            (1024, "1.00 KB"),
            (1152, "1.13 KB"),
            (10240, "10.0 KB"),
            (102400, "100 KB"),
            (1048166, "1.00 MB"),
        ] {
            assert_eq!(attachment_size(bytes), expected);
        }
    }

    #[test]
    fn cards_only_requires_ordinary_files_without_prose_context_or_other_media() {
        let files = [attachment(
            0,
            "report.pdf",
            "/uploads/report.pdf",
            None,
            None,
        )];
        assert_eq!(
            attachments_only_kind(None, "\u{feff}\t", false, false, &files),
            Some(AttachmentKind::File)
        );
        assert_eq!(
            attachments_only_kind(None, "Read this", false, false, &files),
            None
        );
        assert_eq!(attachments_only_kind(None, "", true, false, &files), None);
        assert_eq!(attachments_only_kind(None, "", false, true, &files), None);
        assert_eq!(attachments_only_kind(None, "", false, false, &[]), None);
        for (name, kind) in [
            ("clip.mp4", AttachmentKind::Video),
            ("voice.mp3", AttachmentKind::Audio),
        ] {
            let media = [attachment(0, name, "/uploads/media", None, None)];
            assert_eq!(
                attachments_only_kind(None, "", false, false, &media),
                Some(kind)
            );
            assert_eq!(
                attachments_only_kind(None, "", true, false, &media),
                (kind == AttachmentKind::Video).then_some(kind)
            );
        }
        let mixed = [
            attachment(0, "clip.mp4", "/uploads/clip", None, None),
            attachment(1, "voice.mp3", "/uploads/voice", None, None),
        ];
        assert_eq!(attachments_only_kind(None, "", false, false, &mixed), None);
        let agent = Author::Agent {
            session: agent_protocol::session::SessionRef::new(
                agent_protocol::session::ProviderKind::Codex,
                "agent".into(),
            )
            .unwrap(),
        };
        assert_eq!(
            attachments_only_kind(Some(&agent), "", false, false, &files),
            None
        );
    }

    #[test]
    fn metadata_does_not_invent_a_zero_size_or_change_a_files_target() {
        let pending = attachment(4, "Report.PDF", "/uploads/opaque", None, None);
        assert!(pending.size_label.is_none());
        assert_eq!(pending.extension, "PDF");
        let ready = attachment(
            4,
            "Report.PDF",
            "/uploads/opaque",
            Some(1280),
            Some("application/pdf"),
        );
        assert_eq!(ready.size_label.as_deref(), Some("1.25 KB"));
        assert_eq!(ready.href, pending.href);
        assert_eq!(ready.path, "/uploads/opaque");
    }

    #[test]
    fn media_classification_normalizes_mime_without_treating_typescript_as_video() {
        assert!(
            attachment(0, "index.ts", "/uploads/file", None, Some("video/mp2t")).kind
                == AttachmentKind::File
        );
        assert!(
            attachment(
                0,
                "opaque",
                "/uploads/file",
                None,
                Some(" VIDEO/MP4; codecs=avc1 ")
            )
            .kind
                == AttachmentKind::Video
        );
        assert!(
            attachment(
                0,
                "clip.mp4",
                "/uploads/file",
                None,
                Some("application/pdf")
            )
            .kind
                == AttachmentKind::File
        );
        assert!(
            attachment(
                0,
                "clip.mp4",
                "/uploads/file",
                None,
                Some("binary/octet-stream")
            )
            .kind
                == AttachmentKind::Video
        );
    }
}

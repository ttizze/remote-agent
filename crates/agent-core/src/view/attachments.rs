//! Attachment limits: which picked, pasted or dropped files a draft accepts,
//! and the messages for the ones it refuses.
use crate::state::{DraftAttachment, native_image};

/// Files one message or question response may carry.
pub const MAX_ATTACHMENTS: usize = 100;
/// Larger images are downscaled to fit before upload.
pub const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
const MAX_TOTAL_IMAGE_BYTES: u64 = 80 * 1024 * 1024;
pub const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;

/// A Host may advertise a lower file limit, never a higher one.
pub fn clamp_file_attachment_upload_bytes(advertised_max_upload_bytes: u64) -> u64 {
    advertised_max_upload_bytes.min(MAX_FILE_BYTES)
}

/// "3.2 MB" or "48 KB"; never "0 KB".
pub fn format_attachment_size(size_bytes: u64) -> String {
    if size_bytes >= MIB {
        let tenths = (size_bytes * 10 + MIB / 2) / MIB;
        format!("{}.{} MB", tenths / 10, tenths % 10)
    } else {
        format!("{} KB", size_bytes.div_ceil(1024).max(1))
    }
}

pub fn file_attachment_too_large_message(name: &str, max_upload_bytes: u64) -> String {
    let limit = if max_upload_bytes >= MIB && max_upload_bytes.is_multiple_of(MIB) {
        format!("{} MB", max_upload_bytes / MIB)
    } else if max_upload_bytes >= 1024 && max_upload_bytes.is_multiple_of(1024) {
        format!("{} KB", max_upload_bytes / 1024)
    } else {
        format!(
            "{max_upload_bytes} {}",
            if max_upload_bytes == 1 {
                "byte"
            } else {
                "bytes"
            }
        )
    };
    format!("'{name}' exceeds the {limit} attachment limit.")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum AttachmentFileKind {
    Image,
    File,
    /// An image type providers cannot read; refused rather than sent as a file.
    UnsupportedImage,
}

fn is_unknown_mime_type(mime_type: &str) -> bool {
    mime_type.is_empty() || mime_type.eq_ignore_ascii_case("application/octet-stream")
}

/// The image type a file name implies, for files that arrive without one.
pub fn infer_image_mime_type_from_name(name: &str) -> Option<&'static str> {
    let index = name.rfind('.').filter(|&index| index > 0)?;
    match name[index + 1..].to_lowercase().as_str() {
        "gif" => Some("image/gif"),
        "jpeg" | "jpg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

fn inferred_image_mime_type(name: &str, mime_type: &str) -> Option<&'static str> {
    is_unknown_mime_type(mime_type)
        .then(|| infer_image_mime_type_from_name(name))
        .flatten()
}

/// HEIC photos are converted to JPEG before upload.
pub fn is_heic_image(name: &str, mime_type: &str) -> bool {
    let mime = mime_type.to_ascii_lowercase();
    if mime == "image/heic" || mime == "image/heif" {
        return true;
    }
    let name = name.to_ascii_lowercase();
    is_unknown_mime_type(mime_type) && (name.ends_with(".heic") || name.ends_with(".heif"))
}

pub fn is_supported_image_mime_type(mime_type: &str) -> bool {
    native_image(&mime_type.to_ascii_lowercase())
}

pub fn classify_attachment_file(name: &str, mime_type: &str) -> AttachmentFileKind {
    if is_heic_image(name, mime_type) || inferred_image_mime_type(name, mime_type).is_some() {
        return AttachmentFileKind::Image;
    }
    if !mime_type.to_lowercase().starts_with("image/") {
        return AttachmentFileKind::File;
    }
    if is_supported_image_mime_type(mime_type) {
        AttachmentFileKind::Image
    } else {
        AttachmentFileKind::UnsupportedImage
    }
}

/// Gives an image recognized by its extension a concrete type.
pub fn normalize_image_mime_type(name: &str, mime_type: &str) -> String {
    inferred_image_mime_type(name, mime_type).map_or_else(|| mime_type.into(), str::to_owned)
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AttachmentCandidate {
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
}

/// Whether a paste attaches its files instead of inserting its text. Text
/// copied with a synthetic file stays text; images are always attached.
pub fn should_handle_attachment_paste(files: &[AttachmentCandidate], plain_text: &str) -> bool {
    let kinds: Vec<_> = files
        .iter()
        .map(|file| classify_attachment_file(&file.name, &file.mime_type))
        .collect();
    if kinds.iter().any(|kind| *kind != AttachmentFileKind::File) {
        return true;
    }
    plain_text.is_empty() && !kinds.is_empty()
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AdmittedAttachment {
    /// The candidate's position in the batch.
    pub index: u32,
    /// `Image` or `File`.
    pub kind: AttachmentFileKind,
    pub name: String,
    pub mime_type: String,
    /// Over the image limit: downscale before upload.
    pub needs_compression: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AttachmentAdmission {
    pub accepted: Vec<AdmittedAttachment>,
    /// The last refusal in the batch.
    pub error: Option<String>,
}

/// Which files of a batch join a draft that already holds `existing`. Every
/// file is checked so a later one can still report its own refusal.
pub fn admit_attachments(
    existing: &[DraftAttachment],
    candidates: &[AttachmentCandidate],
) -> AttachmentAdmission {
    let mut reserved = existing.len();
    let mut accepted = Vec::new();
    let mut error = None;
    for (index, file) in candidates.iter().enumerate() {
        if reserved >= MAX_ATTACHMENTS {
            error = Some(format!(
                "You can attach up to {MAX_ATTACHMENTS} files per message."
            ));
            continue;
        }
        let admitted = match classify_attachment_file(&file.name, &file.mime_type) {
            AttachmentFileKind::UnsupportedImage => Err(format!(
                "'{}' is not a supported image type. Attach GIF, HEIC, HEIF, JPEG, PNG, or WebP images.",
                file.name
            )),
            AttachmentFileKind::Image => Ok(AdmittedAttachment {
                index: index as u32,
                kind: AttachmentFileKind::Image,
                name: if file.name.is_empty() {
                    "image"
                } else {
                    &file.name
                }
                .into(),
                mime_type: normalize_image_mime_type(&file.name, &file.mime_type),
                needs_compression: file.size_bytes > MAX_IMAGE_BYTES,
            }),
            AttachmentFileKind::File if file.size_bytes == 0 => {
                Err(format!("'{}' is empty or could not be read.", file.name))
            }
            AttachmentFileKind::File if file.size_bytes > MAX_FILE_BYTES => Err(
                file_attachment_too_large_message(&file.name, MAX_FILE_BYTES),
            ),
            AttachmentFileKind::File => Ok(AdmittedAttachment {
                index: index as u32,
                kind: AttachmentFileKind::File,
                name: if file.name.is_empty() {
                    "file"
                } else {
                    &file.name
                }
                .into(),
                mime_type: if file.mime_type.is_empty() {
                    "application/octet-stream".into()
                } else {
                    file.mime_type.clone()
                },
                needs_compression: false,
            }),
        };
        match admitted {
            Ok(attachment) => {
                accepted.push(attachment);
                reserved += 1;
            }
            Err(message) => error = Some(message),
        }
    }
    AttachmentAdmission { accepted, error }
}

/// Why an image could not be prepared for upload.
pub fn image_preparation_error(name: &str, unreadable: bool) -> String {
    if unreadable {
        format!("'{name}' could not be read as an image.")
    } else {
        format!("'{name}' is too large to attach, even after compression.")
    }
}

fn is_image(attachment: &DraftAttachment) -> bool {
    attachment.kind == "image" || is_supported_image_mime_type(&attachment.mime_type)
}

/// Why the draft's attachments cannot send together.
pub fn attachment_limit_error(attachments: &[DraftAttachment]) -> Option<String> {
    if attachments.len() > MAX_ATTACHMENTS {
        return Some(format!(
            "You can attach up to {MAX_ATTACHMENTS} files per message or question response."
        ));
    }
    let image_bytes: u64 = attachments
        .iter()
        .filter(|attachment| is_image(attachment))
        .map(|attachment| attachment.size_bytes)
        .sum();
    (image_bytes > MAX_TOTAL_IMAGE_BYTES).then(|| {
        "Images can total up to 80 MiB per message or question response. Use smaller images or send fewer at once.".into()
    })
}

/// A kept file over the file limit blocks sending until it is removed.
pub fn file_attachment_block_reason(attachments: &[DraftAttachment]) -> Option<String> {
    attachments
        .iter()
        .find(|attachment| !is_image(attachment) && attachment.size_bytes > MAX_FILE_BYTES)
        .map(|attachment| file_attachment_too_large_message(&attachment.name, MAX_FILE_BYTES))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, mime_type: &str, size_bytes: u64) -> AttachmentCandidate {
        AttachmentCandidate {
            name: name.into(),
            mime_type: mime_type.into(),
            size_bytes,
        }
    }

    fn draft_attachment(id: &str, kind: &str, mime_type: &str, size_bytes: u64) -> DraftAttachment {
        DraftAttachment {
            id: id.into(),
            remote_id: None,
            name: format!("{id}.bin"),
            mime_type: mime_type.into(),
            kind: kind.into(),
            size_bytes,
            local_path: String::new(),
            status: "ready".into(),
            error: None,
        }
    }

    #[test]
    fn clamps_the_advertised_limit_to_the_turn_contract_cap() {
        assert_eq!(clamp_file_attachment_upload_bytes(1024), 1024);
        assert_eq!(
            clamp_file_attachment_upload_bytes(MAX_FILE_BYTES * 2),
            MAX_FILE_BYTES
        );
    }

    #[test]
    fn formats_attachment_row_sizes() {
        assert_eq!(format_attachment_size(3 * 1024 * 1024), "3.0 MB");
        assert_eq!(format_attachment_size(1), "1 KB");
        assert_eq!(format_attachment_size(0), "1 KB");
        assert_eq!(format_attachment_size(1025), "2 KB");
        assert_eq!(format_attachment_size(1_572_864), "1.5 MB");
        assert_eq!(format_attachment_size(1024 * 1024 + 256 * 1024), "1.3 MB");
    }

    #[test]
    fn formats_small_upload_limits_without_rounding_them_to_zero_mb() {
        assert_eq!(
            file_attachment_too_large_message("tiny.txt", 1),
            "'tiny.txt' exceeds the 1 byte attachment limit."
        );
        assert_eq!(
            file_attachment_too_large_message("small.txt", 1024),
            "'small.txt' exceeds the 1 KB attachment limit."
        );
        assert_eq!(
            file_attachment_too_large_message("exact.txt", 1025),
            "'exact.txt' exceeds the 1025 bytes attachment limit."
        );
        assert_eq!(
            file_attachment_too_large_message("medium.zip", 512 * 1024),
            "'medium.zip' exceeds the 512 KB attachment limit."
        );
    }

    #[test]
    fn keeps_whole_mb_upload_limits_for_standard_server_caps() {
        assert_eq!(
            file_attachment_too_large_message("one.bin", 1024 * 1024),
            "'one.bin' exceeds the 1 MB attachment limit."
        );
        assert_eq!(
            file_attachment_too_large_message("big.zip", 50 * 1024 * 1024),
            "'big.zip' exceeds the 50 MB attachment limit."
        );
    }

    #[test]
    fn keeps_supported_images_and_heic_photos_on_the_image_path() {
        assert_eq!(
            classify_attachment_file("photo.png", "image/png"),
            AttachmentFileKind::Image
        );
        assert_eq!(
            classify_attachment_file("photo.heic", ""),
            AttachmentFileKind::Image
        );
    }

    #[test]
    fn rejects_unsupported_image_types_instead_of_attaching_them_as_generic_files() {
        assert_eq!(
            classify_attachment_file("diagram.svg", "image/svg+xml"),
            AttachmentFileKind::UnsupportedImage
        );
        assert_eq!(
            classify_attachment_file("photo.tiff", "image/tiff"),
            AttachmentFileKind::UnsupportedImage
        );
        assert_eq!(
            classify_attachment_file("report.pdf", "application/pdf"),
            AttachmentFileKind::File
        );
    }

    #[test]
    fn preserves_text_paste_when_an_application_adds_a_synthetic_generic_file() {
        assert!(!should_handle_attachment_paste(
            &[file("clipboard.rtf", "application/rtf", 9)],
            "Copied text"
        ));
    }

    #[test]
    fn claims_unsupported_image_pastes_so_the_composer_can_report_them() {
        for image in [
            file("diagram.svg", "image/svg+xml", 3),
            file("photo.tiff", "image/tiff", 4),
        ] {
            assert!(should_handle_attachment_paste(&[image], "Image caption"));
        }
    }

    #[test]
    fn claims_generic_file_only_pastes_so_the_composer_can_report_validation_errors() {
        assert!(should_handle_attachment_paste(
            &[file("report.pdf", "application/pdf", 6)],
            ""
        ));
    }

    #[test]
    fn routes_empty_and_oversized_generic_files_to_composer_feedback() {
        assert!(should_handle_attachment_paste(
            &[file("empty.txt", "text/plain", 0)],
            ""
        ));
        assert!(should_handle_attachment_paste(
            &[file("large.zip", "application/zip", 1024)],
            ""
        ));
    }

    #[test]
    fn ignores_an_empty_clipboard() {
        assert!(!should_handle_attachment_paste(&[], ""));
    }

    #[test]
    fn claims_image_pastes_even_when_clipboard_text_is_present() {
        assert!(should_handle_attachment_paste(
            &[file("photo.heic", "image/heic", 5)],
            "Image caption"
        ));
    }

    #[test]
    fn falls_back_to_the_extension_when_an_image_arrives_without_a_mime_type() {
        assert_eq!(
            classify_attachment_file("photo.jpg", ""),
            AttachmentFileKind::Image
        );
        assert_eq!(
            classify_attachment_file("shot.PNG", ""),
            AttachmentFileKind::Image
        );
        assert_eq!(
            classify_attachment_file("archive.zip", ""),
            AttachmentFileKind::File
        );
        assert_eq!(
            classify_attachment_file("no-extension", ""),
            AttachmentFileKind::File
        );
        assert_eq!(
            infer_image_mime_type_from_name("photo.jpg"),
            Some("image/jpeg")
        );
        assert_eq!(infer_image_mime_type_from_name("archive.zip"), None);
    }

    #[test]
    fn infers_supported_image_types_from_octet_stream_files() {
        let octet = "application/octet-stream";
        assert_eq!(
            classify_attachment_file("photo.jpg", octet),
            AttachmentFileKind::Image
        );
        assert_eq!(
            classify_attachment_file("shot.PNG", octet),
            AttachmentFileKind::Image
        );
        assert_eq!(normalize_image_mime_type("photo.jpg", octet), "image/jpeg");
        assert_eq!(normalize_image_mime_type("shot.PNG", octet), "image/png");
    }

    #[test]
    fn does_not_infer_images_for_unknown_extensions_or_specific_conflicting_mime_types() {
        let octet = "application/octet-stream";
        assert_eq!(
            classify_attachment_file("archive.bin", octet),
            AttachmentFileKind::File
        );
        assert_eq!(
            classify_attachment_file("report.pdf", octet),
            AttachmentFileKind::File
        );
        assert_eq!(
            classify_attachment_file("photo.jpg", "application/pdf"),
            AttachmentFileKind::File
        );
        assert_eq!(
            classify_attachment_file("photo.jpg", "image/png"),
            AttachmentFileKind::Image
        );
        assert_eq!(normalize_image_mime_type("archive.bin", octet), octet);
        assert_eq!(
            normalize_image_mime_type("photo.jpg", "application/pdf"),
            "application/pdf"
        );
        assert_eq!(
            normalize_image_mime_type("photo.jpg", "image/png"),
            "image/png"
        );
    }

    #[test]
    fn admits_files_up_to_the_draft_limit_and_reports_each_refusal() {
        let admission = admit_attachments(
            &[],
            &[
                file("photo.jpg", "", 11 * 1024 * 1024),
                file("diagram.svg", "image/svg+xml", 3),
                file("notes.txt", "text/plain", 12),
                file("empty.txt", "text/plain", 0),
            ],
        );
        assert_eq!(
            admission.accepted,
            [
                AdmittedAttachment {
                    index: 0,
                    kind: AttachmentFileKind::Image,
                    name: "photo.jpg".into(),
                    mime_type: "image/jpeg".into(),
                    needs_compression: true,
                },
                AdmittedAttachment {
                    index: 2,
                    kind: AttachmentFileKind::File,
                    name: "notes.txt".into(),
                    mime_type: "text/plain".into(),
                    needs_compression: false,
                },
            ]
        );
        assert_eq!(
            admission.error.as_deref(),
            Some("'empty.txt' is empty or could not be read.")
        );
        assert_eq!(
            admit_attachments(&[], &[file("diagram.svg", "image/svg+xml", 3)]).error,
            Some(
                "'diagram.svg' is not a supported image type. Attach GIF, HEIC, HEIF, JPEG, PNG, or WebP images."
                    .into()
            )
        );
        assert_eq!(
            admit_attachments(&[], &[file("big.zip", "", MAX_FILE_BYTES + 1)]).error,
            Some("'big.zip' exceeds the 50 MB attachment limit.".into())
        );
        assert_eq!(
            admit_attachments(&[], &[file("", "", 4)]).accepted[0].mime_type,
            "application/octet-stream"
        );
    }

    #[test]
    fn a_full_draft_refuses_more_files() {
        let existing: Vec<_> = (0..MAX_ATTACHMENTS - 1)
            .map(|index| draft_attachment(&index.to_string(), "file", "text/plain", 1))
            .collect();
        let admission = admit_attachments(
            &existing,
            &[
                file("a.txt", "text/plain", 1),
                file("b.txt", "text/plain", 1),
            ],
        );
        assert_eq!(admission.accepted.len(), 1);
        assert_eq!(
            admission.error.as_deref(),
            Some("You can attach up to 100 files per message.")
        );
    }

    #[test]
    fn image_preparation_failures_name_their_cause() {
        assert_eq!(
            image_preparation_error("scan.png", true),
            "'scan.png' could not be read as an image."
        );
        assert_eq!(
            image_preparation_error("scan.png", false),
            "'scan.png' is too large to attach, even after compression."
        );
    }

    #[test]
    fn a_draft_over_the_send_limits_cannot_send() {
        let images: Vec<_> = (0..9)
            .map(|index| draft_attachment(&index.to_string(), "image", "image/png", 10 * MIB))
            .collect();
        assert_eq!(
            attachment_limit_error(&images).as_deref(),
            Some(
                "Images can total up to 80 MiB per message or question response. Use smaller images or send fewer at once."
            )
        );
        assert_eq!(attachment_limit_error(&images[..8]), None);
        let many: Vec<_> = (0..=MAX_ATTACHMENTS)
            .map(|index| draft_attachment(&index.to_string(), "file", "text/plain", 1))
            .collect();
        assert_eq!(
            attachment_limit_error(&many).as_deref(),
            Some("You can attach up to 100 files per message or question response.")
        );
        let large = draft_attachment("large", "file", "application/zip", MAX_FILE_BYTES + 1);
        assert_eq!(
            file_attachment_block_reason(&[large]).as_deref(),
            Some("'large.bin' exceeds the 50 MB attachment limit.")
        );
        assert_eq!(file_attachment_block_reason(&images), None);
    }

    proptest::proptest! {
        #[test]
        fn admission_never_overfills_a_draft(existing in 0usize..=MAX_ATTACHMENTS, batch in 0usize..8) {
            let current: Vec<_> = (0..existing)
                .map(|index| draft_attachment(&index.to_string(), "file", "text/plain", 1))
                .collect();
            let files: Vec<_> = (0..batch).map(|_| file("a.txt", "text/plain", 1)).collect();
            let admission = admit_attachments(&current, &files);
            proptest::prop_assert_eq!(
                admission.accepted.len(),
                batch.min(MAX_ATTACHMENTS - existing)
            );
        }
    }
}

//! Files entering a draft (picked, pasted or dropped), the image re-encoding
//! the composer and the stash need, and local copies of sent images.
use super::{Desktop, Update as AppUpdate};
use agent_core::{
    state::{Intent, LocalFile},
    view::{
        attachments::{
            AttachmentCandidate, MAX_IMAGE_BYTES, admit_attachments,
            infer_image_mime_type_from_name, is_heic_image,
        },
        composer::stash::{StashImage, StashImages},
    },
};
use base64::Engine;
use gpui_kit::*;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

pub(crate) enum Update {
    Staged {
        draft_key: String,
        files: Vec<LocalFile>,
        error: Option<String>,
        snapshot_feedback: Option<(bool, bool)>,
        snapshot_id: Option<String>,
    },
    Downloaded {
        id: String,
        result: Result<PathBuf, String>,
    },
}

/// Local copies of Host attachments, by attachment id; `None` while one
/// downloads.
pub(crate) struct AttachmentCache {
    directory: Arc<tempfile::TempDir>,
    images: BTreeMap<String, Option<PathBuf>>,
}
impl AttachmentCache {
    pub(crate) fn new() -> Self {
        Self {
            directory: Arc::new(tempfile::tempdir().expect("attachment cache directory")),
            images: BTreeMap::new(),
        }
    }
    pub(crate) fn clear(&mut self) {
        self.images.clear();
    }
}

impl Desktop {
    /// Opens the file picker and attaches the chosen files to `draft_key`.
    pub(crate) fn pick_attachments(&self, draft_key: String) {
        let existing = self.draft_attachments(&draft_key);
        self.stage(draft_key, None, None, move || {
            rfd::FileDialog::new()
                .pick_files()
                .map(|paths| stage_paths(paths, &existing))
                .unwrap_or_else(|| Ok((vec![], None)))
        });
    }

    /// Attaches dropped files to `draft_key`.
    pub(crate) fn attach_paths(&self, draft_key: String, paths: Vec<PathBuf>) {
        let existing = self.draft_attachments(&draft_key);
        self.stage(draft_key, None, None, move || stage_paths(paths, &existing));
    }

    /// Captures the desktop through the platform bridge and sends its PNG
    /// through the normal image admission and Host upload path.
    pub(crate) fn capture_snapshot(&self, draft_key: String) {
        let existing = self.draft_attachments(&draft_key);
        let settings = self.snapshot.preferences.snapshot_capture.clone();
        let feedback = Some((settings.flash, settings.animations));
        self.stage_async(draft_key, feedback, None, async move {
            let path = staging_directory()?.join(format!("Snapshot-{}.png", uuid::Uuid::new_v4()));
            let permission =
                crate::platform::snapshot_permission_granted(settings.include_accessibility).await;
            if !agent_core::view::snapshot_capture::capture_is_allowed(&settings, permission) {
                return Err("Screen capture permission is required for this setting.".into());
            }
            crate::platform::capture_snapshot(&path, settings.include_accessibility).await?;
            if settings.play_sound {
                let _ = crate::platform::play_snapshot_sound(settings.sound).await;
            }
            tokio::task::spawn_blocking(move || stage_paths(vec![path], &existing))
                .await
                .map_err(|error| error.to_string())?
        });
    }

    /// Imports one capture produced by a native desktop helper. The helper
    /// owns the queue files until this staging operation succeeds.
    pub(crate) fn attach_external_snapshot(
        &self,
        draft_key: String,
        snapshot: crate::platform::PendingSnapshot,
    ) {
        let existing = self.draft_attachments(&draft_key);
        let snapshot_id = snapshot.id.clone();
        self.stage(draft_key, None, Some(snapshot_id), move || {
            let source_directory = staging_directory()?;
            let result = (|| {
                let source = source_directory.join(&snapshot.name);
                std::fs::copy(&snapshot.path, &source)
                    .map_err(|error| format!("snapshot image could not be staged: {error}"))?;
                let metadata_path = crate::platform::snapshot_metadata_path(&source);
                let metadata = serde_json::to_vec(&snapshot.source)
                    .map_err(|error| format!("snapshot metadata could not be encoded: {error}"))?;
                std::fs::write(&metadata_path, metadata)
                    .map_err(|error| format!("snapshot metadata could not be staged: {error}"))?;
                stage_paths(vec![source], &existing)
            })();
            let _ = std::fs::remove_dir_all(source_directory);
            result
        });
    }

    /// Attaches the files or image on the clipboard; false when it holds
    /// neither, so the text pastes as usual.
    pub(crate) fn paste_attachments(&self, draft_key: String, item: ClipboardItem) -> bool {
        if !item.entries().iter().any(|entry| {
            matches!(
                entry,
                ClipboardEntry::Image(_) | ClipboardEntry::ExternalPaths(_)
            )
        }) {
            return false;
        }
        let existing = self.draft_attachments(&draft_key);
        self.stage(draft_key, None, None, move || {
            stage_clipboard(item, &existing)
        });
        true
    }

    /// The local copy of a Host image attachment, downloading it the first
    /// time it is asked for.
    pub(crate) fn attachment_image(&mut self, attachment_id: &str) -> Option<PathBuf> {
        if let Some(path) = self.attachments.images.get(attachment_id) {
            return path.clone();
        }
        let store = self.store()?;
        self.attachments
            .images
            .insert(attachment_id.to_owned(), None);
        let path = self
            .attachments
            .directory
            .path()
            .join(uuid::Uuid::new_v4().to_string());
        let id = attachment_id.to_owned();
        let updates = self.updates.clone();
        let epoch = self.epoch;
        self.runtime.handle.spawn(async move {
            let result = store
                .download_attachment(id.clone(), path.to_string_lossy().into_owned())
                .await
                .map(|_| path)
                .map_err(|error| error.to_string());
            let _ = updates
                .send((
                    epoch,
                    AppUpdate::Attachments(Update::Downloaded { id, result }),
                ))
                .await;
        });
        None
    }

    /// Saves a Host attachment where the user chooses.
    pub(crate) fn save_attachment(&self, attachment_id: String, name: String) {
        let Some(store) = self.store() else {
            return;
        };
        self.spawn_task(
            async move {
                let Ok(Some(path)) = tokio::task::spawn_blocking(move || {
                    rfd::FileDialog::new().set_file_name(name).save_file()
                })
                .await
                else {
                    return Ok(());
                };
                store
                    .download_attachment(attachment_id, path.to_string_lossy().into_owned())
                    .await
                    .map_err(|error| error.to_string())
            },
            |view, result, window, cx| {
                if let Err(error) = result {
                    view.show_error(&error, window, cx);
                }
            },
        );
    }

    pub(super) fn attachments_update(
        &mut self,
        update: Update,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match update {
            Update::Staged {
                draft_key,
                files,
                error,
                snapshot_feedback,
                snapshot_id,
            } => {
                let succeeded = error.is_none();
                if let Some(error) = error {
                    self.show_error(&error, window, cx);
                }
                if !files.is_empty() {
                    self.perform(Intent::AttachFiles { draft_key, files });
                }
                if succeeded && let Some((flash, animations)) = snapshot_feedback {
                    self.snapshot_feedback(flash, animations, cx);
                }
                if let Some(id) = snapshot_id {
                    if succeeded {
                        if let Err(error) = crate::platform::acknowledge_snapshot(&id) {
                            self.show_error(&error, window, cx);
                        }
                    }
                    self.external_snapshot_ids.remove(&id);
                }
            }
            Update::Downloaded { id, result } => match result {
                Ok(path) => {
                    self.attachments.images.insert(id, Some(path));
                }
                Err(error) => self.show_error(&error, window, cx),
            },
        }
        cx.notify();
    }

    fn draft_attachments(&self, draft_key: &str) -> Vec<agent_core::state::DraftAttachment> {
        self.snapshot
            .drafts
            .get(draft_key)
            .map(|draft| draft.attachments.clone())
            .unwrap_or_else(|| {
                if draft_key == self.snapshot.draft_key() {
                    self.snapshot.current_draft().attachments
                } else {
                    vec![]
                }
            })
    }

    fn stage(
        &self,
        draft_key: String,
        snapshot_feedback: Option<(bool, bool)>,
        snapshot_id: Option<String>,
        work: impl FnOnce() -> Result<(Vec<LocalFile>, Option<String>), String> + Send + 'static,
    ) {
        self.stage_async(draft_key, snapshot_feedback, snapshot_id, async move {
            tokio::task::spawn_blocking(work)
                .await
                .map_err(|error| error.to_string())?
        });
    }

    fn stage_async(
        &self,
        draft_key: String,
        snapshot_feedback: Option<(bool, bool)>,
        snapshot_id: Option<String>,
        work: impl std::future::Future<Output = Result<(Vec<LocalFile>, Option<String>), String>>
        + Send
        + 'static,
    ) {
        let updates = self.updates.clone();
        let epoch = self.epoch;
        self.runtime.handle.spawn(async move {
            let (files, error) = match work.await {
                Ok(staged) => staged,
                Err(error) => (vec![], Some(error)),
            };
            let _ = updates
                .send((
                    epoch,
                    AppUpdate::Attachments(Update::Staged {
                        draft_key,
                        files,
                        error,
                        snapshot_feedback,
                        snapshot_id,
                    }),
                ))
                .await;
        });
    }
}

impl Desktop {
    fn snapshot_feedback(&mut self, flash: bool, animations: bool, cx: &mut Context<Self>) {
        if flash {
            let duration = std::time::Duration::from_millis(220);
            self.snapshot_feedback_until = Some(std::time::Instant::now() + duration);
            self.snapshot_feedback_id = self.snapshot_feedback_id.wrapping_add(1);
            self.snapshot_feedback_animated = animations;
            let id = self.snapshot_feedback_id;
            cx.spawn(async move |view, cx| {
                cx.background_executor().timer(duration).await;
                let _ = view.update(cx, |view, cx| {
                    if view.snapshot_feedback_id == id {
                        view.snapshot_feedback_until = None;
                        cx.notify();
                    }
                });
            })
            .detach();
            cx.notify();
        }
    }
}

/// A file's type from its name: images by extension, common text formats,
/// otherwise opaque bytes.
pub(crate) fn mime_type(name: &str) -> String {
    if let Some(mime) = infer_image_mime_type_from_name(name) {
        return mime.into();
    }
    let extension = Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match extension.as_str() {
        "txt" | "log" => "text/plain",
        "md" | "markdown" => "text/markdown",
        "csv" => "text/csv",
        "json" => "application/json",
        "pdf" => "application/pdf",
        "html" | "htm" => "text/html",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        _ => "application/octet-stream",
    }
    .into()
}

fn staging_directory() -> Result<PathBuf, String> {
    let path = std::env::temp_dir()
        .join("desktop-draft-attachments")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&path).map_err(|error| error.to_string())?;
    Ok(path)
}

/// Copies the files the draft admits, downscaling images over the provider
/// limit as the composer does, and reports the last refusal.
fn stage_paths(
    paths: Vec<PathBuf>,
    existing: &[agent_core::state::DraftAttachment],
) -> Result<(Vec<LocalFile>, Option<String>), String> {
    let mut sources = vec![];
    let mut candidates = vec![];
    let mut error = None;
    for path in paths {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let name = name.to_owned();
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => {
                let mime_type = mime_type(&name);
                candidates.push(AttachmentCandidate {
                    name: name.clone(),
                    mime_type,
                    size_bytes: metadata.len(),
                });
                sources.push(path);
            }
            _ => error = Some(format!("'{name}' is empty or could not be read.")),
        }
    }
    let admission = admit_attachments(existing, &candidates);
    if admission.error.is_some() {
        error = admission.error;
    }
    let mut files = vec![];
    for admitted in admission.accepted {
        let source = &sources[admitted.index as usize];
        let heic = is_heic_image(&admitted.name, &admitted.mime_type);
        let staged = if admitted.needs_compression || heic {
            match compress_image_file(source, &admitted.name, MAX_IMAGE_BYTES) {
                Ok(file) => file,
                Err(reason) => {
                    error = Some(agent_core::view::attachments::image_preparation_error(
                        &admitted.name,
                        reason == CompressionFailure::Unreadable,
                    ));
                    continue;
                }
            }
        } else {
            let path = staging_directory()?.join(&admitted.name);
            std::fs::copy(source, &path).map_err(|error| error.to_string())?;
            LocalFile {
                path: path.to_string_lossy().into_owned(),
                name: admitted.name,
                mime_type: admitted.mime_type,
            }
        };
        copy_snapshot_metadata(source, Path::new(&staged.path))?;
        files.push(staged);
    }
    Ok((files, error))
}

fn copy_snapshot_metadata(source: &Path, target: &Path) -> Result<(), String> {
    let source_metadata = crate::platform::snapshot_metadata_path(source);
    let bytes = match std::fs::read(&source_metadata) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!("snapshot metadata could not be read: {error}"));
        }
    };
    if bytes.len() > 128 * 1024 {
        return Err("snapshot metadata is too large".into());
    }
    serde_json::from_slice::<agent_domain::CapturedWindow>(&bytes)
        .map_err(|error| format!("snapshot metadata is invalid: {error}"))?;
    std::fs::write(crate::platform::snapshot_metadata_path(target), bytes)
        .map_err(|error| format!("snapshot metadata could not be copied: {error}"))
}

fn stage_clipboard(
    item: ClipboardItem,
    existing: &[agent_core::state::DraftAttachment],
) -> Result<(Vec<LocalFile>, Option<String>), String> {
    let mut paths = vec![];
    for entry in item.into_entries() {
        match entry {
            ClipboardEntry::ExternalPaths(files) => paths.extend(files.paths().to_vec()),
            ClipboardEntry::Image(image) => {
                let path = staging_directory()?.join(format!(
                    "Image.{}",
                    match image.format {
                        ImageFormat::Jpeg => "jpg",
                        ImageFormat::Gif => "gif",
                        ImageFormat::Webp => "webp",
                        _ => "png",
                    }
                ));
                std::fs::write(&path, &image.bytes).map_err(|error| error.to_string())?;
                paths.push(path);
            }
            _ => {}
        }
    }
    stage_paths(paths, existing)
}

/// Longest edge kept when an image is re-encoded.
const MAX_DIMENSION: u32 = 2048;
/// Beyond this the source is refused rather than decoded.
const MAX_COMPRESSIBLE_SOURCE_BYTES: u64 = 50 * 1024 * 1024;
/// JPEG qualities tried in order until the image fits.
const QUALITY_STEPS: [u8; 4] = [92, 85, 78, 68];
/// Extra downscales when even the lowest quality overflows.
const FALLBACK_SCALE_STEPS: [f32; 2] = [0.75, 0.55];
/// The base64 budget of one stashed image.
const MAX_STASH_IMAGE_DATA_URL_CHARS: usize = 1_300_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompressionFailure {
    TooLarge,
    Unreadable,
}

fn decode(path: &Path) -> Result<image::DynamicImage, CompressionFailure> {
    let size = std::fs::metadata(path)
        .map_err(|_| CompressionFailure::Unreadable)?
        .len();
    if size > MAX_COMPRESSIBLE_SOURCE_BYTES {
        return Err(CompressionFailure::TooLarge);
    }
    let mut reader = image::ImageReader::open(path)
        .map_err(|_| CompressionFailure::Unreadable)?
        .with_guessed_format()
        .map_err(|_| CompressionFailure::Unreadable)?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(512 * 1024 * 1024);
    reader.limits(limits);
    reader.decode().map_err(|_| CompressionFailure::Unreadable)
}

/// Re-encodes as JPEG within `limit` bytes: fit within the longest edge, then
/// step the quality down, then the size.
fn encode_within(image: &image::DynamicImage, limit: usize) -> Result<Vec<u8>, CompressionFailure> {
    let fitted = if image.width().max(image.height()) > MAX_DIMENSION {
        image.resize(
            MAX_DIMENSION,
            MAX_DIMENSION,
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        image.clone()
    };
    let encode = |image: &image::DynamicImage, quality: u8| {
        let mut bytes = vec![];
        let rgb = image::DynamicImage::ImageRgb8(image.to_rgb8());
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, quality)
            .encode_image(&rgb)
            .map(|_| bytes)
            .map_err(|_| CompressionFailure::Unreadable)
    };
    let mut last = None;
    for quality in QUALITY_STEPS {
        let bytes = encode(&fitted, quality)?;
        if bytes.len() <= limit {
            return Ok(bytes);
        }
        last = Some(quality);
    }
    let quality = last.unwrap_or(QUALITY_STEPS[QUALITY_STEPS.len() - 1]);
    for scale in FALLBACK_SCALE_STEPS {
        let width = ((fitted.width() as f32) * scale).max(1.) as u32;
        let height = ((fitted.height() as f32) * scale).max(1.) as u32;
        let smaller = fitted.resize(width, height, image::imageops::FilterType::Lanczos3);
        let bytes = encode(&smaller, quality)?;
        if bytes.len() <= limit {
            return Ok(bytes);
        }
    }
    Err(CompressionFailure::TooLarge)
}

fn jpeg_name(name: &str) -> String {
    let stem = Path::new(name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Image");
    format!("{stem}.jpg")
}

/// An image re-encoded to fit `limit` bytes, written beside the staged files.
pub(crate) fn compress_image_file(
    source: &Path,
    name: &str,
    limit: u64,
) -> Result<LocalFile, CompressionFailure> {
    let image = decode(source)?;
    let bytes = encode_within(&image, limit as usize)?;
    let name = jpeg_name(name);
    let path = staging_directory()
        .map_err(|_| CompressionFailure::Unreadable)?
        .join(&name);
    std::fs::write(&path, bytes).map_err(|_| CompressionFailure::Unreadable)?;
    Ok(LocalFile {
        path: path.to_string_lossy().into_owned(),
        name,
        mime_type: "image/jpeg".into(),
    })
}

/// One image of the draft as the stash stores it: the original when it fits
/// the budget, otherwise re-encoded.
fn stash_image(
    id: &str,
    name: &str,
    mime_type: &str,
    path: &Path,
) -> Result<StashImage, CompressionFailure> {
    let bytes = std::fs::read(path).map_err(|_| CompressionFailure::Unreadable)?;
    let original = format!(
        "data:{mime_type};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    );
    if original.len() <= MAX_STASH_IMAGE_DATA_URL_CHARS && !is_heic_image(name, mime_type) {
        return Ok(StashImage {
            id: id.into(),
            name: name.into(),
            mime_type: mime_type.into(),
            size_bytes: bytes.len() as u64,
            data_url: original,
        });
    }
    let image = decode(path)?;
    let encoded = encode_within(&image, MAX_STASH_IMAGE_DATA_URL_CHARS / 4 * 3)?;
    Ok(StashImage {
        id: id.into(),
        name: jpeg_name(name),
        mime_type: "image/jpeg".into(),
        size_bytes: encoded.len() as u64,
        data_url: format!(
            "data:image/jpeg;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&encoded)
        ),
    })
}

/// Encodes a stashed draft's images: `(id, name, mime type, local path)`.
pub(crate) fn encode_stash_images(images: Vec<(String, String, String, PathBuf)>) -> StashImages {
    let mut result = StashImages {
        images: vec![],
        dropped_image_names: vec![],
        unreadable_image_names: vec![],
    };
    for (id, name, mime_type, path) in images {
        match stash_image(&id, &name, &mime_type, &path) {
            Ok(image) => result.images.push(image),
            Err(CompressionFailure::TooLarge) => result.dropped_image_names.push(name),
            Err(CompressionFailure::Unreadable) => result.unreadable_image_names.push(name),
        }
    }
    result
}

/// Writes restored stash images back to files the draft can attach.
pub(crate) fn stash_images_to_files(images: &[StashImage]) -> Vec<LocalFile> {
    images
        .iter()
        .filter_map(|image| {
            let payload = image.data_url.split_once(',')?.1;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(payload)
                .ok()?;
            let path = staging_directory().ok()?.join(&image.name);
            std::fs::write(&path, bytes).ok()?;
            Some(LocalFile {
                path: path.to_string_lossy().into_owned(),
                name: image.name.clone(),
                mime_type: image.mime_type.clone(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_DIMENSION, compress_image_file, copy_snapshot_metadata, encode_stash_images, mime_type,
        stash_images_to_files,
    };
    use std::path::Path;

    fn png(path: &Path, width: u32, height: u32) {
        image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([(x % 251) as u8, (y % 241) as u8, ((x * y) % 239) as u8])
        })
        .save(path)
        .unwrap();
    }

    #[test]
    fn an_oversized_image_is_reencoded_within_the_limit_and_the_longest_edge() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("shot.png");
        png(&source, 3000, 1000);
        let file = compress_image_file(&source, "shot.png", 400_000).unwrap();
        assert_eq!(file.name, "shot.jpg");
        assert_eq!(file.mime_type, "image/jpeg");
        assert!(std::fs::metadata(&file.path).unwrap().len() <= 400_000);
        let decoded = image::open(&file.path).unwrap();
        assert_eq!(decoded.width(), MAX_DIMENSION);
    }

    #[test]
    fn small_stash_images_round_trip_and_missing_ones_are_named() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("small.png");
        png(&source, 20, 10);
        let missing = directory.path().join("missing.png");
        let encoded = encode_stash_images(vec![
            ("a".into(), "small.png".into(), "image/png".into(), source),
            (
                "b".into(),
                "missing.png".into(),
                "image/png".into(),
                missing,
            ),
        ]);
        assert_eq!(encoded.images.len(), 1);
        assert_eq!(encoded.unreadable_image_names, ["missing.png"]);
        let files = stash_images_to_files(&encoded.images);
        assert_eq!(files[0].name, "small.png");
        assert_eq!(
            image::open(&files[0].path).unwrap().width(),
            20,
            "a small image is kept as it was"
        );
    }

    #[test]
    fn mime_types_follow_the_extension() {
        assert_eq!(mime_type("a.PNG"), "image/png");
        assert_eq!(mime_type("notes.md"), "text/markdown");
        assert_eq!(mime_type("archive.tar.gz"), "application/octet-stream");
    }

    #[test]
    fn snapshot_metadata_copy_reports_invalid_and_io_failures() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.png");
        let target = directory.path().join("target.png");
        let metadata = agent_domain::CapturedWindow {
            app_name: "Editor".into(),
            window_title: "Draft".into(),
            accessible_text: None,
            accessibility: None,
        };
        std::fs::write(
            crate::platform::snapshot_metadata_path(&source),
            serde_json::to_vec(&metadata).unwrap(),
        )
        .unwrap();
        copy_snapshot_metadata(&source, &target).unwrap();
        assert_eq!(
            std::fs::read(crate::platform::snapshot_metadata_path(&target)).unwrap(),
            serde_json::to_vec(&metadata).unwrap()
        );

        std::fs::write(
            crate::platform::snapshot_metadata_path(&source),
            b"{invalid",
        )
        .unwrap();
        let error = copy_snapshot_metadata(&source, &target).unwrap_err();
        assert!(error.contains("metadata is invalid"));
    }
}

//! The prompt stash: prompts parked from the composer (⌘S) and restored later,
//! newest first, across threads and providers.
use crate::js_text::utf16_units;
use crate::state::{Draft, DraftAttachment};
use agent_domain::MessageContext;
use serde::{Deserialize, Serialize};

pub const MAX_STASH_ENTRIES: usize = 20;
/// Budget for an entry's encoded image payloads; two typical screenshots fit.
pub const MAX_STASH_ENTRY_ATTACHMENT_CHARS: usize = 2_700_000;
/// A message carries at most this many attachments.
pub const MAX_COMPOSER_ATTACHMENTS: usize = 100;
const SNIPPET_MAX_CHARS: usize = 90;

/// An image re-encoded for the stash; the live attachment keeps the original.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct StashImage {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
    pub data_url: String,
}

/// A file kept as its Host upload, not its bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct StashFile {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
    pub attachment_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptStashEntry {
    pub id: String,
    pub created_at_ms: i64,
    pub prompt: String,
    pub images: Vec<StashImage>,
    pub files: Vec<StashFile>,
    /// Images over the attachment budget, not saved.
    pub dropped_image_names: Vec<String>,
    /// Images that could not be decoded or re-encoded.
    pub unreadable_image_names: Vec<String>,
    /// Images still encoding; the entry is written before them.
    pub pending_image_count: u32,
    /// Payloads behind the prompt's context links.
    pub context: Option<MessageContext>,
}

/// The images that finished encoding for an entry.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct StashImages {
    pub images: Vec<StashImage>,
    pub dropped_image_names: Vec<String>,
    pub unreadable_image_names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PromptStash {
    pub entries: Vec<PromptStashEntry>,
}

impl PromptStash {
    /// Prepends `entry`; returns the oldest entry when it falls past the cap.
    pub fn stash(&mut self, entry: PromptStashEntry) -> Option<PromptStashEntry> {
        self.entries.insert(0, entry);
        (self.entries.len() > MAX_STASH_ENTRIES)
            .then(|| self.entries.pop())
            .flatten()
    }

    /// Removes and returns an entry (restore or delete).
    pub fn take(&mut self, id: &str) -> Option<PromptStashEntry> {
        let index = self.entries.iter().position(|entry| entry.id == id)?;
        Some(self.entries.remove(index))
    }

    /// Attaches encoded images to an entry written earlier; false when the
    /// entry was restored or deleted while they were encoding.
    pub fn finalize_images(&mut self, id: &str, images: StashImages) -> bool {
        let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) else {
            return false;
        };
        entry.images = images.images;
        entry.dropped_image_names = images.dropped_image_names;
        entry.unreadable_image_names = images.unreadable_image_names;
        entry.pending_image_count = 0;
        true
    }

    /// After a reload no encode is running: images still pending were lost and
    /// are recorded as unreadable, so the prompt stays restorable.
    pub fn settle_pending_images(&mut self) {
        for entry in &mut self.entries {
            for index in 1..=entry.pending_image_count {
                entry
                    .unreadable_image_names
                    .push(format!("image {index} (not saved before reload)"));
            }
            entry.pending_image_count = 0;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct StashImagePartition {
    pub kept: Vec<StashImage>,
    pub dropped_names: Vec<String>,
}

/// Admits images in order while they fit the entry budget; earlier images win.
pub fn partition_stash_images(images: Vec<StashImage>) -> StashImagePartition {
    let mut partition = StashImagePartition::default();
    let mut used = 0;
    for image in images {
        if used + image.data_url.len() > MAX_STASH_ENTRY_ATTACHMENT_CHARS {
            partition.dropped_names.push(image.name);
            continue;
        }
        used += image.data_url.len();
        partition.kept.push(image);
    }
    partition
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum StashShortcut {
    Stash,
    Restore { entry_id: String },
    ToggleMenu,
}

/// ⌘S stashes a composer with content. An empty composer restores the only
/// entry when it has finished saving, and otherwise opens the menu.
pub fn stash_shortcut(draft: &Draft, stash: &PromptStash) -> StashShortcut {
    if !draft.text.trim().is_empty() || !draft.attachments.is_empty() {
        return StashShortcut::Stash;
    }
    match stash.entries.as_slice() {
        [entry] if entry.pending_image_count == 0 => StashShortcut::Restore {
            entry_id: entry.id.clone(),
        },
        _ => StashShortcut::ToggleMenu,
    }
}

fn is_image(attachment: &DraftAttachment) -> bool {
    attachment.kind == "image"
}

/// The entry for the draft, written before its images finish encoding.
/// Everything it carries then leaves the draft.
pub fn new_stash_entry(
    id: String,
    now_ms: i64,
    draft: &Draft,
    context: Option<MessageContext>,
) -> Result<PromptStashEntry, String> {
    let files = draft
        .attachments
        .iter()
        .filter(|attachment| !is_image(attachment))
        .map(|file| match (&file.remote_id, file.status.as_str()) {
            (Some(attachment_id), "ready") => Ok(StashFile {
                id: file.id.clone(),
                name: file.name.clone(),
                mime_type: file.mime_type.clone(),
                size_bytes: file.size_bytes,
                attachment_id: attachment_id.clone(),
            }),
            _ => Err("Wait for file uploads before stashing this prompt".to_owned()),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PromptStashEntry {
        id,
        created_at_ms: now_ms,
        prompt: draft.text.trim().into(),
        images: vec![],
        files,
        dropped_image_names: vec![],
        unreadable_image_names: vec![],
        pending_image_count: u32::try_from(
            draft.attachments.iter().filter(|a| is_image(a)).count(),
        )
        .unwrap_or(u32::MAX),
        context: context.filter(|context| !context.records.is_empty()),
    })
}

pub fn evicted_entry_warning() -> String {
    format!("The stash holds {MAX_STASH_ENTRIES} prompts; the oldest was removed to make room.")
}

/// What restoring an entry adds to the draft.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct StashRestore {
    pub text: String,
    pub text_changed: bool,
    pub images: Vec<StashImage>,
    pub files: Vec<StashFile>,
    /// "Some attachments were not restored": one sentence per cause.
    pub warning: Option<String>,
}

/// The restored prompt joins existing text after a blank line; an entry
/// without text leaves the draft text alone.
pub fn restored_prompt(current: &str, restored: &str) -> String {
    if restored.is_empty() {
        current.into()
    } else if current.trim().is_empty() {
        restored.into()
    } else {
        format!("{}\n\n{restored}", current.trim_end())
    }
}

fn dedup_key(mime_type: &str, size_bytes: u64, name: &str) -> String {
    format!("{mime_type}\u{0}{size_bytes}\u{0}{name}")
}

/// Restores `entry` into `draft`. `expired_uploads` are the files whose Host
/// upload is gone. The model selection is never restored: the stash carries
/// a prompt across threads and providers.
pub fn restore_stash_entry(
    entry: &PromptStashEntry,
    draft: &Draft,
    expired_uploads: &[String],
) -> StashRestore {
    let text = restored_prompt(&draft.text, &entry.prompt);
    let (images_now, files_now): (Vec<&DraftAttachment>, Vec<&DraftAttachment>) =
        draft.attachments.iter().partition(|a| is_image(a));
    let mut file_ids: Vec<&str> = files_now.iter().map(|file| file.id.as_str()).collect();
    let mut file_keys: Vec<String> = files_now
        .iter()
        .map(|file| dedup_key(&file.mime_type, file.size_bytes, &file.name))
        .collect();
    let mut expired_names = vec![];
    let mut appended = vec![];
    for file in &entry.files {
        let key = dedup_key(&file.mime_type, file.size_bytes, &file.name);
        if file_ids.contains(&file.id.as_str()) || file_keys.contains(&key) {
            continue;
        }
        file_ids.push(&file.id);
        file_keys.push(key);
        if expired_uploads.contains(&file.attachment_id) {
            expired_names.push(file.name.clone());
        } else {
            appended.push(file.clone());
        }
    }
    let capacity = MAX_COMPOSER_ATTACHMENTS.saturating_sub(images_now.len() + files_now.len());
    let unrestored_files: Vec<String> = appended
        .iter()
        .skip(capacity)
        .map(|file| file.name.clone())
        .collect();
    appended.truncate(capacity);

    let image_keys: Vec<String> = images_now
        .iter()
        .map(|image| dedup_key(&image.mime_type, image.size_bytes, &image.name))
        .collect();
    let pending: Vec<&StashImage> = entry
        .images
        .iter()
        .filter(|image| {
            !images_now.iter().any(|existing| existing.id == image.id)
                && !image_keys.contains(&dedup_key(&image.mime_type, image.size_bytes, &image.name))
        })
        .collect();
    let capacity = MAX_COMPOSER_ATTACHMENTS
        .saturating_sub(images_now.len() + files_now.len() + appended.len());
    let unrestored_images: Vec<String> = pending
        .iter()
        .skip(capacity)
        .map(|image| image.name.clone())
        .collect();

    let mut reasons = vec![];
    if !entry.dropped_image_names.is_empty() {
        reasons.push(format!(
            "{} exceeded the stash size limit when this prompt was saved.",
            entry.dropped_image_names.join(", ")
        ));
    }
    if !entry.unreadable_image_names.is_empty() {
        reasons.push(format!(
            "{} could not be read when this prompt was saved.",
            entry.unreadable_image_names.join(", ")
        ));
    }
    for names in [&unrestored_images, &unrestored_files] {
        if !names.is_empty() {
            reasons.push(format!(
                "{} could not be restored: the composer is at its {MAX_COMPOSER_ATTACHMENTS}-attachment limit.",
                names.join(", ")
            ));
        }
    }
    if !expired_names.is_empty() {
        reasons.push(format!(
            "{}: stashed files are kept for 24 hours and this upload expired. Attach the file again.",
            expired_names.join(", ")
        ));
    }
    StashRestore {
        text_changed: text != draft.text,
        text,
        images: pending.into_iter().take(capacity).cloned().collect(),
        files: appended,
        warning: (!reasons.is_empty()).then(|| reasons.join(" ")),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct StashEntryView {
    pub id: String,
    pub snippet: String,
    pub restore_label: String,
    /// "saving 2 images…" or "1 image dropped".
    pub status: Option<String>,
    pub status_warning: bool,
    /// The first three images, shown as thumbnails.
    pub thumbnails: Vec<String>,
    pub file_count: u32,
    pub created_at_ms: i64,
}

fn plural(count: usize, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
}

/// The prompt on one line, at most 90 characters, or what the entry attaches.
pub fn stash_entry_snippet(entry: &PromptStashEntry) -> String {
    let trimmed = entry
        .prompt
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if !trimmed.is_empty() {
        let units = utf16_units(&trimmed);
        return if units.len() > SNIPPET_MAX_CHARS {
            format!("{}…", String::from_utf16_lossy(&units[..SNIPPET_MAX_CHARS]))
        } else {
            trimmed
        };
    }
    let images = entry.images.len() + entry.dropped_image_names.len();
    let files = entry.files.len();
    match (images, files) {
        (0, 0) => "(empty)".into(),
        (0, _) => format!("({})", plural(files, "file")),
        (_, 0) => format!("({})", plural(images, "image")),
        _ => format!("({})", plural(images + files, "attachment")),
    }
}

pub fn stash_menu(stash: &PromptStash) -> Vec<StashEntryView> {
    stash
        .entries
        .iter()
        .map(|entry| {
            let snippet = stash_entry_snippet(entry);
            let missing = entry.dropped_image_names.len() + entry.unreadable_image_names.len();
            let (status, status_warning) = if entry.pending_image_count > 0 {
                (
                    Some(format!(
                        "saving {}…",
                        plural(entry.pending_image_count as usize, "image")
                    )),
                    false,
                )
            } else if missing > 0 {
                (Some(format!("{} dropped", plural(missing, "image"))), true)
            } else {
                (None, false)
            };
            StashEntryView {
                id: entry.id.clone(),
                restore_label: format!("Restore stashed prompt: {snippet}"),
                snippet,
                status,
                status_warning,
                thumbnails: entry
                    .images
                    .iter()
                    .take(3)
                    .map(|image| image.data_url.clone())
                    .collect(),
                file_count: u32::try_from(entry.files.len()).unwrap_or(u32::MAX),
                created_at_ms: entry.created_at_ms,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests;

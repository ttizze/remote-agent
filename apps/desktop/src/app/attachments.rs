use super::*;
use agent_core::state::DraftAttachment;
impl Desktop {
    pub(super) fn pick_attachments(&self) {
        let key = self.snapshot.draft_key();
        let updates = self.updates.clone();
        let epoch = self.epoch;
        self.runtime.handle.spawn(async move {
            if let Ok(Some(paths)) =
                tokio::task::spawn_blocking(|| rfd::FileDialog::new().pick_files()).await
            {
                let result = tokio::task::spawn_blocking(move || stage_paths(paths))
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|result| result);
                let _ = updates
                    .send((epoch, Update::AttachmentsPicked(key, result)))
                    .await;
            }
        });
    }
    pub(super) fn stage_attachment_paths(&self, paths: Vec<PathBuf>) {
        let key = self.snapshot.draft_key();
        let epoch = self.epoch;
        let updates = self.updates.clone();
        self.runtime.handle.spawn(async move {
            let result = tokio::task::spawn_blocking(move || stage_paths(paths))
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r);
            let _ = updates
                .send((epoch, Update::AttachmentsPicked(key, result)))
                .await;
        });
    }
    pub(super) fn paste_attachments(&self, item: ClipboardItem) -> bool {
        if !item.entries.iter().any(|e| {
            matches!(
                e,
                ClipboardEntry::Image(_) | ClipboardEntry::ExternalPaths(_)
            )
        }) {
            return false;
        }
        let key = self.snapshot.draft_key();
        let epoch = self.epoch;
        let updates = self.updates.clone();
        self.runtime.handle.spawn(async move {
            let result = tokio::task::spawn_blocking(move || stage_clipboard(item))
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r);
            let _ = updates
                .send((epoch, Update::AttachmentsPicked(key, result)))
                .await;
        });
        true
    }
    pub(super) fn preload_attachments(&mut self, view: &ConversationView) {
        let Some(session) = &self.session else {
            return;
        };
        for a in view
            .rows
            .iter()
            .flat_map(|r| &r.attachments)
            .filter(|a| a.kind == "image")
        {
            let Some(id) = &a.remote_id else {
                continue;
            };
            if self.attachment_cache.contains_key(id) {
                continue;
            }
            self.attachment_cache.insert(id.clone(), None);
            let store = session.store.clone();
            let directory = self.attachment_directory.clone();
            let updates = self.updates.clone();
            let epoch = self.epoch;
            let id = id.clone();
            self.runtime.handle.spawn(async move {
                let path = directory.path().join(uuid::Uuid::new_v4().to_string());
                let result = store
                    .download_attachment(id.clone(), path.to_string_lossy().into_owned())
                    .await
                    .map(|_| path)
                    .map_err(|e| e.to_string());
                let _ = updates
                    .send((epoch, Update::AttachmentReady(id, result)))
                    .await;
            });
        }
    }
    pub(super) fn save_attachment(&self, a: &DraftAttachment) {
        let Some(session) = &self.session else {
            return;
        };
        let Some(id) = a.remote_id.clone() else {
            return;
        };
        let store = session.store.clone();
        let name = a.name.clone();
        let updates = self.updates.clone();
        let epoch = self.epoch;
        self.runtime.handle.spawn(async move {
            if let Ok(Some(path)) = tokio::task::spawn_blocking(move || {
                rfd::FileDialog::new().set_file_name(name).save_file()
            })
            .await
            {
                let result = store
                    .download_attachment(id, path.to_string_lossy().into_owned())
                    .await
                    .map(|_| Outcome::Applied)
                    .map_err(|e| e.to_string());
                let _ = updates.send((epoch, Update::Completed(None, result))).await;
            }
        });
    }
    pub(super) fn attachment_tile(
        &self,
        a: &DraftAttachment,
        editing: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let attachment = a.clone();
        let mut tile = v_flex().w(px(100.)).gap_1();
        let path = if a.local_path.is_empty() {
            a.remote_id
                .as_ref()
                .and_then(|id| self.attachment_cache.get(id))
                .and_then(Clone::clone)
        } else {
            Some(PathBuf::from(&a.local_path))
        };
        if a.kind == "image"
            && let Some(path) = path
        {
            tile = tile.child(
                img(path)
                    .w(px(72.))
                    .h(px(72.))
                    .rounded(px(16.))
                    .object_fit(ObjectFit::Cover),
            );
        }
        tile = tile.child(
            Button::new(SharedString::from(format!("asset-{}", a.id)))
                .small()
                .ghost()
                .label(a.name.clone())
                .on_click(cx.listener(move |view, _, _, _| view.save_attachment(&attachment))),
        );
        if a.status == "uploading" {
            tile = tile.child(div().text_xs().child("Uploading…"));
        }
        if editing && a.status == "failed" {
            tile = tile.child(self.action(
                SharedString::from(format!("retry-{}", a.id)),
                "Retry",
                Intent::RetryAttachment { id: a.id.clone() },
                cx,
            ));
        }
        if editing {
            tile = tile.child(self.action(
                SharedString::from(format!("remove-{}", a.id)),
                "Remove",
                Intent::RemoveAttachment { id: a.id.clone() },
                cx,
            ));
        }
        if let Some(error) = &a.error {
            tile = tile.child(
                div()
                    .text_xs()
                    .text_color(color("errorForeground"))
                    .child(error.clone()),
            );
        }
        tile.into_any_element()
    }
}

fn draft_directory() -> Result<PathBuf, String> {
    let path = directories::BaseDirs::new()
        .ok_or("Application data directory unavailable")?
        .data_local_dir()
        .join("Bex")
        .join("draft-attachments")
        .join(uuid::Uuid::new_v4().to_string());
    host_daemon::platform::create_state_directory(&path).map_err(|e| e.to_string())?;
    Ok(path)
}
fn stage_paths(paths: Vec<PathBuf>) -> Result<Vec<PathBuf>, String> {
    paths
        .into_iter()
        .map(|source| {
            let info = std::fs::metadata(&source).map_err(|e| e.to_string())?;
            if !info.is_file() || info.len() > 50 * 1024 * 1024 {
                return Err("Choose a file up to 50 MiB".into());
            }
            let path = draft_directory()?.join(source.file_name().ok_or("Missing filename")?);
            std::fs::copy(&source, &path).map_err(|e| e.to_string())?;
            Ok(path)
        })
        .collect()
}
fn stage_clipboard(item: ClipboardItem) -> Result<Vec<PathBuf>, String> {
    let mut paths = vec![];
    for entry in item.entries {
        match entry {
            ClipboardEntry::ExternalPaths(files) => {
                paths.extend(stage_paths(files.paths().to_vec())?)
            }
            ClipboardEntry::Image(image) => {
                let mut reader = image::ImageReader::new(std::io::Cursor::new(image.bytes))
                    .with_guessed_format()
                    .map_err(|e| e.to_string())?;
                let mut limits = image::Limits::default();
                limits.max_alloc = Some(128 * 1024 * 1024);
                reader.limits(limits);
                let image = reader
                    .decode()
                    .map_err(|e| e.to_string())?
                    .thumbnail(2048, 2048);
                let path = draft_directory()?.join("Image.png");
                image
                    .save_with_format(&path, image::ImageFormat::Png)
                    .map_err(|e| e.to_string())?;
                paths.push(path);
            }
            ClipboardEntry::String(_) => {}
        }
    }
    Ok(paths)
}

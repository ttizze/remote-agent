//! Reconcile the Store snapshot with native widgets and display caches.
//!
//! Pending composer/editor revisions and dirty settings keep local input ahead
//! of older snapshots; conversation projection and persistence remain delegated.

use super::Desktop;
use agent_core::state::{Intent, Snapshot};
use gpui_kit::{Context, SharedString, Window};
use std::sync::Arc;

impl Desktop {
    pub(super) fn accept_snapshot(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(store) = self.session.as_ref().map(|session| &session.store) else {
            return;
        };
        let snapshot = store.snapshot();
        let changed = !Arc::ptr_eq(&snapshot, &self.snapshot);
        let previous = std::mem::replace(&mut self.snapshot, snapshot);
        if let Some(request) = self
            .snapshot
            .permission_control(self.draft_key())
            .load_request
        {
            self.dispatch(Intent::ReadPermissionSettings(request));
        }
        if previous.error != self.snapshot.error
            && let Some(error) = &self.snapshot.error
        {
            tracing::error!(target: "bex", operation = "store", message = %error);
        }
        sync_error_banner(
            &mut self.error,
            previous.error.as_deref(),
            self.snapshot.error.as_deref(),
        );
        let previous_draft = previous.drafts.get(&previous.navigation.draft_key);
        let sources_changed = previous_draft.map(|draft| &draft.attachments)
            != self
                .snapshot
                .drafts
                .get(self.draft_key())
                .map(|draft| &draft.attachments);
        let navigated = previous.navigation.draft_key != self.snapshot.navigation.draft_key;
        let project_for_selected = |snapshot: &Snapshot| {
            snapshot
                .threads
                .as_ref()?
                .data
                .iter()
                .find(|thread| thread.id == snapshot.navigation.thread_id)?
                .project_id
                .as_ref()
                .cloned()
        };
        if (navigated || project_for_selected(&previous) != project_for_selected(&self.snapshot))
            && let Some(project) = project_for_selected(&self.snapshot)
        {
            self.expanded_projects.insert(project);
        }
        if navigated {
            self.selection
                .update(cx, |selection, cx| selection.clear(cx));
            self.cancel_recording();
            self.composer_pending = None;
            self.history_loading = false;
            self.history_error.clear();
            self.item_details.clear();
            self.rendered = None;
            self.diffs.clear();
            self.markdown_cache.clear();
        }
        if self.composer_pending.is_none() && self.composer_value.as_ref() != self.draft().text {
            let value: SharedString = self.draft().text.clone().into();
            self.composer_value = value.clone();
            self.composer
                .update(cx, |input, cx| input.set_value(value, window, cx));
        }
        let file = self.snapshot.workspace.file.as_ref();
        let path = file.map(|file| file.path.as_str());
        let file_changed = self.editor_path.as_deref() != path;
        let text = file.map_or("", |file| {
            self.snapshot
                .file_drafts
                .get(&file.path)
                .map_or(file.text.as_str(), |draft| draft.text.as_str())
        });
        if file_changed || (self.editor_pending.is_none() && self.editor_value.as_ref() != text) {
            self.editor_path = path.map(str::to_owned);
            self.editor_value = text.to_owned().into();
            self.editor_pending = None;
            self.editor_input.update(cx, |input, cx| {
                input.set_value(self.editor_value.clone(), window, cx)
            });
        }
        if !self.worktree_dirty
            && !self.worktree_saving
            && previous.workspace.settings != self.snapshot.workspace.settings
            && let Some(settings) = &self.snapshot.workspace.settings
        {
            self.worktree_copy_paths.update(cx, |input, cx| {
                input.set_value(settings.copy_paths.join("\n"), window, cx)
            });
            self.worktree_directory.update(cx, |input, cx| {
                input.set_value(settings.worktree_directory.clone(), window, cx)
            });
        }
        let conversations_changed =
            !Arc::ptr_eq(&previous.conversations, &self.snapshot.conversations);
        if conversations_changed {
            self.sync_request_inputs(window, cx);
        }
        if navigated
            || conversations_changed
            || !Arc::ptr_eq(
                &previous.pending_submissions,
                &self.snapshot.pending_submissions,
            )
        {
            self.sync_rows(navigated, window, cx);
        }
        let user_items_changed = {
            let mut current = self.user_items();
            !self.source_items.iter().all(|previous| {
                current
                    .next()
                    .is_some_and(|item| Arc::ptr_eq(previous, item))
            }) || current.next().is_some()
        };
        if sources_changed || user_items_changed || navigated {
            let mut paths: std::collections::BTreeSet<&str> = self
                .draft()
                .attachments
                .iter()
                .map(|attachment| attachment.path.as_str())
                .collect();
            for item in self.user_items() {
                if let agent_protocol::items::ItemBody::UserMessage { content, .. } = item.body() {
                    paths.extend(content.iter().filter_map(|part| match part {
                        agent_protocol::items::MessagePart::Image { source }
                            if !source.starts_with("data:") =>
                        {
                            Some(source.as_str())
                        }
                        agent_protocol::items::MessagePart::Attachment { path, .. }
                        | agent_protocol::items::MessagePart::Invocation { path, .. } => {
                            Some(path.as_str())
                        }
                        _ => None,
                    }));
                }
            }
            self.source_paths = paths.into_iter().map(str::to_owned).collect();
            self.source_items = self.user_items().cloned().collect();
        }
        if previous.workspace.directory != self.snapshot.workspace.directory
            && let Some(directory) = &self.snapshot.workspace.directory
        {
            let previous_path = previous
                .workspace
                .directory
                .as_ref()
                .map_or("", |directory| directory.path.as_str());
            if self.path.read(cx).value().as_ref() == previous_path {
                self.path.update(cx, |input, cx| {
                    input.set_value(directory.path.clone(), window, cx)
                });
            }
        }
        let cwd_changed = previous.navigation.cwd != self.snapshot.navigation.cwd;
        if cwd_changed {
            let cwd = self.snapshot.navigation.cwd.clone();
            self.path
                .update(cx, |input, cx| input.set_value(cwd, window, cx));
        }
        if changed && let Some(session) = &self.session {
            session.save(self.snapshot.clone());
        }
    }
}

fn sync_error_banner(banner: &mut String, previous: Option<&str>, next: Option<&str>) {
    if previous != next {
        if let Some(error) = next {
            *banner = error.to_owned();
        } else if previous == Some(banner.as_str()) {
            banner.clear();
        }
    }
}

#[cfg(test)]
mod error_tests {
    use super::sync_error_banner;

    #[test]
    fn recovered_store_error_clears_its_banner() {
        let mut banner = String::new();
        sync_error_banner(&mut banner, None, Some("thread read failed"));
        assert_eq!(banner, "thread read failed");
        sync_error_banner(&mut banner, Some("thread read failed"), None);
        assert!(banner.is_empty());
    }

    #[test]
    fn recovery_preserves_a_newer_local_error_and_respects_dismissal() {
        let mut banner = "draft save failed".to_owned();
        sync_error_banner(&mut banner, Some("thread read failed"), None);
        assert_eq!(banner, "draft save failed");
        banner.clear();
        sync_error_banner(
            &mut banner,
            Some("thread read failed"),
            Some("thread read failed"),
        );
        assert!(banner.is_empty());
        sync_error_banner(
            &mut banner,
            Some("thread read failed"),
            Some("disconnected"),
        );
        assert_eq!(banner, "disconnected");
    }
}

//! The prompt editor: keeping it in step with the draft (or the answer being
//! typed), the keys it sends and stashes with, pasting files, and the
//! attachments and context shown above it.
use super::{Desktop, shown_thread};
use crate::app::ui::{color, icon, text_2xs};
use agent_core::{
    state::{AnswerEdit, DraftAttachment, Intent, QueueAction},
    view::{
        api::answer_draft_key,
        attachments::format_attachment_size,
        composer::{actions::ComposerPrimaryAction, stash::StashShortcut, view::ComposerView},
        requests::answer_drafts,
        thread::ThreadView,
    },
};
use gpui_kit::{
    component::{
        Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        input::{Enter, Escape, IndentInline, InputEvent, MoveDown, MoveUp, Paste, TextareaState},
        spinner::Spinner,
        tooltip::Tooltip,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::path::PathBuf;

/// How the editor follows the draft: edits go out with the text they were
/// made on, and the draft's text is adopted only while none is in flight.
#[derive(Default)]
pub(super) struct EditorSync {
    revision: u64,
    pending: Option<u64>,
    /// The text the next edit was made on.
    base: String,
    /// What the editor shows: a draft, an answer or an approval.
    key: String,
    placeholder: String,
}

/// What typing in the editor changes.
enum Source {
    Draft,
    Answer {
        request_id: String,
        question_id: String,
    },
    /// An approval is waiting; the editor is empty and disabled.
    Approval,
}

pub(super) fn new_editor(
    window: &mut Window,
    cx: &mut Context<Desktop>,
    subscriptions: &mut Vec<Subscription>,
) -> Entity<TextareaState> {
    let editor = cx.new(|cx| TextareaState::new(window, cx).auto_grow(3, 9));
    subscriptions.push(cx.subscribe_in(
        &editor,
        window,
        |view: &mut Desktop, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                view.editor_changed(cx);
            }
        },
    ));
    editor
}

/// The byte offset of UTF-16 offset `units` in `text`, clamped to its end.
fn byte_offset(text: &str, units: u32) -> usize {
    let mut counted = 0;
    for (index, character) in text.char_indices() {
        if counted >= units as usize {
            return index;
        }
        counted += character.len_utf16();
    }
    text.len()
}

/// The UTF-16 offset of byte offset `byte` in `text`.
pub(super) fn utf16_offset(text: &str, byte: usize) -> u32 {
    let byte = byte.min(text.len());
    let prefix = text.get(..byte).unwrap_or(text);
    prefix.encode_utf16().count() as u32
}

impl Desktop {
    /// What the editor edits now: its identity, its kind and the text it shows.
    fn editor_source(&self) -> (String, Source, String) {
        if let Some(thread) = shown_thread(&self.views, self.snapshot.selected_thread.as_ref()) {
            if let Some(approval) = &thread.requests.approval {
                return (
                    format!("approval:{}", approval.request_id),
                    Source::Approval,
                    String::new(),
                );
            }
            if let Some(questions) = &thread.requests.questions
                && let Some(active) = &questions.active
            {
                // The snapshot holds the answer as typed; the views may lag.
                let text = answer_drafts(&self.snapshot, &questions.request_id)
                    .into_iter()
                    .find(|draft| draft.question_id == active.id)
                    .map_or_else(String::new, |draft| draft.custom_answer);
                return (
                    answer_draft_key(questions.request_id.clone(), active.id.clone()),
                    Source::Answer {
                        request_id: questions.request_id.clone(),
                        question_id: active.id.clone(),
                    },
                    text,
                );
            }
        }
        (
            self.snapshot.draft_key(),
            Source::Draft,
            self.snapshot.current_draft().text,
        )
    }

    pub(super) fn sync_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (key, _, text) = self.editor_source();
        let sync = &mut self.composer.sync;
        if key != sync.key {
            sync.pending = None;
            sync.key = key;
        }
        if sync.pending.is_none() {
            sync.base = text.clone();
            if self.composer.editor.read(cx).value().as_ref() != text {
                self.composer
                    .editor
                    .update(cx, |editor, cx| editor.set_value(text, window, cx));
            }
        }
        if let Some(placeholder) = self
            .shown_composer()
            .map(|composer| composer.editor.placeholder.clone())
            && placeholder != self.composer.sync.placeholder
        {
            self.composer.sync.placeholder = placeholder.clone();
            self.composer.editor.update(cx, |editor, cx| {
                editor.set_placeholder(placeholder, window, cx)
            });
        }
    }

    fn editor_changed(&mut self, cx: &mut Context<Self>) {
        let text = self.composer.editor.read(cx).value().to_string();
        self.composer.menu.text_changed();
        self.composer.banners.stash_open = false;
        if self.session.is_none() || text == self.composer.sync.base {
            cx.notify();
            return;
        }
        let (_, source, _) = self.editor_source();
        let base = std::mem::replace(&mut self.composer.sync.base, text.clone());
        let intent = match source {
            Source::Draft => Intent::EditDraft {
                text,
                base_text: Some(base),
            },
            Source::Answer {
                request_id,
                question_id,
            } => Intent::EditAnswer {
                request_id,
                question_id,
                edit: AnswerEdit::Custom { text },
            },
            Source::Approval => return,
        };
        self.composer.sync.revision += 1;
        let revision = self.composer.sync.revision;
        self.composer.sync.pending = Some(revision);
        self.perform_then(intent, move |view, result, window, cx| {
            if view.composer.sync.pending == Some(revision) {
                view.composer.sync.pending = None;
            }
            if let Err(error) = result {
                view.show_error(error, window, cx);
            }
        });
        cx.notify();
    }

    /// Moves the caret to UTF-16 offset `cursor` and focuses the editor.
    pub(super) fn move_composer_caret(
        &mut self,
        cursor: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.composer.editor.update(cx, |editor, cx| {
            let offset = byte_offset(&editor.value(), cursor);
            editor.set_selected_range(offset..offset, cx);
            editor.focus(window, cx);
        });
    }

    /// The editor's text and its caret as a UTF-16 offset.
    pub(super) fn editor_text_and_cursor(&self, cx: &App) -> (String, u32) {
        let editor = self.composer.editor.read(cx);
        let text = editor.value().to_string();
        let cursor = utf16_offset(&text, editor.cursor());
        (text, cursor)
    }

    pub(crate) fn composer_focused(&self, window: &Window, cx: &App) -> bool {
        self.editor_focused(window, cx)
    }

    fn editor_focused(&self, window: &Window, cx: &App) -> bool {
        self.composer
            .editor
            .read(cx)
            .focus_handle(cx)
            .is_focused(window)
    }

    /// The keys the composer handles before the editor sees them.
    pub(super) fn composer_keys(&mut self, stack: Div, cx: &mut Context<Self>) -> Div {
        stack
            .key_context("ChatComposer")
            .on_modifiers_changed(cx.listener(|view, event: &ModifiersChangedEvent, _, cx| {
                let held = event.modifiers.secondary();
                if held != view.composer.alternate_modifier {
                    view.composer.alternate_modifier = held;
                    view.refresh_views(cx);
                }
            }))
            .capture_action(cx.listener(|view, action: &Enter, window, cx| {
                if action.shift || !view.editor_focused(window, cx) {
                    return;
                }
                cx.stop_propagation();
                if !view.select_composer_menu_item(cx) && !view.restore_highlighted_stash(cx) {
                    view.submit_composer(action.secondary, window, cx);
                }
            }))
            .capture_action(cx.listener(|view, _: &IndentInline, window, cx| {
                if view.editor_focused(window, cx) && view.select_composer_menu_item(cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|view, _: &MoveUp, window, cx| {
                if view.editor_focused(window, cx)
                    && (view.step_composer_menu(false, cx) || view.step_stash_menu(false, cx))
                {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|view, _: &MoveDown, window, cx| {
                if view.editor_focused(window, cx)
                    && (view.step_composer_menu(true, cx) || view.step_stash_menu(true, cx))
                {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|view, _: &Escape, window, cx| {
                if view.editor_focused(window, cx) && view.dismiss_composer_layer(cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|view, _: &Paste, window, cx| {
                if view.editor_focused(window, cx)
                    && let Some(item) = cx.read_from_clipboard()
                    && let Some(key) = view.composer_attachment_target()
                    && view.paste_attachments(key, item)
                {
                    cx.stop_propagation();
                }
            }))
    }

    /// Steers with the first queued message.
    pub(crate) fn steer_first_queued(&mut self) {
        let queue = shown_thread(&self.views, self.snapshot.selected_thread.as_ref())
            .and_then(|thread| thread.queue.as_ref());
        if let Some(run_id) = queue.and_then(|queue| queue.steer_next_run_id.clone()) {
            self.perform(Intent::Queue {
                action: QueueAction::Steer { run_id },
            });
        }
    }

    /// Edits the last queued message; false when nothing is queued.
    pub(crate) fn edit_last_queued(&mut self) -> bool {
        let Some(run_id) = shown_thread(&self.views, self.snapshot.selected_thread.as_ref())
            .and_then(|thread| thread.queue.as_ref())
            .and_then(|queue| queue.edit_latest_run_id.clone())
        else {
            return false;
        };
        self.composer.banners.queue_collapsed = false;
        self.perform(Intent::Queue {
            action: QueueAction::Edit { run_id },
        });
        true
    }

    /// Enter and the primary button: send, answer, refine or implement as
    /// the composer's primary action says. `alternate` is the second send
    /// gesture (Mod+Enter, Mod+click).
    pub(super) fn submit_composer(
        &mut self,
        alternate: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(action) = self
            .shown_composer()
            .map(|composer| composer.primary_action.clone())
        else {
            return;
        };
        // The views trail typing by a frame; the Host validates what is sent.
        let typed = !self.composer.editor.read(cx).value().trim().is_empty();
        let send = match action {
            ComposerPrimaryAction::Answer {
                submit_disabled, ..
            } => {
                if !submit_disabled {
                    self.advance_answer(window, cx);
                }
                false
            }
            ComposerPrimaryAction::Send(button) if button.resumes && !typed => {
                self.resume_composer_thread();
                false
            }
            ComposerPrimaryAction::Send(button) => !button.disabled || typed,
            ComposerPrimaryAction::Refine { disabled, .. }
            | ComposerPrimaryAction::Implement { disabled, .. } => !disabled,
            ComposerPrimaryAction::Stop { .. } => typed,
        };
        if send {
            self.perform(Intent::Send { alternate });
        }
    }

    /// Resume continues the thread's held queue.
    pub(super) fn resume_composer_thread(&self) {
        self.perform(Intent::Queue {
            action: QueueAction::Resume,
        });
    }

    /// ⌘S stashes a composer with content; an empty one restores the only
    /// entry or opens the stash. While a question waits it opens the stash.
    pub(crate) fn composer_stash_shortcut(&mut self, cx: &mut Context<Self>) {
        let requests = shown_thread(&self.views, self.snapshot.selected_thread.as_ref())
            .map(|thread| &thread.requests);
        if requests.is_some_and(|requests| requests.approval.is_some()) {
            return;
        }
        if requests.is_some_and(|requests| requests.questions.is_some()) {
            self.composer.banners.stash_open = !self.composer.banners.stash_open;
            cx.notify();
            return;
        }
        let Some(shortcut) = self
            .shown_composer()
            .map(|composer| composer.stash_shortcut.clone())
        else {
            return;
        };
        match shortcut {
            StashShortcut::Stash => self.stash_composer_draft(),
            StashShortcut::Restore { entry_id } => self.perform(Intent::RestoreStash { entry_id }),
            StashShortcut::ToggleMenu => {
                self.composer.banners.stash_open = !self.composer.banners.stash_open;
            }
        }
        cx.notify();
    }

    /// Stashes the draft, keeping where its images are so they can be
    /// encoded into the entry once it exists.
    fn stash_composer_draft(&mut self) {
        let images: Vec<DraftAttachment> = self
            .snapshot
            .current_draft()
            .attachments
            .into_iter()
            .filter(|attachment| attachment.kind == "image")
            .collect();
        self.composer.stash_images = images
            .into_iter()
            .map(|image| {
                let local = PathBuf::from(&image.local_path);
                let path = if !image.local_path.is_empty() && local.exists() {
                    local
                } else {
                    image
                        .remote_id
                        .as_deref()
                        .and_then(|id| self.attachment_image(id))
                        .unwrap_or(local)
                };
                (image.id, image.name, image.mime_type, path)
            })
            .collect();
        self.perform(Intent::StashDraft);
    }

    /// The draft's images, files and their upload state above the editor.
    pub(super) fn composer_attachments(
        &mut self,
        thread: Option<&ThreadView>,
        composer: &ComposerView,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let answer = thread
            .and_then(|thread| thread.requests.questions.as_ref())
            .and_then(|questions| {
                questions
                    .active
                    .as_ref()
                    .map(|active| answer_draft_key(questions.request_id.clone(), active.id.clone()))
            });
        let (key, attachments) = match answer {
            Some(key) => {
                let attachments = self
                    .snapshot
                    .drafts
                    .get(&key)
                    .map(|draft| draft.attachments.clone())
                    .unwrap_or_default();
                (key, attachments)
            }
            None => (composer.draft_key.clone(), composer.attachments.clone()),
        };
        let (images, files): (Vec<_>, Vec<_>) = attachments
            .into_iter()
            .partition(|attachment| attachment.kind == "image");
        let mut sections = vec![];
        if !images.is_empty() {
            let tiles: Vec<AnyElement> = images
                .into_iter()
                .map(|image| self.composer_image_tile(&key, image, cx))
                .collect();
            sections.push(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .mb_3()
                    .children(tiles)
                    .into_any_element(),
            );
        }
        if !files.is_empty() {
            let rows: Vec<AnyElement> = files
                .into_iter()
                .map(|file| self.composer_file_row(&key, file, cx))
                .collect();
            sections.push(v_flex().gap_1().mb_3().children(rows).into_any_element());
        }
        sections
    }

    fn composer_image_tile(
        &mut self,
        key: &str,
        image: DraftAttachment,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let source = if !image.local_path.is_empty() {
            Some(PathBuf::from(&image.local_path))
        } else {
            image
                .remote_id
                .as_deref()
                .and_then(|id| self.attachment_image(id))
        };
        let remove = {
            let key = key.to_owned();
            let id = image.id.clone();
            cx.listener(move |view, _: &ClickEvent, _, _| {
                view.perform(Intent::RemoveAttachment {
                    draft_key: Some(key.clone()),
                    id: id.clone(),
                })
            })
        };
        div()
            .relative()
            .size(px(64.))
            .flex_none()
            .overflow_hidden()
            .rounded_lg()
            .border_1()
            .border_color(color("border").opacity(0.8))
            .bg(color("canvas"))
            .child(match source {
                Some(path) => img(path)
                    .size_full()
                    .object_fit(ObjectFit::Cover)
                    .into_any_element(),
                None => div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .px_1()
                    .text_size(px(10.))
                    .text_color(color("textMuted"))
                    .child(image.name.clone())
                    .into_any_element(),
            })
            .when(image.status == "uploading", |tile| {
                tile.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .flex()
                        .justify_center()
                        .bg(color("canvas").opacity(0.85))
                        .child(Spinner::new().with_size(px(10.))),
                )
            })
            .when(image.status == "failed", |tile| {
                tile.child(
                    div()
                        .absolute()
                        .left_1()
                        .bottom_1()
                        .child(retry_button(key, &image, cx)),
                )
            })
            .child(
                div().absolute().top_1().right_1().child(
                    Button::new(SharedString::from(format!("remove-{}", image.id)))
                        .icon(icon("x"))
                        .xsmall()
                        .rounded_full()
                        .bg(hsla(0., 0., 0., 0.6))
                        .text_color(hsla(0., 0., 1., 1.))
                        .accessibility_label(format!("Remove {}", image.name))
                        .on_click(remove),
                ),
            )
            .into_any_element()
    }

    fn composer_file_row(
        &mut self,
        key: &str,
        file: DraftAttachment,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let remove = {
            let key = key.to_owned();
            let id = file.id.clone();
            cx.listener(move |view, _: &ClickEvent, _, _| {
                view.perform(Intent::RemoveAttachment {
                    draft_key: Some(key.clone()),
                    id: id.clone(),
                })
            })
        };
        h_flex()
            .min_w_0()
            .items_center()
            .gap_2()
            .py_1()
            .text_sm()
            .child(icon("file").size(px(16.)).text_color(color("textMuted")))
            .child(div().min_w_0().flex_1().truncate().child(file.name.clone()))
            .child(if file.status == "uploading" {
                Spinner::new().with_size(px(12.)).into_any_element()
            } else {
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(color("textMuted"))
                    .child(format_attachment_size(file.size_bytes))
                    .into_any_element()
            })
            .when(file.status == "failed", |row| {
                row.child(retry_button(key, &file, cx))
            })
            .child(
                Button::new(SharedString::from(format!("remove-{}", file.id)))
                    .icon(icon("x"))
                    .ghost()
                    .xsmall()
                    .accessibility_label(format!("Remove {}", file.name))
                    .on_click(remove),
            )
            .into_any_element()
    }

    /// The context the draft's links stand for, each removable.
    pub(super) fn composer_context_chips(
        &mut self,
        composer: &ComposerView,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if composer.context_chips.is_empty() {
            return None;
        }
        let chips = composer.context_chips.iter().map(|chip| {
            let context_id = chip.context_id.clone();
            let tooltip = chip.tooltip.clone();
            h_flex()
                .id(SharedString::from(format!("context-{}", chip.context_id)))
                .max_w(px(240.))
                .items_center()
                .gap_1()
                .rounded_md()
                .border_1()
                .border_color(color("border"))
                .bg(color("accentSurface"))
                .pl(px(6.))
                .text_xs()
                .child(div().min_w_0().truncate().child(chip.label.clone()))
                .when_some(chip.size_label.clone(), |row, size| {
                    row.child(text_2xs(div()).text_color(color("textMuted")).child(size))
                })
                .when_some(tooltip, |row, tooltip| {
                    row.tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                })
                .child(
                    Button::new(SharedString::from(format!(
                        "remove-context-{}",
                        chip.context_id
                    )))
                    .icon(icon("x"))
                    .ghost()
                    .xsmall()
                    .accessibility_label(format!("Remove {}", chip.label))
                    .on_click(cx.listener(
                        move |view, _: &ClickEvent, _, _| {
                            view.perform(Intent::RemoveDraftContext {
                                context_id: context_id.clone(),
                            })
                        },
                    )),
                )
        });
        Some(
            h_flex()
                .flex_wrap()
                .gap_1()
                .mb_2()
                .children(chips)
                .into_any_element(),
        )
    }
}

/// Retries a failed upload; the tooltip says why it failed.
fn retry_button(key: &str, attachment: &DraftAttachment, cx: &mut Context<Desktop>) -> Button {
    let key = key.to_owned();
    let id = attachment.id.clone();
    Button::new(SharedString::from(format!("retry-{}", attachment.id)))
        .icon(icon("refresh-cw"))
        .ghost()
        .xsmall()
        .accessibility_label(format!("Retry upload for {}", attachment.name))
        .when_some(attachment.error.clone(), |button, error| {
            button.tooltip(error)
        })
        .on_click(cx.listener(move |view, _: &ClickEvent, _, _| {
            view.perform(Intent::RetryAttachment {
                draft_key: Some(key.clone()),
                id: id.clone(),
            })
        }))
}

#[cfg(test)]
mod tests {
    use super::{byte_offset, utf16_offset};

    #[test]
    fn utf16_offsets_map_to_byte_offsets_and_back() {
        let text = "a😀é\nb";
        assert_eq!(byte_offset(text, 0), 0);
        assert_eq!(byte_offset(text, 1), 1);
        assert_eq!(byte_offset(text, 3), 5);
        assert_eq!(byte_offset(text, 4), 7);
        assert_eq!(byte_offset(text, 99), text.len());
        assert_eq!(utf16_offset(text, 5), 3);
        assert_eq!(utf16_offset(text, text.len()), 6);
        assert_eq!(utf16_offset(text, 999), 6);
    }

    #[test]
    fn offsets_round_trip_on_character_boundaries() {
        let text = "日本語 text 🎉!";
        for (index, _) in text.char_indices() {
            assert_eq!(byte_offset(text, utf16_offset(text, index)), index);
        }
    }
}

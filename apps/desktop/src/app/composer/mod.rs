//! The composer: banners above it, the editor surface and its controls, and
//! the new-thread hero.
mod actions;
mod banners;
mod controls;
mod editor;
mod hero;
mod menu;

use super::{
    Desktop, Views,
    sidebar::ThreadDrag,
    ui::{color, is_dark, metrics, prompt_font, tint},
};
use agent_core::{
    connection::Outcome,
    state::Intent,
    view::{
        composer::view::{ComposerOptions, ComposerShortcuts, ComposerView},
        thread::ThreadView,
    },
};
use agent_domain::ThreadId;
use gpui_kit::{
    component::{
        WindowExt, h_flex,
        input::{Textarea, TextareaState},
        notification::Notification,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::path::PathBuf;

pub(crate) struct ComposerState {
    editor: Entity<TextareaState>,
    sync: editor::EditorSync,
    /// The alternate send modifier (⌘ or Ctrl) is held.
    alternate_modifier: bool,
    picker: controls::PickerState,
    branches: controls::BranchPickerState,
    menu: menu::MenuState,
    banners: banners::BannerState,
    /// How many footer blocks sit in the overflow menu.
    footer_layout: agent_core::view::composer::footer_layout::FooterLayout,
    /// The image attachments of the draft just sent to the stash: id, name,
    /// MIME type and local file.
    stash_images: Vec<(String, String, String, PathBuf)>,
}
impl ComposerState {
    pub(crate) fn new(
        window: &mut Window,
        cx: &mut Context<Desktop>,
        subscriptions: &mut Vec<Subscription>,
    ) -> Self {
        let editor = editor::new_editor(window, cx, subscriptions);
        Self {
            editor,
            sync: editor::EditorSync::default(),
            alternate_modifier: false,
            picker: controls::PickerState::new(window, cx, subscriptions),
            branches: controls::BranchPickerState::new(window, cx, subscriptions),
            menu: menu::MenuState::default(),
            banners: banners::BannerState::new(cx),
            footer_layout: agent_core::view::composer::footer_layout::FooterLayout {
                hidden_count: 0,
                visible: true,
            },
            stash_images: vec![],
        }
    }
    /// Labels and the held modifier the composer view is derived with.
    pub(crate) fn options(&self) -> ComposerOptions {
        ComposerOptions {
            compact: false,
            alternate_modifier: self.alternate_modifier,
            shortcuts: shortcuts(),
        }
    }
}

/// The send and queue chords as the desktop writes them.
fn shortcuts() -> ComposerShortcuts {
    let (send, steer, edit) = if cfg!(target_os = "macos") {
        ("⌘↵", "⌘⇧↵", "⌥↑")
    } else {
        ("Ctrl+Enter", "Ctrl+Shift+Enter", "Alt+Up")
    };
    ComposerShortcuts {
        alternate_send: Some(send.into()),
        queue_steer: Some(steer.into()),
        queue_edit: Some(edit.into()),
    }
}

/// The selected thread's view, once it has been derived.
fn shown_thread<'a>(views: &'a Views, selected: Option<&ThreadId>) -> Option<&'a ThreadView> {
    views
        .thread
        .as_ref()
        .filter(|thread| selected.is_some_and(|id| id.as_str() == thread.thread_id))
}

/// The composer glass: the card (light) or raised surface (dark) at 80% over
/// the canvas.
fn glass() -> Hsla {
    let surface = if is_dark() {
        "surfaceRaised"
    } else {
        "surface"
    };
    let opacity = super::ui::appearance().glass_opacity as f32 / 100.;
    color("canvas").blend(color(surface).opacity(opacity))
}

/// The composer's 1px outline.
fn outline() -> Hsla {
    if is_dark() {
        hsla(0., 0., 1., 0.05)
    } else {
        hsla(0., 0., 0., 0.08)
    }
}

/// The composer shadow, drawn in light mode only.
fn composer_shadow() -> Vec<BoxShadow> {
    if is_dark() {
        return vec![];
    }
    vec![BoxShadow {
        color: hsla(0., 0., 0., 0.4),
        offset: point(px(0.), px(12.)),
        blur_radius: px(28.),
        spread_radius: px(-18.),
        inset: false,
    }]
}

/// A click handler that updates the window's `Desktop`, for elements built
/// where no `Context<Desktop>` is at hand (popovers and menus).
fn on_click(
    view: &WeakEntity<Desktop>,
    f: impl Fn(&mut Desktop, &mut Window, &mut Context<Desktop>) + 'static,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let view = view.clone();
    move |_, window, cx| {
        let _ = view.update(cx, |view, cx| f(view, window, cx));
    }
}

impl Desktop {
    /// The composer on screen: the open thread's, else the new-thread draft's.
    fn shown_composer(&self) -> Option<&ComposerView> {
        match shown_thread(&self.views, self.snapshot.selected_thread.as_ref()) {
            Some(thread) => Some(&thread.composer),
            None => self
                .views
                .new_thread
                .as_ref()
                .filter(|_| self.snapshot.selected_thread.is_none())
                .map(|draft| &draft.composer),
        }
    }

    /// The banner stack and composer under the open thread's timeline.
    pub(crate) fn render_composer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let views = self.views.clone();
        let Some(thread) = shown_thread(&views, self.snapshot.selected_thread.as_ref()) else {
            return div().into_any_element();
        };
        self.sync_composer_menu(cx);
        let mut stack = self.composer_stack(Some(thread), &thread.composer, window, cx);
        // "Composer context" keeps the workspace and branch under the
        // composer once the thread has started.
        if super::ui::appearance().composer_context
            && let Some(header) = &thread.header
            && let Some(workspace) = &header.workspace
        {
            stack = stack.child(controls::thread_context_strip(
                workspace,
                header.branch.as_deref(),
            ));
        }
        dock(stack)
    }

    /// The new-thread draft: the hero headline over a centred composer.
    pub(crate) fn render_new_thread(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let views = self.views.clone();
        let Some(draft) = views.new_thread.as_ref() else {
            return div().flex_1().into_any_element();
        };
        self.sync_composer_menu(cx);
        let strip = self.composer_host_strip(
            draft.workspace.as_ref(),
            draft.project_id.as_ref(),
            window,
            cx,
        );
        let stack = self
            .composer_stack(None, &draft.composer, window, cx)
            .child(strip);
        if draft.show_hero {
            return self.render_draft_hero(&draft.hero, stack, cx);
        }
        v_flex()
            .flex_1()
            .min_h_0()
            .child(div().flex_1())
            .child(dock(stack))
            .into_any_element()
    }

    /// The banners above the composer and the composer itself, at the chat
    /// column's width.
    fn composer_stack(
        &mut self,
        thread: Option<&ThreadView>,
        composer: &ComposerView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let approval = thread.is_some_and(|thread| thread.requests.approval.is_some());
        let surface = self.composer_surface(thread, composer, approval, window, cx);
        let dock = self.composer_banners(thread, composer, approval, window, cx);
        let stack = v_flex()
            .w_full()
            .max_w(px(metrics().chat_max_width))
            .mx_auto()
            .child(dock)
            .child(surface);
        self.composer_keys(stack, cx)
    }

    /// The rounded glass surface: attachments, the editor, any validation
    /// message and the controls footer.
    fn composer_surface(
        &mut self,
        thread: Option<&ThreadView>,
        composer: &ComposerView,
        approval: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let message = composer
            .validation_message
            .clone()
            .or_else(|| composer.attachment_error.clone());
        let body = v_flex()
            .px(px(16.))
            .pt(px(16.))
            .pb(px(if approval { 16. } else { 8. }))
            .when(!approval, |body| {
                body.children(self.composer_attachments(thread, composer, cx))
                    .children(self.composer_context_chips(composer, cx))
            })
            .child(
                Textarea::new(&self.composer.editor)
                    .appearance(false)
                    .disabled(composer.editor.disabled)
                    .aria_label("Message")
                    .text_size(px(metrics().prompt_size))
                    .when_some(prompt_font(), |field, family| field.font_family(family))
                    .line_height(relative(1.625))
                    // The field's own padding replaces the body's here.
                    .mx(px(-10.))
                    .my(px(-8.))
                    .min_h(px(if approval { 56. } else { 94. }))
                    .max_h(px(224.)),
            );
        div()
            .id("composer-surface")
            .relative()
            .w_full()
            .rounded(px(22.))
            .bg(glass())
            .border_1()
            .border_color(outline())
            .shadow(composer_shadow())
            .text_color(color("text"))
            .drag_over::<ThreadDrag>(|surface, _, _, _| {
                surface
                    .bg(tint("accentSurface", 0.45))
                    .border_color(tint("accent", 0.7))
            })
            .on_drop(
                cx.listener(|view, drag: &ThreadDrag, _, cx| {
                    view.drop_threads_on_composer(drag, cx)
                }),
            )
            .child(body)
            .when_some(message, |surface, message| {
                surface.child(
                    div()
                        .px(px(16.))
                        .pb(px(8.))
                        .text_xs()
                        .text_color(color("error"))
                        .child(message),
                )
            })
            .when(!approval, |surface| {
                surface.child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .justify_between()
                        .gap_2()
                        .px(px(16.))
                        .pb(px(16.))
                        .when(
                            thread.is_some_and(|thread| thread.requests.questions.is_some()),
                            |footer| footer.pt(px(8.)),
                        )
                        .child(self.composer_controls(composer, window, cx))
                        .child(self.composer_actions(composer, window, cx)),
                )
            })
            .into_any_element()
    }

    /// Brings the editor in line with the draft after a snapshot or views change.
    pub(crate) fn sync_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_editor(window, cx);
        self.sync_banners();
    }

    pub(crate) fn composer_outcome(
        &mut self,
        outcome: &Outcome,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match outcome {
            Outcome::ComposerEdited { cursor } => {
                self.sync_editor(window, cx);
                self.move_composer_caret(*cursor, window, cx);
            }
            Outcome::Stashed { entry_id } => {
                let images = std::mem::take(&mut self.composer.stash_images);
                if images.is_empty() {
                    return;
                }
                let entry_id = entry_id.clone();
                self.spawn_task(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            super::attachments::encode_stash_images(images)
                        })
                        .await
                        .unwrap_or_default()
                    },
                    move |view, images, _, _| {
                        view.perform(Intent::FinalizeStashImages { entry_id, images });
                    },
                );
            }
            Outcome::StashRestored { images, warning } => {
                if let Some(warning) = warning {
                    window.push_notification(Notification::warning(warning.clone()), cx);
                }
                if images.is_empty() {
                    return;
                }
                let images = images.clone();
                let draft_key = self.snapshot.draft_key();
                self.spawn_task(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            super::attachments::stash_images_to_files(&images)
                        })
                        .await
                        .unwrap_or_default()
                    },
                    move |view, files, _, _| {
                        if !files.is_empty() {
                            view.perform(Intent::AttachFiles { draft_key, files });
                        }
                    },
                );
            }
            _ => {}
        }
    }

    pub(crate) fn focus_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.composer
            .editor
            .update(cx, |editor, cx| editor.focus(window, cx));
    }

    /// Wraps the chat area so files dropped anywhere over it attach to the
    /// composer's draft, with the drop hint while they hover.
    pub(crate) fn chat_drop_target(&self, body: AnyElement, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("chat-drop-target")
            .group("chat-drop-target")
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(body)
            .child(
                div()
                    .absolute()
                    .inset_2()
                    .invisible()
                    .group_drag_over::<ExternalPaths>("chat-drop-target", |style| style.visible())
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(18.))
                    .border_2()
                    .border_dashed()
                    .border_color(color("accent").opacity(0.6))
                    .bg(color("accent").opacity(0.035))
                    .child(
                        h_flex()
                            .gap_2()
                            .rounded_full()
                            .border_1()
                            .border_color(color("accent").opacity(0.25))
                            .bg(color("canvas").opacity(0.95))
                            .px_4()
                            .py(px(10.))
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .shadow_lg()
                            .child(
                                super::ui::icon("paperclip")
                                    .size(px(16.))
                                    .text_color(color("accent")),
                            )
                            .child("Drop files to attach"),
                    ),
            )
            .on_drop(cx.listener(|view, paths: &ExternalPaths, _, _| {
                if let Some(key) = view.composer_attachment_target() {
                    view.attach_paths(key, paths.paths().to_vec());
                }
            }))
            .into_any_element()
    }

    /// The shown composer's text and caret (UTF-16), where added context goes.
    pub(crate) fn composer_caret(&self, cx: &App) -> Option<(String, u32)> {
        self.shown_composer()?;
        Some(self.editor_text_and_cursor(cx))
    }

    /// The draft that picked or dropped files join: the answer being typed
    /// to a question, else the composer's.
    fn composer_attachment_target(&self) -> Option<String> {
        if let Some(thread) = shown_thread(&self.views, self.snapshot.selected_thread.as_ref())
            && let Some(questions) = &thread.requests.questions
            && let Some(active) = &questions.active
        {
            return Some(agent_core::view::api::answer_draft_key(
                questions.request_id.clone(),
                active.id.clone(),
            ));
        }
        self.shown_composer()
            .map(|composer| composer.draft_key.clone())
    }
}

/// Docks a composer stack to the bottom of the chat column.
fn dock(stack: Div) -> AnyElement {
    div()
        .w_full()
        .flex_none()
        .px(px(20.))
        .pt(px(8.))
        .pb(px(16.))
        .child(stack)
        .into_any_element()
}

//! The Files tab: the thread's folder on the Host and an editor for one file.
use super::PanelTab;
use crate::app::{
    Desktop,
    ui::{color, icon, icon_button, tint},
};
use agent_core::state::Intent;
use gpui_kit::{
    component::{
        Disableable, Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        input::{Editor, EditorState, InputEvent},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};

pub(super) struct FilesState {
    editor: Entity<EditorState>,
    /// The file the editor holds.
    path: Option<String>,
    value: String,
    revision: u64,
    /// The newest edit the Host has not acknowledged; until it lands, the
    /// snapshot's older text never replaces what the user typed.
    pending: Option<u64>,
    /// The folder listed when the tab opened.
    listed_for: Option<String>,
}
impl FilesState {
    pub(super) fn new(
        window: &mut Window,
        cx: &mut Context<Desktop>,
        subscriptions: &mut Vec<Subscription>,
    ) -> Self {
        let editor = cx.new(|cx| EditorState::new(window, cx));
        subscriptions.push(cx.subscribe(&editor, |view, input, event, cx| {
            if matches!(event, InputEvent::Change) {
                let text = input.read(cx).value().to_string();
                view.file_edited(text);
            }
        }));
        Self {
            editor,
            path: None,
            value: String::new(),
            revision: 0,
            pending: None,
            listed_for: None,
        }
    }
    pub(super) fn reset(&mut self) {
        self.path = None;
        self.value.clear();
        self.pending = None;
        self.listed_for = None;
    }
}

impl Desktop {
    fn file_edited(&mut self, text: String) {
        let state = &mut self.panels.files;
        let Some(path) = state.path.clone() else {
            return;
        };
        if text == state.value {
            return;
        }
        state.value = text.clone();
        state.revision += 1;
        let revision = state.revision;
        state.pending = Some(revision);
        self.perform_then(
            Intent::EditFile { path, text },
            move |view, result, window, cx| {
                if view.panels.files.pending == Some(revision) {
                    view.panels.files.pending = None;
                }
                if let Err(error) = result {
                    view.show_error(error, window, cx);
                }
            },
        );
    }

    /// Lists the thread's folder when the tab first shows it, and loads the
    /// open file's text, or its unsaved draft, into the editor.
    pub(super) fn sync_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = self.snapshot.cwd();
        if self.panels.shows(PanelTab::Files)
            && !cwd.is_empty()
            && self.panels.files.listed_for.as_ref() != Some(&cwd)
        {
            self.panels.files.listed_for = Some(cwd.clone());
            self.perform(Intent::ListFiles { path: cwd });
        }
        let Some(file) = self.snapshot.workspace.file.clone() else {
            return;
        };
        let text = self
            .snapshot
            .workspace
            .file_drafts
            .get(&file.path)
            .map_or(&file.text, |draft| &draft.text)
            .clone();
        let state = &mut self.panels.files;
        if state.path.as_ref() != Some(&file.path)
            || (state.pending.is_none() && state.value != text)
        {
            state.path = Some(file.path.clone());
            state.value = text.clone();
            state
                .editor
                .update(cx, |editor, cx| editor.set_value(text, window, cx));
        }
    }

    pub(super) fn render_files(&mut self, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let workspace = &self.snapshot.workspace;
        let directory = workspace.directory.clone();
        let listing = directory
            .as_ref()
            .map(|directory| directory.path.clone())
            .or_else(|| workspace.requested_directory.clone())
            .unwrap_or_else(|| self.snapshot.cwd());
        let loading = workspace.requested_directory.is_some()
            && directory.as_ref().map(|directory| &directory.path)
                != workspace.requested_directory.as_ref();
        let parent = directory
            .as_ref()
            .and_then(|directory| std::path::Path::new(&directory.path).parent())
            .map(|path| path.to_string_lossy().into_owned());
        let row = |id: SharedString, icon_name: &'static str, name: String, selected: bool| {
            h_flex()
                .id(id)
                .h_7()
                .w_full()
                .flex_shrink_0()
                .gap_2()
                .px_2()
                .rounded_md()
                .text_sm()
                .cursor_pointer()
                .when(selected, |row| row.bg(color("accentSurface")))
                .hover(|row| row.bg(tint("accentSurface", 0.6)))
                .child(icon(icon_name).size(px(14.)).text_color(color("textMuted")))
                .child(div().flex_1().min_w_0().truncate().child(name))
        };
        let open_path = self.panels.files.path.clone();
        let mut entries = v_flex()
            .id("files-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_1()
            .py_1();
        if let Some(path) = parent {
            entries = entries.child(
                row("files-parent".into(), "corner-left-up", "..".into(), false).on_click(
                    cx.listener(move |view, _, _, _| {
                        view.perform(Intent::ListFiles { path: path.clone() })
                    }),
                ),
            );
        }
        for entry in directory.iter().flat_map(|directory| &directory.entries) {
            let path = entry.path.clone();
            let directory = entry.directory;
            entries = entries.child(
                row(
                    SharedString::from(format!("files-entry-{}", entry.path)),
                    if directory { "folder" } else { "file" },
                    entry.name.clone(),
                    open_path.as_ref() == Some(&entry.path),
                )
                .on_click(cx.listener(move |view, _, _, _| {
                    view.perform(if directory {
                        Intent::ListFiles { path: path.clone() }
                    } else {
                        Intent::ReadFile {
                            path: path.clone(),
                            discard_draft: false,
                        }
                    })
                })),
            );
        }
        v_flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(
                h_flex()
                    .h_9()
                    .flex_shrink_0()
                    .gap_1()
                    .px_2()
                    .child(
                        icon_button("refresh-files", "refresh-cw", "Refresh files")
                            .loading(loading)
                            .on_click(cx.listener(move |view, _, _, _| {
                                let path = view
                                    .snapshot
                                    .workspace
                                    .directory
                                    .as_ref()
                                    .map(|directory| directory.path.clone())
                                    .unwrap_or_else(|| view.snapshot.cwd());
                                view.perform(Intent::ListFiles { path });
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(color("textMuted"))
                            .child(listing),
                    ),
            )
            .when(loading, |column| {
                column.child(
                    div()
                        .px_3()
                        .py_1()
                        .text_xs()
                        .text_color(color("textMuted"))
                        .child("Loading files…"),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .map(|list| {
                        if open_path.is_some() {
                            list.max_h(relative(0.4))
                        } else {
                            list.flex_1()
                        }
                    })
                    .child(entries),
            )
            .when_some(open_path, |column, path| {
                column.child(self.render_editor(path, cx))
            })
            .into_any_element()
    }

    fn render_editor(&self, path: String, cx: &mut Context<Self>) -> impl IntoElement {
        let edited = self.snapshot.workspace.file_drafts.contains_key(&path);
        let name = std::path::Path::new(&path)
            .file_name()
            .map_or_else(|| path.clone(), |name| name.to_string_lossy().into_owned());
        let save_path = path.clone();
        v_flex()
            .flex_1()
            .min_h_0()
            .border_t_1()
            .border_color(tint("border", 0.6))
            .child(
                h_flex()
                    .h_9()
                    .flex_shrink_0()
                    .gap_2()
                    .px_3()
                    .child(icon("file").size(px(14.)).text_color(color("textMuted")))
                    .child(
                        div()
                            .id("open-file-path")
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .tooltip(move |window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(path.clone())
                                    .build(window, cx)
                            })
                            .child(name),
                    )
                    .child(
                        Button::new("save-file")
                            .label("Save")
                            .primary()
                            .xsmall()
                            .disabled(!edited)
                            .on_click(cx.listener(move |view, _, _, _| {
                                view.perform(Intent::SaveFile {
                                    path: save_path.clone(),
                                })
                            })),
                    ),
            )
            .child(
                div().flex_1().min_h_0().child(
                    Editor::new(&self.panels.files.editor)
                        .bordered(false)
                        .h_full(),
                ),
            )
    }
}

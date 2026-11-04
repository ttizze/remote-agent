//! Shared menu content from T3's MarkdownFileLink and fileContextMenu.
//! Native clients render the result and the Host owns editor/reveal operations.
use crate::presentation::editors::{open_editor_label, reveal_label};
use agent_protocol::editors::{EditorId, FileManagerRevealKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilePrimaryAction {
    Open,
    Editor,
    Options,
}

/// Source files use the workbench; known binary files use a native viewer.
/// `path` is a resolved Host path without a line/column suffix.
pub fn desktop_file_primary_action(
    path: &str,
    workspace_root: &str,
    has_thread: bool,
    local_shell: bool,
    editor_modifier: bool,
) -> FilePrimaryAction {
    if local_shell && editor_modifier {
        return FilePrimaryAction::Editor;
    }
    if super::file_preview_mime_type(path.into()).is_some() {
        return FilePrimaryAction::Open;
    }
    let in_workspace =
        super::super::markdown::file_workspace_relative_path(path, workspace_root).is_some();
    let host_path = agent_protocol::paths::is_absolute_host_path(path)
        || path.starts_with("~/")
        || path.starts_with("~\\");
    if has_thread && (in_workspace || host_path) {
        FilePrimaryAction::Open
    } else if local_shell {
        FilePrimaryAction::Editor
    } else {
        FilePrimaryAction::Options
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileAction {
    OpenEditor { editor: Option<EditorId> },
    Reveal,
    CopyRelativePath,
    CopyFullPath,
    CopyMention,
    AddToChat,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileMenuItem {
    pub action: Option<FileAction>,
    pub label: String,
    pub children: Vec<FileMenuItem>,
}
impl FileMenuItem {
    fn action(action: FileAction, label: impl Into<String>) -> Self {
        Self {
            action: Some(action),
            label: label.into(),
            children: Vec::new(),
        }
    }
}

pub fn file_reference_menu(
    local_shell: bool,
    preferred_editor: Option<EditorId>,
    reveal: Option<FileManagerRevealKind>,
    workspace_relative: bool,
) -> Vec<FileMenuItem> {
    let mut items = Vec::new();
    if local_shell {
        items.push(FileMenuItem::action(
            FileAction::OpenEditor {
                editor: preferred_editor,
            },
            open_editor_label(preferred_editor),
        ));
        if let Some(kind) = reveal {
            items.push(FileMenuItem::action(FileAction::Reveal, reveal_label(kind)));
        }
    }
    if workspace_relative {
        items.push(FileMenuItem::action(
            FileAction::CopyRelativePath,
            "相対パスをコピー",
        ));
    }
    items.push(FileMenuItem::action(
        FileAction::CopyFullPath,
        "フルパスをコピー",
    ));
    items
}

pub fn file_tree_menu(
    has_absolute_path: bool,
    local_shell: bool,
    editors: &[EditorId],
    reveal: Option<FileManagerRevealKind>,
) -> Vec<FileMenuItem> {
    let mut items = Vec::new();
    if has_absolute_path && local_shell {
        if editors.contains(&EditorId::FileManager) {
            items.push(FileMenuItem::action(
                FileAction::OpenEditor {
                    editor: Some(EditorId::FileManager),
                },
                "開く",
            ));
            if let Some(kind) = reveal {
                items.push(FileMenuItem::action(FileAction::Reveal, reveal_label(kind)));
            }
        }
        let children = editors
            .iter()
            .copied()
            .filter(|editor| *editor != EditorId::FileManager)
            .map(|editor| {
                FileMenuItem::action(
                    FileAction::OpenEditor {
                        editor: Some(editor),
                    },
                    editor.label(),
                )
            })
            .collect::<Vec<_>>();
        if !children.is_empty() {
            items.push(FileMenuItem {
                action: None,
                label: "このアプリで開く".into(),
                children,
            });
        }
    }
    items.push(FileMenuItem::action(
        FileAction::CopyMention,
        "メンションをコピー",
    ));
    items.push(FileMenuItem::action(FileAction::AddToChat, "会話に追加"));
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn primary_clicks_keep_workspace_files_in_the_panel_and_modifier_clicks_in_the_editor() {
        for file in [
            "a.rs",
            "image.png",
            "movie.mp4",
            "voice.mp3",
            "paper.pdf",
            "bundle.zip",
            "installer.exe",
        ] {
            let path = format!("/workspace/{file}");
            assert_eq!(
                desktop_file_primary_action(&path, "/workspace", true, false, false),
                FilePrimaryAction::Open
            );
            assert_eq!(
                desktop_file_primary_action(&path, "/workspace", true, true, true),
                FilePrimaryAction::Editor
            );
            assert_eq!(
                desktop_file_primary_action(&path, "/workspace", true, false, true),
                FilePrimaryAction::Open
            );
        }
        assert_eq!(
            desktop_file_primary_action(
                r"C:\workspace\image.png",
                r"C:\workspace",
                true,
                false,
                false
            ),
            FilePrimaryAction::Open
        );
    }

    #[test]
    fn outside_media_uses_native_open_including_without_a_thread() {
        for path in ["~/voice.mp3", r"~\voice.mp3"] {
            assert_eq!(
                desktop_file_primary_action(path, "/workspace", true, false, false),
                FilePrimaryAction::Open
            );
        }
        for extension in [
            "avif", "bmp", "heic", "heif", "gif", "ico", "jpeg", "jpg", "png", "svg", "tif",
            "tiff", "webp", "avi", "m4v", "mkv", "mov", "mp4", "ogv", "webm",
        ] {
            let path = format!("/other/file.{}", extension.to_uppercase());
            assert_eq!(
                desktop_file_primary_action(&path, "/workspace", true, true, false),
                FilePrimaryAction::Open
            );
            assert_eq!(
                desktop_file_primary_action(&path, "/workspace", true, false, false),
                FilePrimaryAction::Open
            );
        }
        for local in [false, true] {
            assert_eq!(
                desktop_file_primary_action(
                    "/outside/video.mp4",
                    "/workspace",
                    false,
                    local,
                    false
                ),
                FilePrimaryAction::Open
            );
        }
        for file in [
            "a.rs",
            "voice.mp3",
            "voice.flac",
            "paper.pdf",
            "bundle.zip",
            "icon.icns",
        ] {
            assert_eq!(
                desktop_file_primary_action(
                    &format!("/other/{file}"),
                    "/workspace",
                    true,
                    false,
                    false
                ),
                FilePrimaryAction::Open
            );
        }
        assert_eq!(
            desktop_file_primary_action("/other/a.rs", "/workspace", false, true, false),
            FilePrimaryAction::Editor
        );
        assert_eq!(
            desktop_file_primary_action("/other/a.rs", "/workspace", false, false, false),
            FilePrimaryAction::Options
        );
    }
    #[test]
    fn conversation_menu_has_t3_order_and_omits_unavailable_shell_actions() {
        let menu = file_reference_menu(
            true,
            Some(EditorId::VsCode),
            Some(FileManagerRevealKind::Finder),
            true,
        );
        assert_eq!(
            menu.iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            [
                "VS Codeで開く",
                "Finderで表示",
                "相対パスをコピー",
                "フルパスをコピー"
            ]
        );
        let remote = file_reference_menu(
            false,
            Some(EditorId::VsCode),
            Some(FileManagerRevealKind::Finder),
            false,
        );
        assert_eq!(
            remote,
            vec![FileMenuItem::action(
                FileAction::CopyFullPath,
                "フルパスをコピー"
            )]
        );
        assert_eq!(
            file_reference_menu(true, None, None, true)[0].label,
            "エディターで開く"
        );
    }
    #[test]
    fn tree_menu_uses_file_manager_for_open_and_excludes_it_from_the_submenu() {
        let menu = file_tree_menu(
            true,
            true,
            &[EditorId::FileManager, EditorId::Zed, EditorId::VsCode],
            Some(FileManagerRevealKind::FileExplorer),
        );
        assert_eq!(
            menu.iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            [
                "開く",
                "エクスプローラーで表示",
                "このアプリで開く",
                "メンションをコピー",
                "会話に追加"
            ]
        );
        assert_eq!(
            menu[0].action,
            Some(FileAction::OpenEditor {
                editor: Some(EditorId::FileManager)
            })
        );
        assert_eq!(
            menu[2]
                .children
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            ["Zed", "VS Code"]
        );
        for (absolute, local) in [(false, true), (true, false), (false, false)] {
            assert_eq!(
                file_tree_menu(
                    absolute,
                    local,
                    &[EditorId::FileManager, EditorId::Zed],
                    Some(FileManagerRevealKind::Finder)
                )
                .len(),
                2
            );
        }
        let no_manager = file_tree_menu(
            true,
            true,
            &[EditorId::Zed],
            Some(FileManagerRevealKind::Finder),
        );
        assert_eq!(no_manager[0].label, "このアプリで開く");
        assert_eq!(file_tree_menu(true, true, &[], None).len(), 2);
    }
}

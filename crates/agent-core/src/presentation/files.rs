mod actions;
mod chip_color;
pub use chip_color::{desktop_file_chip_color, mobile_file_chip_color};
mod editor;
mod explorer;
pub use editor::{FileEditorPresentation, file_edit_prompt, file_line_start_utf16};
pub use explorer::{
    EXPLORER_CONTENT_MIN_WIDTH, EXPLORER_DEFAULT_WIDTH, EXPLORER_MIN_WIDTH, explorer_max_width,
    explorer_width,
};
mod icons;
mod metadata;
mod mobile;
mod mobile_icons;
mod preview;
pub use mobile::{
    MobileFileAction, MobileFileMenuItem, mobile_file_menu, mobile_file_open_request,
};
pub use preview::{file_preview_mime_type, image_extension, is_image_file};
mod rendered;
pub use rendered::{file_markdown_blocks, file_renders_markdown, is_markdown_preview_file};

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn mobile_file_icon(path: String) -> String {
    mobile_icons::icon(&path).into()
}

pub use metadata::FileMetadataCache;
pub mod tree;
pub use actions::{
    FileAction, FileMenuItem, FilePrimaryAction, desktop_file_primary_action, file_reference_menu,
    file_tree_menu,
};
pub use icons::{desktop_file_icon, desktop_metadata_icon};

pub fn file_reference_tooltip(path: &str, byte_length: Option<u64>) -> String {
    let Some(bytes) = byte_length else {
        return path.into();
    };
    let size = if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.)
    } else {
        format!("{} KB", bytes.div_ceil(1_024).max(1))
    };
    format!("{path} · {size}")
}
pub use tree::{
    FileTreeAction, FileTreeKey, FileTreeRequest, FileTreeRow, FileTreeSurface, FileTreeView,
    FileTrees, file_tree_path, file_tree_toggle_all_label,
};

/// The desktop client's composer file link, shared by menus and dragging.
pub fn file_mention(path: &str) -> String {
    let name = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]");
    format!("[{name}]({})", link_destination(path))
}

/// `path` as a Markdown link's destination, which reads back as the same path.
fn link_destination(path: &str) -> String {
    use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
    const DESTINATION: &percent_encoding::AsciiSet = &NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'_')
        .remove(b'.')
        .remove(b'!')
        .remove(b'~')
        .remove(b'*')
        .remove(b'\'')
        .remove(b';')
        .remove(b'/')
        .remove(b':')
        .remove(b'@')
        .remove(b'&')
        .remove(b'=')
        .remove(b'+')
        .remove(b'$')
        .remove(b',');
    utf8_percent_encode(path, DESTINATION).to_string()
}

/// The desktop client's explorer follows the workspace, except when displaying a Host folder.
/// An absolute file outside the workspace still leaves its explorer at cwd.
pub fn desktop_file_tree_root(workspace: &str, selected: Option<&str>, directory: bool) -> String {
    selected
        .filter(|path| {
            directory && super::markdown::file_workspace_relative_path(path, workspace).is_none()
        })
        .unwrap_or(workspace)
        .into()
}

pub fn file_tree_mention(root: &str, workspace: &str, path: &str) -> String {
    let host_folder = super::markdown::file_workspace_relative_path(root, workspace).is_none();
    file_mention(&if host_folder {
        file_tree_path(root, path)
    } else {
        path.into()
    })
}

pub fn append_file_mentions(text: &str, mentions: &str) -> String {
    let boundary = if text.chars().last().is_some_and(|ch| !ch.is_whitespace()) {
        " "
    } else {
        ""
    };
    format!("{text}{boundary}{mentions} ")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_tooltips_use_the_full_path_and_t3_size_rounding() {
        assert_eq!(file_reference_tooltip("/a", None), "/a");
        for (bytes, size) in [
            (0, "1 KB"),
            (1_024, "1 KB"),
            (1_025, "2 KB"),
            (1_048_575, "1024 KB"),
            (1_048_576, "1.0 MB"),
            (1_572_864, "1.5 MB"),
        ] {
            assert_eq!(
                file_reference_tooltip("C:\\a", Some(bytes)),
                format!("C:\\a · {size}")
            );
        }
    }
    #[test]
    fn mentions_escape_markdown_and_uri_destinations_without_losing_host_paths() {
        assert_eq!(
            file_mention("src/a [b](c)#?.rs"),
            "[a \\[b\\](c)#?.rs](src/a%20%5Bb%5D%28c%29%23%3F.rs)"
        );
        assert_eq!(
            file_mention("C:\\日本語\\a.rs"),
            "[a.rs](C:%5C%E6%97%A5%E6%9C%AC%E8%AA%9E%5Ca.rs)"
        );
        assert_eq!(
            file_tree_mention("/workspace", "/workspace", "src/a.rs"),
            "[a.rs](src/a.rs)"
        );
        assert_eq!(
            file_tree_mention("/other", "/workspace", "a.rs"),
            "[a.rs](/other/a.rs)"
        );
        assert_eq!(append_file_mentions("draft", "[a](a)"), "draft [a](a) ");
        assert_eq!(append_file_mentions("draft\n", "[a](a)"), "draft\n[a](a) ");
    }
    #[test]
    fn the_explorer_follows_the_workspace_unless_it_shows_a_host_folder() {
        let root = |selected, directory| desktop_file_tree_root("/repo", selected, directory);
        assert_eq!(root(None, true), "/repo");
        assert_eq!(root(Some("/repo/src"), true), "/repo");
        assert_eq!(root(Some("/other"), true), "/other");
        assert_eq!(root(Some("/other/a.rs"), false), "/repo");
    }
}

#[cfg(feature = "bindings")]
#[derive(Clone, uniffi::Object)]
pub struct Files {
    pub(crate) resources: crate::state::Files,
    pub(crate) selection: std::sync::Arc<crate::state::WorkspaceLocation>,
}

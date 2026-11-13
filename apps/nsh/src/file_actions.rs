//! File chips and file menus on every NSH platform, as T3 Code's file links and
//! fileContextMenu: Core decides a chip's glyph, accent and tooltip and the
//! menus' items; the Host opens editors and file managers.
use crate::l10n::tr;
use crate::{
    Connection,
    files::{Browser, Reference},
};
#[cfg(any(desktop, test))]
use agent_core::presentation::files::file_tree_menu;
#[cfg(desktop)]
use agent_core::presentation::files::{FileTreeKey, file_tree_mention, file_tree_path};
use agent_core::{
    presentation::{
        editors::preferred_editor,
        files::{
            FileAction, FileMenuItem, FileMetadataCache, FilePrimaryAction, append_file_mentions,
            desktop_file_chip_color, desktop_file_primary_action, desktop_metadata_icon,
            file_reference_menu, file_reference_tooltip,
        },
        markdown::{
            MarkdownFileReference, MarkdownFileSurface, file_host_path, file_path_for_clipboard,
            file_position_target, file_workspace_relative_path, markdown_file_target,
        },
    },
    state::{Intent, operations as op},
};
use agent_protocol::{
    editors::{EditorId, FileManagerRevealKind},
    file_metadata::FileMetadataKind,
};
use nsh_core::keymap::Modifiers;
use nsh_core::prelude::*;
use serde_json::{Value, json};

/// The surfaces a chip stands on in the dark and light looks, which its accent
/// stays readable on.
const DARK_SURFACES: [u32; 3] = [0x080808, 0x0f0f0f, 0x141414];
const LIGHT_SURFACES: [u32; 3] = [0xffffff, 0xfafafa, 0xf4f4f5];

/// The editor-opening modifier follows the platform's primary shortcut key.
pub(crate) fn primary_modifier(modifiers: Modifiers, apple: bool) -> bool {
    if apple { modifiers.cmd } else { modifiers.ctrl }
}

/// What a message's chips read from their connection: the workspace, the
/// Host's editors when it is this PC, and what the Host reported of each file.
#[derive(Clone, PartialEq)]
pub(crate) struct Chips {
    root: String,
    local: bool,
    editor: Option<EditorId>,
    reveal: Option<FileManagerRevealKind>,
    metadata: FileMetadataCache,
}

impl Chips {
    pub(crate) fn read(connection: Connection) -> Self {
        let local = connection.host.with(|host| host == "local");
        connection.publication.with(|p| {
            let capabilities = p.snapshot.files.file_open_capabilities.as_deref();
            Self {
                root: file_host_path(p.composer.controls.navigation.cwd.clone(), p.context.home()),
                local,
                editor: preferred_editor(
                    p.snapshot.files.preferred_editor,
                    capabilities.map_or(&[], |c| c.editors.as_slice()),
                ),
                reveal: capabilities.and_then(|c| c.reveal),
                metadata: p.snapshot.files.workspace.file_metadata.clone(),
            }
        })
    }

    /// The chip a message's file run shows: its glyph (a folder's, or the
    /// file's refined by its sniffed type), its full path and size, its menu
    /// and its accent in either look.
    pub(crate) fn chip(&self, file: &MarkdownFileReference) -> Value {
        let metadata = self
            .metadata
            .get(&file.path)
            .and_then(|entry| entry.metadata.as_ref());
        let folder = metadata.is_some_and(|metadata| metadata.kind == FileMetadataKind::Directory);
        let glyph = desktop_metadata_icon(
            &file.path,
            metadata.and_then(|metadata| metadata.mime_type.as_deref()),
        );
        json!({
            "icon": (!folder).then(|| crate::files::glyph(glyph)),
            "folder": folder,
            "tooltip": file_reference_tooltip(
                &file_position_target(&file.path, file.line, file.column),
                metadata.and_then(|metadata| metadata.byte_length),
            ),
            "menu": menu(self.reference_menu(&file.path)),
            "darkColor": desktop_file_chip_color(&file.path, &DARK_SURFACES),
            "lightColor": desktop_file_chip_color(&file.path, &LIGHT_SURFACES),
        })
    }

    fn reference_menu(&self, path: &str) -> Vec<FileMenuItem> {
        file_reference_menu(
            crate::l10n::language(),
            self.local,
            self.editor,
            self.reveal,
            file_workspace_relative_path(path, &self.root).is_some(),
        )
    }
}

/// `items` as `ui::popup` and the message text's menu read them: an item's id
/// is its action, serialized.
pub(crate) fn menu(items: Vec<FileMenuItem>) -> Value {
    Value::Array(
        items
            .into_iter()
            .map(|item| {
                let title = item.label;
                match item.action {
                    Some(action) => json!({"id": id(action), "title": title}),
                    None => json!({"title": title, "children": menu(item.children)}),
                }
            })
            .collect(),
    )
}

fn id(action: FileAction) -> String {
    serde_json::to_string(&action).expect("file actions serialize")
}

fn action(id: &str) -> Option<FileAction> {
    serde_json::from_str(id).ok()
}

/// Asks the Host about the chips' files, and once about this PC's editors.
pub(crate) fn read_metadata(connection: Connection, paths: Vec<String>) {
    if connection.host.with_untracked(|host| host == "local")
        && connection.publication.with_untracked(|p| {
            p.snapshot.connection.connected && !p.snapshot.files.file_open_capabilities_requested
        })
    {
        connection.dispatch(Intent::ReadFileOpenCapabilities(
            op::ReadFileOpenCapabilities {},
        ));
    }
    if !paths.is_empty() {
        connection.dispatch(Intent::EnsureFileMetadata {
            paths,
            now_ms: chrono::Utc::now().timestamp_millis().max(0) as u64,
        });
    }
}

/// Where a message's link was: its Host and the workspace it resolves against.
#[derive(Clone)]
pub(crate) struct LinkTarget {
    pub link: String,
    pub cwd: String,
    pub home: Option<String>,
    pub host: String,
}

/// A press on a link of a message: the desktop client's primary action for a file, which
/// the editor takes with ⌘ (Ctrl elsewhere) on this PC; any other link opens.
pub(crate) fn press(connection: Connection, files: Browser, target: LinkTarget, modifier: bool) {
    let root = file_host_path(target.cwd.clone(), target.home.clone());
    let Some(file) = markdown_file_target(
        target.link.clone(),
        root.clone(),
        MarkdownFileSurface::Desktop,
    ) else {
        return files.open(
            target.link,
            target.cwd,
            target.home,
            target.host,
            connection.client.get(),
        );
    };
    let path = file_host_path(file.path, target.home.clone());
    let local = target.host == "local";
    match desktop_file_primary_action(&path, &root, true, local, modifier) {
        FilePrimaryAction::Open => files.open(
            target.link,
            target.cwd,
            target.home,
            target.host,
            connection.client.get(),
        ),
        FilePrimaryAction::Editor => run_reference(
            connection,
            files,
            target,
            FileAction::OpenEditor { editor: None },
        ),
        FilePrimaryAction::Options => {
            let items = Chips::read(connection).reference_menu(&path);
            crate::ui::popup(&menu(items), move |id: &str| {
                if let Some(action) = action(id) {
                    run_reference(connection, files, target.clone(), action);
                }
            });
        }
    }
}

/// The item `id` of a chip's menu, on the file the Host resolves its link to.
pub(crate) fn choose(connection: Connection, files: Browser, target: LinkTarget, id: &str) {
    if let Some(action) = action(id) {
        run_reference(connection, files, target, action);
    }
}

fn run_reference(connection: Connection, files: Browser, target: LinkTarget, action: FileAction) {
    let root = file_host_path(target.cwd.clone(), target.home.clone());
    let host = target.host.clone();
    files.resolve(
        target.link,
        target.cwd,
        target.home,
        target.host,
        connection.client.get(),
        move |file| {
            let relative = file_workspace_relative_path(&file.path, &root);
            act(connection, &host, &file, relative.as_deref(), "", action);
        },
    );
}

/// The menu of a tree row: open it here or in an editor of this PC, reveal
/// it, and mention it in the chat.
#[cfg(desktop)]
pub(crate) fn tree_menu(connection: Connection, key: &FileTreeKey, path: &str) {
    let host = connection.host.get_untracked();
    let (cwd, editors, reveal) = connection.publication.with_untracked(|p| {
        let capabilities = p.snapshot.files.file_open_capabilities.as_deref();
        (
            p.composer.controls.navigation.cwd.clone(),
            capabilities.map_or_else(Vec::new, |c| c.editors.clone()),
            capabilities.and_then(|c| c.reveal),
        )
    });
    let items = file_tree_menu(
        crate::l10n::language(),
        true,
        host == "local",
        &editors,
        reveal,
    );
    let relative = file_workspace_relative_path(&file_tree_path(&key.root, path), &key.root)
        .unwrap_or_default();
    let mention = file_tree_mention(&key.root, &cwd, &relative);
    let file = Reference {
        path: file_tree_path(&key.root, path),
        line: None,
        column: None,
    };
    crate::ui::popup(&menu(items), move |id: &str| {
        if let Some(action) = action(id) {
            act(connection, &host, &file, Some(&relative), &mention, action);
        }
    });
}

/// The mentions of tree paths under `key`'s root, as dragging them writes.
#[cfg(desktop)]
pub(crate) fn tree_mentions(connection: Connection, key: &FileTreeKey, paths: &[String]) -> String {
    let cwd = connection
        .composer
        .with_untracked(|c| c.controls.navigation.cwd.clone());
    paths
        .iter()
        .map(|path| file_tree_mention(&key.root, &cwd, path))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Appends `mentions` to the visible draft and returns to the composer.
pub(crate) fn add_mentions(connection: Connection, mentions: &str) {
    let (draft, text) = connection.composer.with_untracked(|c| {
        (
            c.controls.navigation.draft_key.clone(),
            append_file_mentions(&c.draft.text, mentions),
        )
    });
    connection.dispatch(Intent::SetDraftText {
        thread_id: draft,
        text,
    });
    connection.composer_ref.focus();
}

fn act(
    connection: Connection,
    host: &str,
    file: &Reference,
    relative: Option<&str>,
    mention: &str,
    action: FileAction,
) {
    let open = |editor: EditorId, reveal: bool| {
        connection.client.with(|client| {
            client.dispatch(
                host,
                None,
                Intent::OpenInEditor(op::OpenInEditor {
                    path: file.path.clone(),
                    line: file.line.filter(|_| !reveal),
                    column: file.column.filter(|_| !reveal),
                    editor,
                    reveal,
                }),
            )
        })
    };
    let copy = |path: &str| {
        set_clipboard(file_path_for_clipboard(
            &file_position_target(path, file.line, file.column),
            &file.path,
        ))
    };
    match action {
        FileAction::OpenEditor { editor } => {
            let saved = connection.publication.with_untracked(|p| {
                let available = p
                    .snapshot
                    .files
                    .file_open_capabilities
                    .as_deref()
                    .map_or(&[][..], |c| c.editors.as_slice());
                preferred_editor(p.snapshot.files.preferred_editor, available)
            });
            match editor.or(saved) {
                Some(editor) => open(editor, false),
                None => crate::native::message(
                    tr("ファイル"),
                    tr("インストール済みのエディターが見つかりません"),
                ),
            }
        }
        FileAction::Reveal => open(EditorId::FileManager, true),
        FileAction::CopyRelativePath => {
            if let Some(relative) = relative {
                copy(relative)
            }
        }
        FileAction::CopyFullPath => copy(&file.path),
        FileAction::CopyMention => set_clipboard(mention.to_owned()),
        FileAction::AddToChat => add_mentions(connection, mention),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_editor_modifier_is_command_on_apple_and_control_elsewhere() {
        let command = Modifiers {
            cmd: true,
            ..Default::default()
        };
        let control = Modifiers {
            ctrl: true,
            ..Default::default()
        };
        assert!(primary_modifier(command, true));
        assert!(!primary_modifier(control, true));
        assert!(primary_modifier(control, false));
        assert!(!primary_modifier(command, false));
    }

    #[test]
    fn menu_ids_round_trip_their_actions_and_submenus_keep_their_items() {
        let items = file_tree_menu(
            crate::l10n::language(),
            true,
            true,
            &[EditorId::FileManager, EditorId::VsCode],
            Some(FileManagerRevealKind::Finder),
        );
        let value = menu(items);
        let entries = value.as_array().unwrap();
        assert_eq!(
            action(entries[0]["id"].as_str().unwrap()),
            Some(FileAction::OpenEditor {
                editor: Some(EditorId::FileManager)
            })
        );
        let submenu = entries
            .iter()
            .find(|entry| entry.get("children").is_some())
            .unwrap();
        assert_eq!(
            action(submenu["children"][0]["id"].as_str().unwrap()),
            Some(FileAction::OpenEditor {
                editor: Some(EditorId::VsCode)
            })
        );
        assert_eq!(
            entries.last().unwrap()["id"].as_str().and_then(action),
            Some(FileAction::AddToChat)
        );
        assert_eq!(action("not an action"), None);
    }

    #[test]
    fn chips_name_folders_sizes_and_the_relative_copy_only_inside_the_workspace() {
        let chips = Chips {
            root: "/repo".into(),
            local: false,
            editor: None,
            reveal: None,
            metadata: FileMetadataCache::default(),
        };
        let file = MarkdownFileReference {
            path: "/repo/src/main.rs".into(),
            label: "main.rs".into(),
            kind: agent_core::presentation::markdown::MarkdownFileKind::Code,
            line: Some(4),
            column: None,
        };
        let chip = chips.chip(&file);
        assert_eq!(chip["tooltip"], "/repo/src/main.rs:4");
        assert_eq!(chip["folder"], false);
        use base64::Engine;
        let png = base64::engine::general_purpose::STANDARD
            .decode(chip["icon"].as_str().unwrap())
            .unwrap();
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(png.len() > 100);
        assert_eq!(
            crate::files::glyph("unknown-catalog-token"),
            crate::files::glyph("file-tree-builtin-default"),
        );
        let titles: Vec<_> = chip["menu"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["title"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(titles, [tr("相対パスをコピー"), tr("フルパスをコピー")]);
        let outside = MarkdownFileReference {
            path: "/other/a.rs".into(),
            ..file
        };
        assert_eq!(chips.chip(&outside)["menu"].as_array().unwrap().len(), 1);
        assert_ne!(chip["darkColor"], chip["lightColor"]);
    }
}

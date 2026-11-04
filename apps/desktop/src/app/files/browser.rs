use super::*;
use agent_core::presentation::{
    files::{FileTreeAction, FileTreeKey, FileTreeRequest, FileTreeSurface, FileTreeView},
    markdown::file_workspace_relative_path,
};

#[derive(Clone)]
pub(in crate::app) struct FileBrowserModel {
    pub key: FileTreeKey,
    pub tree: Arc<FileTreeView>,
    pub scroll: UniformListScrollHandle,
}
impl PartialEq for FileBrowserModel {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && Arc::ptr_eq(&self.tree, &other.tree)
    }
}
pub(in crate::app) struct FileBrowser {
    pub model: FileBrowserModel,
    revision: u64,
    scope: PanelScope,
    selected_panel: Option<String>,
    reveal: Option<(u64, Option<String>)>,
    opened: u64,
    menu: u64,
    pub bounds: Bounds<Pixels>,
}

impl Desktop {
    pub(in crate::app) fn close_file_browser(&mut self) {
        if let Some(browser) = self.file_browser.take() {
            self.dispatch(Intent::FileTree {
                key: browser.model.key,
                action: FileTreeAction::Close,
            });
        }
    }
    pub(in crate::app) fn file_tree_action(&self, action: FileTreeAction) {
        if let Some(browser) = &self.file_browser {
            self.dispatch(Intent::FileTree {
                key: browser.model.key.clone(),
                action,
            });
        }
    }
    // External navigation invalidates pending tree UI requests in the same owner.
    // The following projection must not reopen a previously selected file.
    pub(in crate::app) fn reveal_file_in_tree(&mut self, path: Option<&str>, directory: bool) {
        let Some(browser) = &mut self.file_browser else {
            return;
        };
        let relative = path
            .and_then(|path| file_workspace_relative_path(path, &browser.model.key.root))
            .filter(|path| agent_protocol::file_tree::valid_tree_path(path));
        browser.selected_panel = path.map(str::to_owned);
        browser.reveal = Some((browser.model.tree.reveal_id, relative.clone()));
        browser.opened = browser
            .model
            .tree
            .open_file
            .as_ref()
            .map_or(0, |request| request.id);
        browser.menu = browser
            .model
            .tree
            .context_menu
            .as_ref()
            .map_or(0, |request| request.id);
        self.file_tree_action(FileTreeAction::Reveal {
            path: relative,
            directory,
        });
    }
    pub(in crate::app) fn sync_file_browser(
        &mut self,
        visible: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = self.panels.get(self.panel_scope());
        let workspace = agent_core::presentation::markdown::file_host_path(
            self.snapshot.navigation.cwd.clone(),
            self.snapshot.host_home_directory.clone(),
        );
        if !visible || workspace.is_empty() {
            self.close_file_browser();
            return;
        }
        let selected = panel.selected().and_then(|surface| surface.path.clone());
        let folder = panel.kind() == Panel::Files;
        let root = agent_core::presentation::files::desktop_file_tree_root(
            &workspace,
            selected.as_deref(),
            folder,
        );
        let scope = self.panel_scope();
        if self
            .file_browser
            .as_ref()
            .is_none_or(|browser| browser.model.key.root != root || browser.scope != scope)
        {
            self.close_file_browser();
            let key = FileTreeKey {
                owner: format!("desktop-files-{}", uuid::Uuid::new_v4()),
                root,
            };
            self.file_browser = Some(FileBrowser {
                model: FileBrowserModel {
                    key: key.clone(),
                    tree: Arc::default(),
                    scroll: UniformListScrollHandle::default(),
                },
                revision: u64::MAX,
                scope,
                selected_panel: None,
                reveal: None,
                opened: 0,
                menu: 0,
                bounds: Bounds::default(),
            });
            self.file_tree_action(FileTreeAction::Open {
                surface: FileTreeSurface::Desktop,
                locale: String::new(),
                workspace_root: workspace,
            });
        }
        if self.file_browser.as_ref().unwrap().selected_panel != selected {
            self.reveal_file_in_tree(selected.as_deref(), folder);
        }
        let key = self.file_browser.as_ref().unwrap().model.key.clone();
        let revision = self.snapshot.workspace.file_trees.revision();
        if self.file_browser.as_ref().unwrap().revision != revision {
            let tree = self.snapshot.file_tree(key.clone());
            let browser = self.file_browser.as_mut().unwrap();
            if self.file_search.read(cx).value().as_ref() != tree.query {
                self.file_search.update(cx, |input, cx| {
                    input.set_value(tree.query.clone(), window, cx)
                });
            }
            let waiting_reveal = browser
                .reveal
                .as_ref()
                .is_some_and(|(previous, _)| *previous == tree.reveal_id);
            if !waiting_reveal {
                if let Some((_, path)) = &browser.reveal {
                    if let Some(index) = path.as_deref().and_then(|path| tree.index_for_path(path))
                    {
                        browser
                            .model
                            .scroll
                            .scroll_to_item(index, ScrollStrategy::Center);
                        browser.reveal = None;
                        self.panel_focus.focus(window, cx);
                    } else if path.is_none() {
                        browser.reveal = None;
                    }
                } else if browser.model.tree.focused_path != tree.focused_path
                    && let Some(index) = tree
                        .focused_path
                        .as_deref()
                        .and_then(|path| tree.index_for_path(path))
                {
                    browser
                        .model
                        .scroll
                        .scroll_to_item(index, ScrollStrategy::Nearest);
                }
            }
            let open = tree.open_file.clone();
            let menu = tree.context_menu.clone();
            if *browser.model.tree != tree {
                browser.model.tree = Arc::new(tree);
            }
            browser.revision = revision;
            if !waiting_reveal && let Some(request) = open {
                self.open_file_from_tree(&key, &request, cx);
            }
            if !waiting_reveal && let Some(request) = menu {
                self.open_tree_keyboard_menu(&key, &request, window, cx);
            }
        }
    }
    pub(in crate::app) fn open_file_from_tree(
        &mut self,
        key: &FileTreeKey,
        request: &FileTreeRequest,
        cx: &mut Context<Self>,
    ) {
        let Some(browser) = self
            .file_browser
            .as_mut()
            .filter(|browser| &browser.model.key == key && browser.opened != request.id)
        else {
            return;
        };
        browser.opened = request.id;
        browser.reveal = None;
        let path = agent_core::presentation::files::file_tree_path(&key.root, &request.path);
        if self.open_file(path.clone(), None, false, cx) {
            self.file_browser.as_mut().unwrap().selected_panel = Some(path);
        }
        cx.notify();
    }
    fn open_tree_keyboard_menu(
        &mut self,
        key: &FileTreeKey,
        request: &FileTreeRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(browser) = self
            .file_browser
            .as_mut()
            .filter(|browser| &browser.model.key == key && browser.menu != request.id)
        else {
            return;
        };
        browser.menu = request.id;
        let Some(index) = browser.model.tree.index_for_path(&request.path) else {
            return;
        };
        let position = point(
            browser.bounds.left() + px(16.),
            browser.bounds.top()
                + px((index + 1) as f32 * 24.)
                + browser.model.scroll.0.borrow().base_handle.offset().y,
        );
        self.show_tree_file_menu(key, &request.path, position, window, cx);
    }
    pub(in crate::app) fn select_file_tree_row(
        &self,
        key: &FileTreeKey,
        path: &str,
        additive: bool,
        range: bool,
        cx: &mut Context<Self>,
    ) {
        if self
            .file_browser
            .as_ref()
            .is_some_and(|browser| &browser.model.key == key)
        {
            self.file_tree_action(FileTreeAction::Click {
                path: path.into(),
                additive,
                range,
            });
            cx.notify();
        }
    }
    pub(in crate::app) fn file_tree_key(
        &mut self,
        event: &KeyDownEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(browser) = &self.file_browser else {
            return;
        };
        let key = if event.keystroke.key == "f10" && event.keystroke.modifiers.shift {
            "contextmenu"
        } else {
            event.keystroke.key.as_str()
        };
        let additive = event.keystroke.modifiers.control || event.keystroke.modifiers.platform;
        let shift = event.keystroke.modifiers.shift;
        if browser.model.tree.handles_desktop_key(key, additive, shift) {
            self.file_tree_action(FileTreeAction::Keyboard {
                key: key.into(),
                additive,
                shift,
            });
            cx.stop_propagation();
            cx.notify();
        }
    }
}

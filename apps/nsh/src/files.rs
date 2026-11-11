use crate::l10n::{localized, tr};
use crate::{
    Connection,
    client::Client,
    media::Preview,
    native,
    ui::{self, Icon, colors},
    workbench::Workbench,
};
use agent_core::{
    panels::{PanelKind, PanelScope},
    presentation::files::{
        FileTreeAction, FileTreeKey, FileTreeRow, FileTreeSurface, desktop_file_icon,
        file_preview_mime_type, file_tree_path, mobile_file_open_request,
    },
    state::Intent,
    store::Outcome,
};
use agent_protocol::file_tree::EntryKind;
use nsh_core::{
    element::{Rgba, rgb, rgba},
    prelude::*,
};
use std::{cell::RefCell, rc::Rc, sync::Arc};

#[cfg(desktop)]
mod desktop;

#[derive(Clone, Copy)]
pub(crate) struct Browser {
    pub root: Memo<Option<String>>,
    pub file: Memo<Option<TextFile>>,
    workspace: Workbench,
    target: Memo<(PanelScope, (bool, Option<u64>))>,
    preview: Preview,
    owner: Owner,
}

#[derive(Clone, PartialEq)]
pub(crate) struct TextFile {
    pub id: u64,
    pub path: String,
    pub line: Option<u32>,
}

impl Browser {
    pub fn close(self) {
        self.workspace.hide();
        self.preview.close();
    }

    pub fn show(self, path: String) {
        self.preview.close();
        self.workspace.open(PanelKind::Files, Some(path), None);
    }

    pub fn new(preview: Preview) -> Self {
        let workspace = use_context::<Workbench>().unwrap();
        let connection = use_context::<Connection>().unwrap();
        let line = computed(move || connection.state.with(|state| state.file_line));
        let root = computed(move || {
            workspace.state.with(|state| {
                state
                    .surfaces
                    .iter()
                    .rev()
                    .find(|surface| surface.kind == PanelKind::Files)
                    .and_then(|surface| surface.path.clone())
            })
        });
        let file = computed(move || {
            workspace.state.with(|state| {
                let surface = state
                    .selected()
                    .filter(|surface| state.open && surface.kind == PanelKind::File)?;
                Some(TextFile {
                    id: surface.id,
                    path: surface.path.clone()?,
                    line: line
                        .get()
                        .filter(|(id, _)| *id == surface.id)
                        .and_then(|(_, line)| line),
                })
            })
        });
        let target = computed(move || {
            (
                workspace.scope.get(),
                workspace.state.with(|state| (state.open, state.active)),
            )
        });
        effect(move || {
            target.get();
            preview.close();
        });
        Self {
            root,
            file,
            workspace,
            target,
            preview,
            owner: Owner::current(),
        }
    }

    pub fn open(
        self,
        source: String,
        cwd: String,
        home: Option<String>,
        host: String,
        client: Arc<Client>,
    ) {
        let line = position(&source, &cwd).0;
        let Some(request) = mobile_file_open_request(source.clone(), cwd, home) else {
            if !source.starts_with('#') {
                spawn(async move {
                    if !open_url(source).await {
                        native::message(tr("リンク"), tr("このリンクを開けませんでした。"));
                    }
                });
            }
            return;
        };
        let token = self.preview.begin();
        let target = self.target.get_untracked();
        let resolve = client.request(&host, None, Intent::ResolveFileReference(request));
        self.owner.run(move || {
            spawn(async move {
                let result = resolve
                    .recv()
                    .await
                    .map_err(|reason| reason.to_string())
                    .and_then(|result| result.map_err(|reason| reason.to_string()));
                if !self.preview.is_pending(token) || self.target.get_untracked() != target {
                    return;
                }
                match result {
                    Ok(Outcome::FileReferenceResolved {
                        path,
                        directory: true,
                    }) => self.show(path),
                    Ok(Outcome::FileReferenceResolved {
                        path,
                        directory: false,
                    }) => self.open_file(path, line, host, client),
                    Err(reason) => {
                        self.preview.close();
                        native::message(tr("ファイル"), reason);
                    }
                    _ => {
                        self.preview.close();
                        native::message(
                            tr("ファイル"),
                            tr("ファイルの場所を確認できませんでした。"),
                        );
                    }
                }
            })
        });
    }

    /// The Host path `source` names, with its position, for an editor or a
    /// menu's action; a target that does not exist yet still resolves.
    pub fn resolve(
        self,
        source: String,
        cwd: String,
        home: Option<String>,
        host: String,
        client: Arc<Client>,
        then: impl FnOnce(Reference) + 'static,
    ) {
        let (line, column) = position(&source, &cwd);
        let Some(mut request) = mobile_file_open_request(source, cwd, home) else {
            return;
        };
        request.inspect_directory = false;
        let resolve = client.request(&host, None, Intent::ResolveFileReference(request));
        self.owner.run(move || {
            spawn(async move {
                match resolve.recv().await {
                    Ok(Ok(Outcome::FileReferenceResolved { path, .. })) => {
                        then(Reference { path, line, column })
                    }
                    Ok(Err(reason)) => native::message(tr("ファイル"), reason.to_string()),
                    Err(reason) => native::message(tr("ファイル"), reason.to_string()),
                    Ok(Ok(_)) => native::message(
                        tr("ファイル"),
                        tr("ファイルの場所を確認できませんでした。"),
                    ),
                }
            })
        });
    }

    pub fn open_file(self, path: String, line: Option<u32>, host: String, client: Arc<Client>) {
        if file_preview_mime_type(path.clone()).is_some() {
            self.preview
                .load(crate::media::receive(client.download(host, path)));
        } else {
            self.preview.close();
            self.workspace
                .open(PanelKind::File, Some(path.clone()), line);
            client.dispatch(
                &host,
                None,
                Intent::ReadFile(agent_core::state::operations::ReadFile {
                    path,
                    discard_draft: false,
                }),
            );
        }
    }
}

/// A file a link names, resolved on the Host, and the position it names.
#[derive(Clone)]
pub(crate) struct Reference {
    pub path: String,
    pub line: Option<u32>,
    pub column: Option<u32>,
}

/// The line and column a link names.
fn position(source: &str, cwd: &str) -> (Option<u32>, Option<u32>) {
    agent_core::presentation::markdown::markdown_file_target(
        source.into(),
        cwd.into(),
        agent_core::presentation::markdown::MarkdownFileSurface::Mobile,
    )
    .map_or((None, None), |target| (target.line, target.column))
}

/// The Host path of the workspace the conversation works in.
#[cfg(desktop)]
fn workspace(connection: Connection) -> String {
    connection.publication.with(|p| {
        agent_core::presentation::markdown::file_host_path(
            p.composer.controls.navigation.cwd.clone(),
            p.context.home(),
        )
    })
}

/// The Files tool's tree: on the desktop, of the workspace unless the tool
/// shows a Host folder outside it, as the desktop client's explorer.
pub(crate) fn screen(id: u64) -> impl Element {
    let browser = use_context::<Browser>().unwrap();
    #[cfg(desktop)]
    let root = {
        let connection = use_context::<Connection>().unwrap();
        computed(move || {
            let selected = browser.root.get();
            let workspace = workspace(connection);
            if workspace.is_empty() {
                return selected;
            }
            Some(agent_core::presentation::files::desktop_file_tree_root(
                &workspace,
                selected.as_deref(),
                true,
            ))
        })
    };
    #[cfg(not(desktop))]
    let root = browser.root;
    tree(format!("nsh-files-{id}"), root)
}

/// The file list beside an open file: the workspace's tree.
#[cfg(desktop)]
pub(crate) fn explorer(id: u64) -> impl Element {
    let connection = use_context::<Connection>().unwrap();
    let root = computed(move || Some(workspace(connection)).filter(|root| !root.is_empty()));
    tree(format!("nsh-explorer-{id}"), root)
}

fn tree(owner: String, root: Memo<Option<String>>) -> impl Element {
    let connection = use_context::<Connection>().unwrap();
    let browser = use_context::<Browser>().unwrap();
    let key = computed(move || FileTreeKey {
        owner: owner.clone(),
        root: root.get().unwrap_or_default(),
    });
    // Opening a file from the tree replaces this screen while its effect runs, so
    // the open tree is kept outside the reactive graph for the cleanup to close it.
    let mounted = Rc::new(RefCell::new(None::<(String, FileTreeKey)>));
    let closing = mounted.clone();
    effect(move || {
        let host = connection.host.get();
        let key = key.get();
        let next = (!key.root.is_empty()).then(|| (host.clone(), key.clone()));
        if let Some((host, key)) = mounted.replace(next) {
            connection.client.with(|client| {
                client.dispatch(
                    &host,
                    None,
                    Intent::FileTree {
                        key,
                        action: FileTreeAction::Close,
                    },
                )
            });
        }
        if key.root.is_empty() {
            return;
        }
        let workspace_root = connection
            .composer
            .with_untracked(|p| p.controls.navigation.cwd.clone());
        connection.client.with(|client| {
            client.dispatch(
                &host,
                None,
                Intent::FileTree {
                    key,
                    action: FileTreeAction::Open {
                        surface: if is_desktop() {
                            FileTreeSurface::Desktop
                        } else {
                            FileTreeSurface::Mobile
                        },
                        locale: locale().as_str().into(),
                        workspace_root,
                    },
                },
            )
        });
    });
    cleanup(move || {
        if let Some((host, key)) = closing.take() {
            connection.client.with(|client| {
                client.dispatch(
                    &host,
                    None,
                    Intent::FileTree {
                        key,
                        action: FileTreeAction::Close,
                    },
                )
            });
        }
    });
    let tree = computed(move || {
        connection
            .publication
            .with(|p| p.snapshot.files.file_tree(key.get()))
    });
    let send = move |action| {
        connection.dispatch(Intent::FileTree {
            key: key.get_untracked(),
            action,
        })
    };
    let opened = computed(move || tree.with(|tree| tree.open_file.clone()));
    let previous = stored(opened.get_untracked());
    effect(move || {
        let next = opened.get();
        if previous.replace(next.clone()) != next
            && let Some(request) = next
        {
            browser.open_file(
                file_tree_path(&key.get_untracked().root, &request.path),
                None,
                connection.host.get_untracked(),
                connection.client.get(),
            );
        }
    });
    // The focused tree takes the keys an outline view would; Core decides
    // what each key does to the focused row.
    let engaged = signal(false);
    let keyed = stored(false);
    let list = element_ref();
    let area = element_ref();
    #[cfg_attr(not(desktop), allow(unused_mut))]
    let mut keys = vec![
        ("up", "up", false),
        ("down", "down", false),
        ("left", "left", false),
        ("right", "right", false),
        ("home", "home", false),
        ("end", "end", false),
        ("enter", "enter", false),
        ("space", "space", false),
        ("shift+up", "up", true),
        ("shift+down", "down", true),
    ];
    // The focused row's menu, as the desktop client's.
    #[cfg(desktop)]
    keys.push(("shift+f10", "contextmenu", false));
    for (keystroke, key, shift) in keys {
        let options = KeyOptions {
            when: nsh_core::keymap::When::Always,
            repeat: true,
        };
        on_key_with(keystroke, options, move |_| {
            if engaged.get_untracked()
                && tree.with_untracked(|tree| tree.handles_desktop_key(key, false, shift))
            {
                keyed.replace(true);
                send(FileTreeAction::Keyboard {
                    key: key.into(),
                    additive: false,
                    shift,
                });
            }
        });
    }
    // A row the keys move past the viewport's edge scrolls in at that edge.
    let viewable = stored(None::<(usize, usize)>);
    let focused = computed(move || tree.with(|tree| tree.focused_path.clone()));
    effect(move || {
        let path = focused.get();
        if !keyed.replace(false) {
            return;
        }
        let Some(index) =
            path.and_then(|path| tree.with_untracked(|tree| tree.index_for_path(&path)))
        else {
            return;
        };
        let edge = match viewable.get() {
            Some((first, _)) if index < first => 0.0,
            Some((_, last)) if index > last => 1.0,
            _ => return,
        };
        list.scroll_to_index(
            index as u32,
            nsh_core::geometry::Percentage(edge),
            px(0.0),
            false,
        );
    });
    #[cfg(desktop)]
    let pointer = desktop::Pointer {
        connection,
        key,
        tree,
        dragged: signal(None),
    };
    #[cfg(desktop)]
    pointer.keyboard_menu();
    let all_expanded = move || tree.with(|tree| tree.all_expanded);
    let toggle_all_label = move || {
        agent_core::presentation::files::file_tree_toggle_all_label(
            crate::l10n::language(),
            tree.with(|tree| tree.collapse_all),
        )
    };
    view()
        .flex_1()
        .min_h_0()
        .child(
            view()
                .flex_row()
                .items_center()
                .gap_1()
                .h(px(40.0))
                .px_3()
                .border_b_1()
                .border_b_color(colors::border())
                .child(
                    text(move || key.with(|key| key.root.clone()))
                        .text_size(px(12.0))
                        .text_color(colors::muted())
                        .truncate()
                        .selectable(true)
                        .flex_1()
                        .min_w_0(),
                )
                .child(ui::icon_button(
                    Icon::REFRESH,
                    tr("ファイル一覧を更新"),
                    move || send(FileTreeAction::Refresh),
                ))
                .child(when(
                    move || tree.with(|tree| tree.can_expand_all),
                    move || {
                        view()
                            .child(when(all_expanded, move || {
                                ui::icon_button(Icon::COLLAPSE_ALL, toggle_all_label, move || {
                                    send(FileTreeAction::ToggleAll)
                                })
                            }))
                            .child(when(
                                move || !all_expanded(),
                                move || {
                                    ui::icon_button(Icon::EXPAND_ALL, toggle_all_label, move || {
                                        send(FileTreeAction::ToggleAll)
                                    })
                                },
                            ))
                    },
                )),
        )
        .child(
            ui::search_field(
                move || tree.with(|tree| tree.query.clone()),
                move |text| send(FileTreeAction::Query { text: text.into() }),
                Some(tr("ファイルを検索")),
            )
            .mx_2()
            .my_2(),
        )
        .child(when(
            move || tree.with(|tree| tree.error.is_some()),
            move || {
                notice(move || {
                    tree.with(|tree| {
                        tree.error
                            .as_ref()
                            .map(|error| localized(error.to_string()))
                            .unwrap_or_default()
                    })
                })
            },
        ))
        .child(when(
            move || tree.with(|tree| tree.truncated),
            || {
                notice(tr(
                    "ほかにも一致する項目があります。検索語を絞り込んでください。",
                ))
            },
        ))
        .child(when(
            move || tree.with(|tree| tree.rows.is_empty()),
            move || {
                notice(move || {
                    tree.with(|tree| {
                        if tree.pending {
                            tr("ファイルを読み込み中…")
                        } else if tree.error.is_some() {
                            tr("更新して再試行してください。")
                        } else if tree.query.trim().is_empty() {
                            tr("ファイルはありません")
                        } else {
                            tr("一致するファイルはありません")
                        }
                    })
                })
            },
        ))
        .child(
            // The tree's area, not its scrolling list, holds the keyboard focus,
            // so the keymap gets the arrows.
            view()
                .flex_1()
                .min_h_0()
                .element_ref(&area)
                .focusable(true)
                .on_focus(move || engaged.set(true))
                .on_blur(move || engaged.set(false))
                .child(
                    virtual_list(
                        move || tree.with(|tree| tree.rows.len()),
                        move |at| {
                            let row = computed(move || {
                                tree.with(|tree| {
                                    let row = tree.rows.get(at.index())?;
                                    Some((row.clone(), tree.is_search_match(row)))
                                })
                            });
                            let element = tree_row(
                                row,
                                move || {
                                    engaged.get()
                                        && tree.with(|tree| {
                                            tree.rows.get(at.index()).map(|row| &row.path)
                                                == tree.focused_path.as_ref()
                                        })
                                },
                                // ⌘ (Ctrl) adds a row to the selection and Shift
                                // extends it, as the desktop client's tree.
                                move |path, modifiers: nsh_core::keymap::Modifiers| {
                                    area.focus();
                                    send(FileTreeAction::Click {
                                        path,
                                        additive: modifiers.cmd || modifiers.ctrl,
                                        range: modifiers.shift,
                                    })
                                },
                            );
                            #[cfg(desktop)]
                            let element = pointer.row(element, move || {
                                row.with_untracked(|row| {
                                    row.as_ref().map(|(row, _)| row.path.clone())
                                })
                            });
                            element
                        },
                    )
                    .keys(move || {
                        tree.with(|tree| {
                            tree.rows
                                .iter()
                                .map(|row| row.path.clone())
                                .collect::<Vec<_>>()
                        })
                    })
                    .element_ref(&list)
                    .on_viewable_items_change(move |items: &ViewableItems| {
                        viewable.replace(items.viewable);
                    })
                    .estimated_item_size(px(24.0))
                    .content_padding_x(px(8.0))
                    .flex_1()
                    .min_h_0(),
                ),
        )
        .map(|tree| {
            #[cfg(desktop)]
            let tree = tree.child(desktop::drag_label(pointer.dragged));
            tree
        })
}

/// A muted line under a panel's header: an error, a limit, or why it is empty.
pub(crate) fn notice(message: impl IntoReactive<SharedString>) -> impl IntoElement {
    text(message)
        .px_3()
        .py_1()
        .text_size(px(12.0))
        .text_color(colors::muted())
}

/// A row of the tree, as the desktop client's: a folder's chevron (a spinner while
/// it loads) or a file's glyph, and its name; a press selects it, which opens a
/// file and folds or unfolds a folder.
fn tree_row(
    row: Memo<Option<(FileTreeRow, bool)>>,
    focused: impl Fn() -> bool + 'static,
    press: impl Fn(String, nsh_core::keymap::Modifiers) + Copy + 'static,
) -> View {
    let field = move |read: fn(&FileTreeRow) -> bool| {
        row.with(|row| row.as_ref().is_some_and(|(row, _)| read(row)))
    };
    let directory = move || field(|row| row.kind == EntryKind::Directory);
    let expanded = move || field(|row| row.expanded);
    let loading = move || directory() && expanded() && field(|row| row.loading);
    let ignored = move || field(|row| row.ignored);
    let selected = move || field(|row| row.selected);
    let label = move || {
        row.with(|row| {
            row.as_ref()
                .map(|(row, _)| row.label.clone())
                .unwrap_or_default()
        })
    };
    // A view rather than a button: its press reports the keys held.
    view()
        .role(nsh_core::AccessibilityRole::Button)
        .label(move || SharedString::from(label()))
        .accessibility_state(move || nsh_core::AccessibilityState {
            selected: selected(),
            ..Default::default()
        })
        .flex_row()
        .items_center()
        .gap_1()
        .h(px(24.0))
        .pl(move || {
            px(6.0 + row.with(|row| row.as_ref().map_or(0, |(row, _)| row.depth)) as f32 * 18.0)
        })
        .pr_2()
        .rounded(px(ui::RADIUS))
        .hover(|s| s.bg(ROW_HOVER))
        .style_when(selected, |s| s.bg(ROW_SELECTED))
        // The keyboard's row, ringed as the desktop client's.
        .border_1()
        .border_color(move || {
            if focused() {
                rgb(0x009fff)
            } else {
                rgba(0x00000000)
            }
        })
        .child(
            view()
                .w(px(16.0))
                .h(px(16.0))
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .text_color(move || {
                    if selected() {
                        colors::foreground()
                    } else {
                        GLYPH
                    }
                })
                .opacity(move || if ignored() { 0.5_f32 } else { 1.0 })
                .child(when(loading, ui::spinner))
                .child(when(
                    move || directory() && !loading() && expanded(),
                    || ui::icon(Icon::CHEVRON_DOWN, 16.0),
                ))
                .child(when(
                    move || directory() && !expanded(),
                    || ui::icon(Icon::CHEVRON_RIGHT, 16.0),
                ))
                .child(when(
                    move || !directory(),
                    move || {
                        image(move || {
                            file_icon(&row.with(|row| {
                                row.as_ref()
                                    .map(|(row, _)| row.path.clone())
                                    .unwrap_or_default()
                            }))
                        })
                        .w(px(16.0))
                        .h(px(16.0))
                    },
                )),
        )
        .child(
            text(label)
                .text_size(px(12.0))
                .number_of_lines(1)
                .flex_1()
                .min_w_0()
                .text_color(move || {
                    if ignored() {
                        IGNORED
                    } else {
                        colors::foreground()
                    }
                })
                .style_when(
                    move || row.with(|row| row.as_ref().is_some_and(|(_, found)| *found)),
                    |s| s.font_weight(FontWeight::SEMIBOLD),
                ),
        )
        .on_press(move |event: &PressEvent| {
            if let Some(path) =
                row.with_untracked(|row| row.as_ref().map(|(row, _)| row.path.clone()))
            {
                press(path, event.modifiers);
            }
        })
}

// The desktop client's tree inks: a row under the pointer and a selected row
// (the foreground at 7% and 12%), glyphs, and ignored entries.
const ROW_HOVER: Rgba = rgba(0xe6e6e612);
const ROW_SELECTED: Rgba = rgba(0xe6e6e61f);
const GLYPH: Rgba = rgb(0x84848a);
const IGNORED: Rgba = rgb(0x4a4a4e);

include!(concat!(env!("OUT_DIR"), "/file_icons.rs"));

/// A file's glyph from T3 Code's tree icons, its tree variant when it has one.
fn file_icon(path: &str) -> String {
    let name = desktop_file_icon(path);
    let data = file_icon_data(&format!("{name}-tree")).unwrap_or_else(|| glyph(name));
    format!("data:image/png;base64,{data}")
}

/// The base64 PNG of the glyph Core names, or the default file's.
pub(crate) fn glyph(name: &str) -> &'static str {
    file_icon_data(name)
        .or_else(|| file_icon_data("file-tree-builtin-default"))
        .expect("the file icon catalog contains the default glyph")
}

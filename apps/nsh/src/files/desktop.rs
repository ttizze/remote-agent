//! The desktop tree's pointer, as the desktop client's: a secondary press or
//! Shift+F10 opens a row's menu, and rows drag into the composer as mentions,
//! their label under the pointer.
use crate::{Connection, l10n::tr_format, ui};
use agent_core::{
    presentation::files::{FileTreeAction, FileTreeKey, FileTreeView},
    state::Intent,
};
use nsh_core::prelude::*;

/// How far a press moves before it drags its row.
const DRAG_SLOP: f32 = 4.0;

/// Rows being dragged: their paths, the label that follows the pointer, and
/// where the pointer is.
#[derive(Clone, PartialEq)]
pub(super) struct Dragged {
    paths: Vec<String>,
    label: String,
    pointer: Point<Points>,
}

/// What a tree's rows act on.
#[derive(Clone, Copy)]
pub(super) struct Pointer {
    pub connection: Connection,
    pub key: Memo<FileTreeKey>,
    pub tree: Memo<FileTreeView>,
    pub dragged: Signal<Option<Dragged>>,
}

impl Pointer {
    fn send(self, action: FileTreeAction) {
        self.connection.dispatch(Intent::FileTree {
            key: self.key.get_untracked(),
            action,
        })
    }

    /// `row`, whose path `path` reads, with its menu on a secondary press and
    /// the drag of its selection.
    pub(super) fn row(self, row: View, path: impl Fn() -> Option<String> + Copy + 'static) -> View {
        row.on_context_menu(move |_: &ContextMenuEvent| {
            if let Some(path) = path() {
                self.send(FileTreeAction::Focus { path: path.clone() });
                crate::file_actions::tree_menu(self.connection, &self.key.get_untracked(), &path);
            }
        })
        .claim_touches(TouchClaim::on_move().ask(move |request: &ClaimRequest| {
            let moved = request.event.gesture.delta;
            if moved.x.0.hypot(moved.y.0) >= DRAG_SLOP
                && path().is_some_and(|path| {
                    self.tree
                        .with_untracked(|tree| !tree.drag_paths(&path).is_empty())
                })
            {
                request.claim();
            }
        }))
        .keep_touches(true)
        .on_responder_grant(move |event: &ResponderEvent| {
            if let Some(path) = path() {
                self.start(path, event.page);
            }
        })
        .on_responder_move(move |event: &ResponderEvent| {
            self.dragged.update(|dragged| {
                if let Some(dragged) = dragged {
                    dragged.pointer = event.page;
                }
            })
        })
        .on_responder_release(move |event: &ResponderEvent| self.drop(Some(event.page)))
        .on_responder_terminate(move |_: &ResponderEvent| self.drop(None))
    }

    fn start(self, path: String, pointer: Point<Points>) {
        let (paths, label) = self.tree.with_untracked(|tree| {
            let paths = tree.drag_paths(&path);
            let label = match &paths[..] {
                [one] => tree
                    .rows
                    .iter()
                    .find(|row| &row.path == one)
                    .map_or_else(|| one.clone(), |row| row.label.clone()),
                _ => tr_format("{0} 個の項目", [&paths.len().to_string()]),
            };
            (paths, label)
        });
        self.send(FileTreeAction::DragStart { path });
        self.dragged.set(Some(Dragged {
            paths,
            label,
            pointer,
        }));
    }

    /// Ends the drag; dropped at `pointer` on a composer that takes text, its
    /// rows join the draft as mentions.
    fn drop(self, pointer: Option<Point<Points>>) {
        let Some(dragged) = self.dragged.get_untracked() else {
            return;
        };
        self.dragged.set(None);
        let connection = self.connection;
        let mentions = crate::file_actions::tree_mentions(
            connection,
            &self.key.get_untracked(),
            &dragged.paths,
        );
        self.send(FileTreeAction::DragEnd {
            paths: dragged.paths,
        });
        let open = connection
            .composer
            .with_untracked(|c| c.controls.connected && c.controls.question.is_none());
        let Some(pointer) = pointer.filter(|_| open) else {
            return;
        };
        spawn(async move {
            let Some(composer) = connection.composer_box.measure().await else {
                return;
            };
            let (start, size) = (composer.page, composer.frame.size);
            if (start.x.0..start.x.0 + size.width.0).contains(&pointer.x.0)
                && (start.y.0..start.y.0 + size.height.0).contains(&pointer.y.0)
            {
                crate::file_actions::add_mentions(connection, &mentions);
            }
        });
    }

    /// Opens the menu Shift+F10 asks Core for, on the focused row.
    pub(super) fn keyboard_menu(self) {
        let requested = computed(move || self.tree.with(|tree| tree.context_menu.clone()));
        let previous = stored(requested.get_untracked());
        effect(move || {
            let next = requested.get();
            if previous.replace(next.clone()) != next
                && let Some(request) = next
            {
                crate::file_actions::tree_menu(
                    self.connection,
                    &self.key.get_untracked(),
                    &request.path,
                );
            }
        });
    }
}

/// The label of the rows being dragged, beside the pointer, over everything.
pub(super) fn drag_label(dragged: Signal<Option<Dragged>>) -> impl IntoElement {
    use ui::colors;
    let at = move || {
        dragged.with(|d| {
            d.as_ref()
                .map_or((0.0, 0.0), |d| (d.pointer.x.0, d.pointer.y.0))
        })
    };
    when(
        move || dragged.with(Option::is_some),
        move || {
            overlay().absolute().inset_0().child(
                view()
                    .absolute()
                    .left(move || px(at().0 + 12.0))
                    .top(move || px(at().1 + 12.0))
                    .px_2()
                    .py_1()
                    .rounded(px(ui::RADIUS))
                    .bg(colors::RAISED)
                    .border_1()
                    .border_color(colors::border())
                    .child(
                        text(move || {
                            dragged
                                .with(|d| d.as_ref().map(|d| d.label.clone()).unwrap_or_default())
                        })
                        .text_size(px(12.0))
                        .text_color(colors::foreground())
                        .number_of_lines(1),
                    ),
            )
        },
    )
}

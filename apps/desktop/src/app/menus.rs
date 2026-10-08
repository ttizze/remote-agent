//! The thread action menu the sidebar rows and the header share, and the
//! dialogs its items open (custom snooze, rename).
use super::{
    Desktop,
    settings::SettingsPage,
    ui::{self, color, icon},
};
use agent_core::{
    state::{Intent, ThreadAction},
    view::{
        api::snooze_presets,
        header::{EMPTY_TITLE_WARNING, RENAME_FAILED, RenameCommit, resolve_rename_commit},
        sidebar::{
            ForwardNavigation, ParkAction, SidebarMenuAction, SidebarMenuItem,
            bulk_delete_confirmation, should_navigate_after_thread_park,
        },
        snooze::{
            CustomSnoozeInput, SnoozeDurationUnit, local_snooze_date, local_snooze_time,
            resolve_custom_snooze,
        },
        thread_menu::{
            DraftMenuItem, DraftMenuItemId, ThreadMenuAction, ThreadMenuChild,
            ThreadMenuConfirmation, ThreadMenuItem, ThreadMenuOptions, ThreadMenuSurface,
            build_draft_menu,
        },
        thread_summary::ThreadSummary,
    },
};
use chrono::{Local, NaiveDate, TimeZone};
use gpui_kit::{
    component::{
        Selectable, Sizable, WindowExt,
        button::{Button, ButtonVariants},
        calendar::Date,
        date_picker::{DatePicker, DatePickerEvent, DatePickerState},
        h_flex,
        input::{Escape, Input, InputEvent, InputState, NumberInput, NumberInputEvent, StepAction},
        menu::{DropdownMenu, PopupMenu, PopupMenuItem},
        notification::Notification,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};

/// Where a thread menu opened: the sidebar scopes by project and moves on
/// after parking the open thread; the header does neither.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuSurface {
    Sidebar,
    Header,
}

struct OpenMenu {
    menu: Entity<PopupMenu>,
    position: Point<Pixels>,
    _dismiss: Subscription,
}

/// An inline title edit, in a sidebar row or the header.
pub(crate) struct Rename {
    pub(crate) thread_id: String,
    pub(crate) surface: MenuSurface,
    original: String,
    pub(crate) input: Entity<InputState>,
    _events: Subscription,
}

pub(crate) struct MenuState {
    open: Option<OpenMenu>,
    pub(crate) rename: Option<Rename>,
}
impl MenuState {
    pub(crate) fn new(_: &mut Window, _: &mut Context<Desktop>) -> Self {
        Self {
            open: None,
            rename: None,
        }
    }
}

/// What a menu item runs once chosen.
#[derive(Clone)]
enum Command {
    Thread {
        thread_id: String,
        surface: MenuSurface,
        action: ThreadMenuAction,
        confirmation: Option<ThreadMenuConfirmation>,
    },
    /// `until: None` asks for a custom time.
    Snooze {
        thread_ids: Vec<String>,
        surface: MenuSurface,
        until: Option<String>,
    },
    Bulk {
        thread_ids: Vec<String>,
        action: SidebarMenuAction,
    },
    Draft {
        draft_key: String,
        project_id: String,
        id: DraftMenuItemId,
    },
    NewThread {
        project_id: String,
    },
}

/// The label of the button that confirms `action`.
fn confirm_label(action: &ThreadMenuAction) -> &'static str {
    match action {
        ThreadMenuAction::Thread {
            action: ThreadAction::Unpin,
        } => "Unpin",
        ThreadMenuAction::Thread {
            action: ThreadAction::Archive,
        } => "Archive",
        ThreadMenuAction::Thread {
            action: ThreadAction::Delete,
        } => "Delete",
        _ => "Confirm",
    }
}

fn entry(
    label: String,
    icon_name: Option<&str>,
    destructive: bool,
    detail: Option<String>,
) -> PopupMenuItem {
    let item = if destructive || detail.is_some() {
        PopupMenuItem::element(move |_, _| {
            h_flex()
                .w_full()
                .gap_4()
                .child(
                    div()
                        .flex_1()
                        .when(destructive, |label| {
                            label.text_color(color("errorForeground"))
                        })
                        .child(label.clone()),
                )
                .when_some(detail.clone(), |row, detail| {
                    row.child(div().text_xs().text_color(color("textMuted")).child(detail))
                })
        })
    } else {
        PopupMenuItem::new(label)
    };
    item.when_some(icon_name, |item, name| {
        let glyph = icon(name);
        item.icon(if destructive {
            glyph.text_color(color("errorForeground"))
        } else {
            glyph
        })
    })
}

fn on_choose(
    desktop: &WeakEntity<Desktop>,
    command: Command,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let desktop = desktop.clone();
    move |_, window, cx| {
        let command = command.clone();
        let _ = desktop.update(cx, |view, cx| view.run_menu_command(command, window, cx));
    }
}

fn thread_children(
    mut menu: PopupMenu,
    children: &[ThreadMenuChild],
    thread_id: &str,
    surface: MenuSurface,
    desktop: &WeakEntity<Desktop>,
) -> PopupMenu {
    for child in children {
        if child.separator_before {
            menu = menu.separator();
        }
        let command = Command::Thread {
            thread_id: thread_id.into(),
            surface,
            action: child.action.clone(),
            confirmation: None,
        };
        menu = menu.item(
            entry(
                child.label.clone(),
                child.icon.as_deref(),
                false,
                child.detail.clone(),
            )
            .checked(child.checked == Some(true))
            .on_click(on_choose(desktop, command)),
        );
    }
    menu
}

fn thread_items(
    mut menu: PopupMenu,
    items: &[ThreadMenuItem],
    thread_id: &str,
    surface: MenuSurface,
    desktop: &WeakEntity<Desktop>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    for item in items {
        if item.separator_before {
            menu = menu.separator();
        }
        if !item.children.is_empty() {
            let children = item.children.clone();
            let id = thread_id.to_owned();
            let owner = desktop.clone();
            let submenu = PopupMenu::build(window, cx, move |sub, _, _| {
                thread_children(sub, &children, &id, surface, &owner)
            });
            menu = menu.item(
                PopupMenuItem::submenu(item.label.clone(), submenu)
                    .when_some(item.icon.as_deref(), |entry, name| entry.icon(icon(name)))
                    .disabled(!item.enabled),
            );
            continue;
        }
        let entry = entry(
            item.label.clone(),
            item.icon.as_deref(),
            item.destructive,
            None,
        )
        .disabled(!item.enabled || item.action.is_none());
        menu = menu.item(match &item.action {
            Some(action) => entry.on_click(on_choose(
                desktop,
                Command::Thread {
                    thread_id: thread_id.into(),
                    surface,
                    action: action.clone(),
                    confirmation: item.confirmation.clone(),
                },
            )),
            None => entry,
        });
    }
    menu
}

fn draft_items(
    mut menu: PopupMenu,
    items: &[DraftMenuItem],
    draft_key: &str,
    project_id: &str,
    desktop: &WeakEntity<Desktop>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let command = |id| Command::Draft {
        draft_key: draft_key.into(),
        project_id: project_id.into(),
        id,
    };
    for item in items {
        if item.separator_before {
            menu = menu.separator();
        }
        if !item.children.is_empty() || item.id == DraftMenuItemId::Copy {
            let children: Vec<_> = item
                .children
                .iter()
                .map(|child| (child.label.clone(), child.icon.clone(), command(child.id)))
                .collect();
            let owner = desktop.clone();
            let submenu = PopupMenu::build(window, cx, move |mut sub, _, _| {
                for (label, icon_name, command) in &children {
                    sub = sub.item(
                        entry(label.clone(), icon_name.as_deref(), false, None)
                            .on_click(on_choose(&owner, command.clone())),
                    );
                }
                sub
            });
            menu = menu.item(
                PopupMenuItem::submenu(item.label.clone(), submenu)
                    .when_some(item.icon.as_deref(), |entry, name| entry.icon(icon(name)))
                    .disabled(!item.enabled),
            );
            continue;
        }
        menu = menu.item(
            entry(
                item.label.clone(),
                item.icon.as_deref(),
                item.destructive,
                None,
            )
            .disabled(!item.enabled)
            .on_click(on_choose(desktop, command(item.id))),
        );
    }
    menu
}

/// The snooze presets with their wake time, then "Custom…".
fn snooze_items(
    mut menu: PopupMenu,
    thread_ids: &[String],
    surface: MenuSurface,
    format: agent_core::view::time::TimestampFormat,
    desktop: &WeakEntity<Desktop>,
) -> PopupMenu {
    let snooze = |until| Command::Snooze {
        thread_ids: thread_ids.to_vec(),
        surface,
        until,
    };
    for preset in snooze_presets(ui::now_ms(), format) {
        menu = menu.item(
            entry(preset.label, None, false, Some(preset.when_label))
                .on_click(on_choose(desktop, snooze(Some(preset.snoozed_until)))),
        );
    }
    menu.separator()
        .item(PopupMenuItem::new("Custom…").on_click(on_choose(desktop, snooze(None))))
}

impl Desktop {
    /// Shows `build`'s menu at `position` until an item runs or it is dismissed.
    pub(crate) fn open_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
        build: impl FnOnce(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu,
    ) {
        let menu = PopupMenu::build(window, cx, build);
        let dismiss = cx.subscribe_in(&menu, window, |view, menu, _: &DismissEvent, _, cx| {
            if view
                .menus
                .open
                .as_ref()
                .is_some_and(|open| open.menu == *menu)
            {
                view.menus.open = None;
                view.command_palette_open = false;
                cx.notify();
            }
        });
        menu.focus_handle(cx).focus(window, cx);
        self.menus.open = Some(OpenMenu {
            menu,
            position,
            _dismiss: dismiss,
        });
        cx.notify();
    }

    pub(crate) fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menus.open.take().is_some() {
            self.command_palette_open = false;
            cx.notify();
        }
    }

    /// The open menu, drawn over everything.
    pub(crate) fn render_menu_layer(&self) -> Option<AnyElement> {
        let open = self.menus.open.as_ref()?;
        Some(
            deferred(
                anchored()
                    .position(open.position)
                    .snap_to_window_with_margin(px(8.))
                    .child(open.menu.clone()),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    /// The thread action menu at `position`.
    pub(crate) fn open_thread_menu(
        &mut self,
        thread_id: String,
        surface: MenuSurface,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.snapshot.thread_menu(
            thread_id,
            ui::now_ms(),
            ThreadMenuOptions {
                surface: match surface {
                    MenuSurface::Sidebar => ThreadMenuSurface::List,
                    MenuSurface::Header => ThreadMenuSurface::Header,
                },
                ..ThreadMenuOptions::default()
            },
        ) else {
            return;
        };
        let desktop = cx.entity().downgrade();
        self.open_menu(position, window, cx, move |menu, window, cx| {
            thread_items(
                menu.min_w(px(200.)),
                &view.items,
                &view.thread_id,
                surface,
                &desktop,
                window,
                cx,
            )
        });
    }

    /// The menu of an unsent new-thread draft row.
    pub(crate) fn open_draft_menu(
        &mut self,
        draft_key: String,
        project_id: String,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let has_path = self.project_root(&project_id).is_some();
        let has_project = self
            .snapshot
            .shell_projects()
            .iter()
            .any(|project| project.id == project_id);
        let items = build_draft_menu(has_path, false, has_project);
        let desktop = cx.entity().downgrade();
        self.open_menu(position, window, cx, move |menu, window, cx| {
            draft_items(
                menu.min_w(px(200.)),
                &items,
                &draft_key,
                &project_id,
                &desktop,
                window,
                cx,
            )
        });
    }

    /// The bulk menu over the rendered rows of the multi-selection.
    pub(crate) fn open_selection_menu(
        &mut self,
        items: Vec<SidebarMenuItem>,
        thread_ids: Vec<String>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let desktop = cx.entity().downgrade();
        let format = self.snapshot.preferences.timestamp_format;
        self.open_menu(position, window, cx, move |mut menu, window, cx| {
            menu = menu.min_w(px(200.));
            for item in &items {
                if item.action == SidebarMenuAction::Snooze {
                    let ids = thread_ids.clone();
                    let owner = desktop.clone();
                    let submenu = PopupMenu::build(window, cx, move |sub, _, _| {
                        snooze_items(sub, &ids, MenuSurface::Sidebar, format, &owner)
                    });
                    menu = menu.item(PopupMenuItem::submenu(item.label.clone(), submenu));
                    continue;
                }
                menu = menu.item(
                    entry(item.label.clone(), None, item.destructive, None)
                        .disabled(item.disabled)
                        .on_click(on_choose(
                            &desktop,
                            Command::Bulk {
                                thread_ids: thread_ids.clone(),
                                action: item.action,
                            },
                        )),
                );
            }
            menu
        });
    }

    /// The hover snooze button's presets.
    pub(crate) fn open_snooze_menu(
        &mut self,
        thread_id: String,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let desktop = cx.entity().downgrade();
        let format = self.snapshot.preferences.timestamp_format;
        self.open_menu(position, window, cx, move |menu, _, _| {
            snooze_items(menu, &[thread_id], MenuSurface::Sidebar, format, &desktop)
        });
    }

    /// "New thread in…": one item per project.
    pub(crate) fn open_new_thread_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let projects: Vec<_> = self
            .views
            .sidebar
            .project_scope
            .iter()
            .filter_map(|item| Some((item.project_id.clone()?, item.label.clone())))
            .collect();
        let desktop = cx.entity().downgrade();
        self.open_menu(position, window, cx, move |mut menu, _, _| {
            menu = menu.min_w(px(200.)).label("New thread in…");
            for (project_id, label) in &projects {
                menu = menu.item(PopupMenuItem::new(label.clone()).on_click(on_choose(
                    &desktop,
                    Command::NewThread {
                        project_id: project_id.clone(),
                    },
                )));
            }
            menu
        });
    }

    fn run_menu_command(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        match command {
            Command::Thread {
                thread_id,
                surface,
                action,
                confirmation,
            } => self.run_thread_action(thread_id, surface, action, confirmation, window, cx),
            Command::Snooze {
                thread_ids,
                surface,
                until: Some(until),
            } => self.park_threads(
                thread_ids,
                ThreadAction::Snooze { until },
                surface,
                window,
                cx,
            ),
            Command::Snooze {
                thread_ids,
                surface,
                until: None,
            } => self.open_custom_snooze(thread_ids, surface, window, cx),
            Command::Bulk { thread_ids, action } => {
                self.run_bulk_action(thread_ids, action, window, cx)
            }
            Command::Draft {
                draft_key,
                project_id,
                id,
            } => match id {
                DraftMenuItemId::Copy | DraftMenuItemId::CopyBranch => {}
                DraftMenuItemId::CopyPath => {
                    if let Some(path) = self.project_root(&project_id) {
                        self.copy_with_toast(path, "Path copied", window, cx);
                    }
                }
                DraftMenuItemId::ProjectSettings => self.open_settings(
                    SettingsPage::Projects {
                        project_id: Some(project_id),
                    },
                    window,
                    cx,
                ),
                DraftMenuItemId::Discard => self.perform(Intent::DiscardDraft { draft_key }),
            },
            Command::NewThread { project_id } => self.new_thread(Some(project_id), cx),
        }
    }

    /// Runs one thread menu action from `surface`.
    pub(crate) fn run_thread_action(
        &mut self,
        thread_id: String,
        surface: MenuSurface,
        action: ThreadMenuAction,
        confirmation: Option<ThreadMenuConfirmation>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            ThreadMenuAction::Thread {
                action: action @ (ThreadAction::Settle | ThreadAction::Snooze { .. }),
            } => self.park_threads(vec![thread_id], action, surface, window, cx),
            ThreadMenuAction::Thread { action } => {
                let label = confirm_label(&ThreadMenuAction::Thread {
                    action: action.clone(),
                });
                self.perform_confirmed(
                    Intent::Thread { thread_id, action },
                    confirmation.as_ref(),
                    label,
                    window,
                    cx,
                );
            }
            ThreadMenuAction::FilterProject { project_id } => self.filter_project(project_id, cx),
            ThreadMenuAction::NewThreadOnBranch {
                project_id,
                branch,
                worktree_path,
            } => {
                self.route = super::Route::Chat;
                self.perform(Intent::NewThreadOnBranch {
                    project_id,
                    branch,
                    worktree_path,
                });
                cx.notify();
            }
            ThreadMenuAction::CustomSnooze => {
                self.open_custom_snooze(vec![thread_id], surface, window, cx)
            }
            ThreadMenuAction::StartRename => self.start_rename(thread_id, surface, window, cx),
            ThreadMenuAction::OpenProjectSettings { project_id } => self.open_settings(
                SettingsPage::Projects {
                    project_id: Some(project_id),
                },
                window,
                cx,
            ),
            ThreadMenuAction::CopyPath { path: Some(path) } => {
                self.copy_with_toast(path, "Path copied", window, cx)
            }
            ThreadMenuAction::CopyPath { path: None } => window.push_notification(
                Notification::error("This thread does not have a workspace path to copy.")
                    .title("Path unavailable"),
                cx,
            ),
            ThreadMenuAction::CopyBranch { branch } => {
                self.copy_with_toast(branch, "Branch copied", window, cx)
            }
            ThreadMenuAction::CopyThreadId { thread_id } => {
                self.copy_with_toast(thread_id, "Thread ID copied", window, cx)
            }
            // The arrangement sheet and its moves belong to the mobile list.
            ThreadMenuAction::Arrange | ThreadMenuAction::Move { .. } => {}
        }
    }

    fn run_bulk_action(
        &mut self,
        thread_ids: Vec<String>,
        action: SidebarMenuAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rows: Vec<_> = thread_ids
            .iter()
            .filter_map(|id| self.views.sidebar.row(id).cloned())
            .collect();
        let each =
            |view: &mut Desktop,
             action: ThreadAction,
             keep: &dyn Fn(&agent_core::view::sidebar::SidebarThreadRow) -> bool| {
                for row in rows.iter().filter(|row| keep(row)) {
                    view.perform(Intent::Thread {
                        thread_id: row.id.clone(),
                        action: action.clone(),
                    });
                }
            };
        match action {
            SidebarMenuAction::Unpin => each(self, ThreadAction::Unpin, &|row| row.pinned),
            SidebarMenuAction::MarkUnread => each(self, ThreadAction::MarkUnread, &|_| true),
            SidebarMenuAction::RegenerateTitle => {
                each(self, ThreadAction::RegenerateTitle, &|row| {
                    !row.title_regenerating
                })
            }
            SidebarMenuAction::Archive => each(self, ThreadAction::Archive, &|_| true),
            SidebarMenuAction::Settle => {
                let ids = rows
                    .iter()
                    .filter(|row| row.section != agent_core::view::sidebar::SidebarSection::Settled)
                    .map(|row| row.id.clone())
                    .collect();
                self.park_threads(ids, ThreadAction::Settle, MenuSurface::Sidebar, window, cx);
            }
            // The presets live in the submenu.
            SidebarMenuAction::Snooze => {}
            SidebarMenuAction::Delete => {
                let ids: Vec<_> = rows.iter().map(|row| row.id.clone()).collect();
                let delete =
                    move |view: &mut Desktop, _: &mut Window, cx: &mut Context<Desktop>| {
                        for id in &ids {
                            view.perform(Intent::Thread {
                                thread_id: id.clone(),
                                action: ThreadAction::Delete,
                            });
                        }
                        view.clear_selection(cx);
                    };
                if ThreadMenuOptions::default().confirm_delete {
                    self.confirm(
                        crate::app::dialogs::Confirm {
                            title: None,
                            message: bulk_delete_confirmation(rows.len()),
                            action: "Delete".into(),
                            destructive: true,
                        },
                        window,
                        cx,
                        delete,
                    );
                } else {
                    delete(self, window, cx);
                }
                return;
            }
        }
        self.clear_selection(cx);
    }

    /// Settles or snoozes threads. From the sidebar, parking the open thread
    /// moves on to the next card, skipping the others parking with it.
    pub(crate) fn park_threads(
        &mut self,
        thread_ids: Vec<String>,
        action: ThreadAction,
        surface: MenuSurface,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let park = match action {
            ThreadAction::Snooze { .. } => ParkAction::Snooze,
            _ => ParkAction::Settle,
        };
        let current = self.thread_id();
        if surface == MenuSurface::Sidebar {
            self.clear_selection(cx);
        }
        for thread_id in &thread_ids {
            let intent = Intent::Thread {
                thread_id: thread_id.clone(),
                action: action.clone(),
            };
            let forward = match surface {
                MenuSurface::Sidebar => {
                    self.views
                        .sidebar
                        .forward_target(thread_id, current.as_deref(), &thread_ids)
                }
                MenuSurface::Header => None,
            };
            let Some(forward) = forward else {
                self.perform(intent);
                continue;
            };
            let thread_id = thread_id.clone();
            self.perform_then(intent, move |view, result, window, cx| match result {
                Err(error) => view.show_error(error, window, cx),
                Ok(_) if view.parked(&thread_id, park) => view.move_forward(forward, cx),
                Ok(_) => {}
            });
        }
    }

    fn parked(&self, thread_id: &str, park: ParkAction) -> bool {
        let summary = self.snapshot.shell_view().and_then(|shell| {
            shell
                .threads
                .iter()
                .find(|row| row.id.as_str() == thread_id)
                .map(ThreadSummary::from_shell)
        });
        should_navigate_after_thread_park(
            thread_id,
            self.thread_id().as_deref(),
            park,
            ui::now_ms(),
            summary.as_ref(),
        )
    }

    fn move_forward(&mut self, target: ForwardNavigation, cx: &mut Context<Self>) {
        match target {
            ForwardNavigation::Thread { id } => self.open_thread(id, cx),
            ForwardNavigation::NewThread { project_id } => self.new_thread(Some(project_id), cx),
            ForwardNavigation::Home => self.perform(Intent::LeaveThread),
        }
    }

    pub(crate) fn filter_project(&mut self, project_id: Option<String>, cx: &mut Context<Self>) {
        self.perform(Intent::FilterProject { project_id });
        self.sidebar_scope_changed(cx);
    }

    fn project_root(&self, project_id: &str) -> Option<String> {
        self.snapshot
            .shell_projects()
            .iter()
            .find(|project| project.id == project_id)
            .and_then(|project| project.roots.first())
            .map(|root| root.path.clone())
            .filter(|path| !path.is_empty())
    }

    fn copy_with_toast(
        &mut self,
        value: String,
        title: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.write_to_clipboard(ClipboardItem::new_string(value.clone()));
        window.push_notification(Notification::success(value).title(title.to_owned()), cx);
    }

    /// Edits the title in place where the menu opened.
    pub(crate) fn start_rename(
        &mut self,
        thread_id: String,
        surface: MenuSurface,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(original) = agent_domain::ThreadId::new(thread_id.clone())
            .ok()
            .and_then(|id| {
                self.snapshot.shell_view().and_then(|shell| {
                    shell
                        .threads
                        .iter()
                        .find(|row| row.id == id)
                        .map(|row| row.title.clone())
                })
            })
        else {
            return;
        };
        let input = cx.new(|cx| InputState::new(window, cx));
        input.update(cx, |input, cx| {
            input.set_value(original.clone(), window, cx);
            input.select_all(window, cx);
        });
        let events = cx.subscribe_in(&input, window, |view, _, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                view.commit_rename(window, cx);
            }
        });
        self.menus.rename = Some(Rename {
            thread_id,
            surface,
            original,
            input,
            _events: events,
        });
        cx.notify();
    }

    fn commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rename) = self.menus.rename.take() else {
            return;
        };
        cx.notify();
        match resolve_rename_commit(&rename.input.read(cx).value(), &rename.original) {
            RenameCommit::RejectEmpty => {
                window.push_notification(Notification::warning(EMPTY_TITLE_WARNING), cx)
            }
            RenameCommit::Noop => {}
            RenameCommit::Commit { title } => self.perform_then(
                Intent::Thread {
                    thread_id: rename.thread_id,
                    action: ThreadAction::Rename { title },
                },
                |_, result, window, cx| {
                    if let Err(error) = result {
                        window.push_notification(
                            Notification::error(agent_core::presentation::error::error_message(
                                error,
                            ))
                            .title(RENAME_FAILED),
                            cx,
                        );
                    }
                },
            ),
        }
    }

    /// The title input of an inline rename, with Escape to cancel.
    pub(crate) fn render_rename_input(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let rename = self.menus.rename.as_ref()?;
        Some(
            div()
                .flex_1()
                .min_w_0()
                .on_action(cx.listener(|view, _: &Escape, _, cx| {
                    view.menus.rename = None;
                    cx.notify();
                }))
                .child(Input::new(&rename.input).small().aria_label("Thread title"))
                .into_any_element(),
        )
    }

    fn open_custom_snooze(
        &mut self,
        thread_ids: Vec<String>,
        surface: MenuSurface,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let desktop = cx.entity().downgrade();
        let form = cx.new(|cx| CustomSnooze::new(desktop, thread_ids, surface, window, cx));
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title("Custom snooze")
                .w(px(384.))
                .child(form.clone())
        });
    }
}

/// The custom snooze dialog: a date and time, or a duration from now.
struct CustomSnooze {
    desktop: WeakEntity<Desktop>,
    thread_ids: Vec<String>,
    surface: MenuSurface,
    duration: bool,
    date: Entity<DatePickerState>,
    time: Entity<InputState>,
    amount: Entity<InputState>,
    unit: SnoozeDurationUnit,
    error: Option<&'static str>,
    _subscriptions: Vec<Subscription>,
}

fn unit_label(unit: SnoozeDurationUnit) -> &'static str {
    match unit {
        SnoozeDurationUnit::Minutes => "Minutes",
        SnoozeDurationUnit::Hours => "Hours",
        SnoozeDurationUnit::Days => "Days",
    }
}

impl CustomSnooze {
    fn new(
        desktop: WeakEntity<Desktop>,
        thread_ids: Vec<String>,
        surface: MenuSurface,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let initial = Local::now() + chrono::Duration::hours(1);
        let today = Local::now().date_naive();
        let date = cx.new(|cx| {
            let mut picker = DatePickerState::new(window, cx)
                .date_format("%b %-d, %Y")
                .disabled_matcher(move |day: &NaiveDate| *day < today);
            picker.set_date(initial.date_naive(), window, cx);
            picker
        });
        let time = cx.new(|cx| InputState::new(window, cx).placeholder("HH:MM"));
        let amount = cx.new(|cx| InputState::new(window, cx));
        time.update(cx, |input, cx| {
            input.set_value(local_snooze_time(&initial), window, cx)
        });
        amount.update(cx, |input, cx| input.set_value("2", window, cx));
        let subscriptions = vec![
            cx.subscribe_in(&date, window, |form, _, _: &DatePickerEvent, _, cx| {
                form.error = None;
                cx.notify();
            }),
            cx.subscribe_in(&time, window, Self::input_event),
            cx.subscribe_in(&amount, window, Self::input_event),
            cx.subscribe_in(
                &amount,
                window,
                |_, amount, event: &NumberInputEvent, window, cx| {
                    let NumberInputEvent::Step(step) = event;
                    amount.update(cx, |input, cx| {
                        let value: f64 = input.value().trim().parse().unwrap_or(0.);
                        let next = match step {
                            StepAction::Increment => value + 1.,
                            StepAction::Decrement => (value - 1.).max(0.),
                        };
                        input.set_value(next.to_string(), window, cx);
                    });
                },
            ),
        ];
        Self {
            desktop,
            thread_ids,
            surface,
            duration: false,
            date,
            time,
            amount,
            unit: SnoozeDurationUnit::Hours,
            error: None,
            _subscriptions: subscriptions,
        }
    }

    fn input_event(
        &mut self,
        _: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                self.error = None;
                cx.notify();
            }
            InputEvent::PressEnter { .. } => self.submit(window, cx),
            _ => {}
        }
    }

    fn input(&self, cx: &App) -> CustomSnoozeInput {
        if self.duration {
            return CustomSnoozeInput::Duration {
                amount: self.amount.read(cx).value().to_string(),
                unit: self.unit,
            };
        }
        let date = match self.date.read(cx).date() {
            Date::Single(Some(day)) => day
                .and_hms_opt(12, 0, 0)
                .and_then(|noon| Local.from_local_datetime(&noon).earliest())
                .map(|noon| local_snooze_date(&noon))
                .unwrap_or_default(),
            _ => String::new(),
        };
        CustomSnoozeInput::Date {
            date,
            time: self.time.read(cx).value().trim().to_owned(),
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(until) = resolve_custom_snooze(&self.input(cx), &Local::now()) else {
            self.error = Some(if self.duration {
                "Enter a positive duration."
            } else {
                "Choose a valid date and time in the future."
            });
            cx.notify();
            return;
        };
        let thread_ids = self.thread_ids.clone();
        let surface = self.surface;
        let _ = self.desktop.update(cx, |view, cx| {
            view.park_threads(
                thread_ids,
                ThreadAction::Snooze { until },
                surface,
                window,
                cx,
            )
        });
        window.close_dialog(cx);
    }

    fn field(label: &'static str, control: impl IntoElement) -> Div {
        v_flex()
            .flex_1()
            .min_w_0()
            .gap_1p5()
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(label))
            .child(control)
    }
}

impl Render for CustomSnooze {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mode = |id: &'static str, label: &'static str, duration: bool| {
            Button::new(id)
                .label(label)
                .small()
                .outline()
                .flex_1()
                .selected(self.duration == duration)
                .on_click(cx.listener(move |form, _, _, cx| {
                    form.duration = duration;
                    form.error = None;
                    cx.notify();
                }))
        };
        let current = self.unit;
        let entity = cx.entity().downgrade();
        let fields = if self.duration {
            h_flex()
                .gap_3()
                .child(Self::field("Snooze for", NumberInput::new(&self.amount)))
                .child(Self::field(
                    "Unit",
                    Button::new("snooze-unit")
                        .label(unit_label(current))
                        .outline()
                        .w_full()
                        .dropdown_caret(true)
                        .dropdown_menu(move |mut menu, _, _| {
                            for unit in [
                                SnoozeDurationUnit::Minutes,
                                SnoozeDurationUnit::Hours,
                                SnoozeDurationUnit::Days,
                            ] {
                                let entity = entity.clone();
                                menu = menu.item(
                                    PopupMenuItem::new(unit_label(unit))
                                        .checked(unit == current)
                                        .on_click(move |_, _, cx| {
                                            let _ = entity.update(cx, |form, cx| {
                                                form.unit = unit;
                                                form.error = None;
                                                cx.notify();
                                            });
                                        }),
                                );
                            }
                            menu
                        }),
                ))
        } else {
            h_flex()
                .gap_3()
                .child(Self::field(
                    "Date",
                    DatePicker::new(&self.date).placeholder("Choose snooze date"),
                ))
                .child(Self::field("Time", Input::new(&self.time)))
        };
        v_flex()
            .gap_4()
            .child(
                div()
                    .text_sm()
                    .text_color(color("textMuted"))
                    .child("Choose when snoozed threads return to your inbox."),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_1()
                    .child(mode("snooze-date", "Date and time", false))
                    .child(mode("snooze-duration", "Duration", true)),
            )
            .child(fields)
            .when_some(self.error, |form, error| {
                form.child(
                    div()
                        .text_sm()
                        .text_color(color("errorForeground"))
                        .child(error),
                )
            })
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("snooze-cancel")
                            .label("Cancel")
                            .outline()
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("snooze-submit")
                            .label("Snooze")
                            .primary()
                            .on_click(cx.listener(|form, _, window, cx| form.submit(window, cx))),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{ThreadAction, ThreadMenuAction, confirm_label};

    #[test]
    fn confirmations_name_the_action_they_run() {
        let label = |action| confirm_label(&ThreadMenuAction::Thread { action });
        assert_eq!(label(ThreadAction::Delete), "Delete");
        assert_eq!(label(ThreadAction::Archive), "Archive");
        assert_eq!(label(ThreadAction::Unpin), "Unpin");
        assert_eq!(label(ThreadAction::Pin), "Confirm");
    }
}

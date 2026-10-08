//! Adding a project: a path field that browses the Host's folders.
use crate::app::{
    Desktop, Route,
    ui::{color, icon, tint},
};
use agent_core::{
    state::Intent,
    view::projects::{
        add::{
            add_project_initial_query, filesystem_browse_path, filter_filesystem_browse_entries,
            find_existing_add_project, resolve_add_project_path,
        },
        paths::append_browse_path_segment,
    },
};
use gpui_kit::{
    component::{
        Disableable, Sizable, StyledExt, WindowExt,
        button::{Button, ButtonVariants},
        h_flex,
        input::{Input, InputEvent, InputState},
        notification::Notification,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::path::PathBuf;

/// Opens the add-project field at the Host's configured base folder.
pub(super) fn open(view: &mut Desktop, window: &mut Window, cx: &mut Context<Desktop>) {
    let local = view.remote.is_none();
    // The local Host runs as this user, so its home folder is ours; a remote
    // Host's is unknown until the Host lists paths with `~`.
    let home_path = local
        .then(|| directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()))
        .flatten();
    let home = home_path
        .as_deref()
        .map(|path| path.to_string_lossy().into_owned());
    let configured_base_directory = view
        .snapshot
        .host_settings
        .as_ref()
        .map(|settings| settings.add_project_base_directory.trim().to_owned());
    let initial = match configured_base_directory.as_deref() {
        Some(base_directory) => add_project_initial_query(Some(base_directory)),
        None => add_project_initial_query(home.as_deref()),
    };
    let initial_directory = configured_base_directory
        .as_deref()
        .and_then(|base_directory| expand_local_directory(base_directory, home_path.as_deref()));
    let platform = if local { std::env::consts::OS } else { "" };
    let desktop = cx.entity().downgrade();
    let form = cx.new(|cx| {
        AddProject::new(
            desktop,
            initial,
            initial_directory,
            platform,
            local,
            window,
            cx,
        )
    });
    window.open_dialog(cx, move |dialog, _, _| {
        dialog.w(px(576.)).p_0().child(form.clone())
    });
}

impl Desktop {
    /// Adds the project at `raw_path`, or opens it when it is already one.
    fn add_project_at(
        &mut self,
        raw_path: &str,
        platform: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self
            .snapshot
            .selected_project
            .as_ref()
            .and_then(|id| {
                self.snapshot
                    .shell_projects()
                    .iter()
                    .find(|project| &project.id == id)
            })
            .and_then(|project| project.roots.first())
            .map(|root| root.path.clone());
        let path = match resolve_add_project_path(raw_path, current.as_deref(), platform) {
            Ok(path) => path,
            Err(error) => {
                window.push_notification(
                    Notification::error(error).title("Failed to add project"),
                    cx,
                );
                return;
            }
        };
        if let Some(existing) = find_existing_add_project(self.snapshot.shell_projects(), &path) {
            let project = existing.id.clone();
            window.close_dialog(cx);
            if self.snapshot.selected_thread.is_none()
                && self.snapshot.open_new_thread_draft.is_some()
            {
                self.perform(Intent::SetNewThreadProject {
                    project_id: Some(project),
                });
            } else {
                self.new_thread(Some(project), cx);
            }
            return;
        }
        self.perform_then(
            Intent::AddProject { path },
            |view, result, window, cx| match result {
                Ok(_) => {
                    window.close_dialog(cx);
                    let project = view.snapshot.selected_project.clone();
                    if view.snapshot.selected_thread.is_none()
                        && view.snapshot.open_new_thread_draft.is_some()
                    {
                        view.perform(Intent::SetNewThreadProject {
                            project_id: project,
                        });
                    } else {
                        view.new_thread(project, cx);
                    }
                }
                Err(error) => {
                    let message = agent_core::presentation::error::error_message(error);
                    window.push_notification(
                        Notification::error(message).title("Failed to add project"),
                        cx,
                    );
                }
            },
        );
    }
}

struct AddProject {
    desktop: WeakEntity<Desktop>,
    input: Entity<InputState>,
    initial_directory: Option<PathBuf>,
    platform: &'static str,
    local: bool,
    /// The folder last requested and whether its listing arrived.
    requested: Option<(String, bool)>,
    failed: Option<String>,
    _subscription: Subscription,
}

impl AddProject {
    fn new(
        desktop: WeakEntity<Desktop>,
        initial: String,
        initial_directory: Option<PathBuf>,
        platform: &'static str,
        local: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Enter project path (e.g. ~/projects/my-app)")
        });
        input.update(cx, |input, cx| {
            input.set_value(initial, window, cx);
            input.focus(window, cx);
        });
        let subscription =
            cx.subscribe_in(&input, window, |form, _, event, window, cx| match event {
                InputEvent::Change => form.browse(cx),
                InputEvent::PressEnter { .. } => form.submit(window, cx),
                _ => {}
            });
        let mut form = Self {
            desktop,
            input,
            initial_directory,
            platform,
            local,
            requested: None,
            failed: None,
            _subscription: subscription,
        };
        form.browse(cx);
        form
    }

    fn connected(&self, cx: &App) -> bool {
        self.desktop
            .upgrade()
            .is_some_and(|desktop| desktop.read(cx).snapshot.connected)
    }

    /// Lists the folder the field names, once per folder.
    fn browse(&mut self, cx: &mut Context<Self>) {
        let query = self.input.read(cx).value().to_string();
        let browse = filesystem_browse_path(&query, self.platform, self.connected(cx));
        if !browse.is_browsing
            || self.requested.as_ref().map(|(path, _)| path) == Some(&browse.directory_path)
        {
            cx.notify();
            return;
        }
        let path = browse.directory_path;
        self.requested = Some((path.clone(), false));
        self.failed = None;
        let form = cx.entity().downgrade();
        let _ = self.desktop.update(cx, |view, _| {
            view.perform_then(
                Intent::ListFiles { path: path.clone() },
                move |_, result, _, cx| {
                    let failed = result
                        .as_ref()
                        .err()
                        .map(|error| agent_core::presentation::error::error_message(error));
                    let _ = form.update(cx, |form, cx| {
                        if form.requested.as_ref().map(|(requested, _)| requested) == Some(&path) {
                            form.requested = Some((path, true));
                            form.failed = failed;
                            cx.notify();
                        }
                    });
                },
            );
        });
        cx.notify();
    }

    fn set_query(&mut self, query: String, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| {
            input.set_value(query, window, cx);
            input.focus(window, cx);
        });
        self.browse(cx);
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.input.read(cx).value().to_string();
        self.add(query, window, cx);
    }

    fn add(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let platform = self.platform;
        let _ = self.desktop.update(cx, |view, cx| {
            view.add_project_at(&path, platform, window, cx)
        });
    }

    fn pick_folder(&mut self, cx: &mut Context<Self>) {
        let platform = self.platform;
        let initial_directory = self.initial_directory.clone();
        let _ = self.desktop.update(cx, |view, _| {
            view.spawn_task(
                async move {
                    tokio::task::spawn_blocking(move || {
                        crate::platform::choose_folder(initial_directory.as_deref())
                    })
                    .await
                    .ok()
                    .flatten()
                },
                move |view, picked: Option<PathBuf>, window, cx| {
                    if let Some(path) = picked {
                        view.add_project_at(&path.to_string_lossy(), platform, window, cx);
                    }
                },
            );
        });
    }
}

fn expand_local_directory(value: &str, home: Option<&std::path::Path>) -> Option<PathBuf> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if value == "~" {
        return home.map(PathBuf::from);
    }
    value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
        .and_then(|relative| home.map(|home| home.join(relative)))
        .or_else(|| Some(PathBuf::from(value)))
}

fn file_manager_name() -> &'static str {
    match std::env::consts::OS {
        "macos" => "Finder",
        "windows" => "File Explorer",
        _ => "Files",
    }
}

impl Render for AddProject {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snapshot = self
            .desktop
            .upgrade()
            .map(|desktop| desktop.read(cx).snapshot.clone())
            .unwrap_or_default();
        let query = self.input.read(cx).value().to_string();
        let browse = filesystem_browse_path(&query, self.platform, snapshot.connected);
        let listed = self.requested.as_ref().is_some_and(|(path, arrived)| {
            *arrived
                && path == &browse.directory_path
                && snapshot.workspace.requested_directory.as_ref() == Some(path)
        });
        let entries = snapshot
            .workspace
            .directory
            .as_ref()
            .filter(|_| listed && self.failed.is_none())
            .map(|directory| {
                filter_filesystem_browse_entries(&directory.entries, &browse.filter_query).visible
            })
            .unwrap_or_default();
        let mut items = v_flex().gap(px(2.));
        if browse.can_browse_up
            && let Some(parent) = browse.parent_path.clone()
        {
            items = items.child(browse_item("browse-up", "corner-left-up", "..").on_click(
                cx.listener(move |form, _, window, cx| form.set_query(parent.clone(), window, cx)),
            ));
        }
        for (index, entry) in entries.into_iter().enumerate() {
            let next = append_browse_path_segment(&browse.directory_path, &entry.name);
            items = items.child(
                browse_item(("browse", index), "folder", entry.name.clone()).on_click(
                    cx.listener(move |form, _, window, cx| {
                        form.set_query(next.clone(), window, cx)
                    }),
                ),
            );
        }
        let status = if !browse.is_browsing {
            None
        } else if let Some(error) = &self.failed {
            Some(error.clone())
        } else if !listed {
            Some("Loading…".to_owned())
        } else {
            None
        };
        v_flex()
            .child(
                h_flex()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(color("border"))
                    .child(icon("folder-plus").size_4().text_color(color("textMuted")))
                    .child(
                        Input::new(&self.input)
                            .flex_1()
                            .appearance(false)
                            .aria_label("Project path"),
                    )
                    .child(
                        Button::new("add-project-submit")
                            .outline()
                            .xsmall()
                            .label("Add")
                            .disabled(!snapshot.connected)
                            .tooltip("Add (Enter)")
                            .on_click(cx.listener(|form, _, window, cx| form.submit(window, cx))),
                    ),
            )
            .child(
                div()
                    .id("add-project-list")
                    .max_h(px(320.))
                    .overflow_y_scroll()
                    .p_2()
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .text_xs()
                            .font_medium()
                            .text_color(color("textMuted"))
                            .child("Directories"),
                    )
                    .child(items)
                    .when_some(status, |list, status| {
                        list.child(
                            div()
                                .px_2()
                                .py_4()
                                .text_xs()
                                .text_color(color("textMuted"))
                                .child(status),
                        )
                    }),
            )
            .when(self.local, |form| {
                form.child(
                    h_flex()
                        .justify_end()
                        .px_3()
                        .py_2()
                        .border_t_1()
                        .border_color(color("border"))
                        .child(
                            Button::new("add-project-pick")
                                .ghost()
                                .xsmall()
                                .label(format!("Open in {}", file_manager_name()))
                                .on_click(cx.listener(|form, _, _, cx| form.pick_folder(cx))),
                        ),
                )
            })
    }
}

fn browse_item(
    id: impl Into<ElementId>,
    icon_name: &'static str,
    label: impl Into<SharedString>,
) -> Stateful<Div> {
    h_flex()
        .id(id)
        .h_8()
        .px_2()
        .gap_2()
        .rounded(px(6.))
        .text_sm()
        .cursor_pointer()
        .hover(|row| row.bg(tint("accentSurface", 1.)))
        .child(icon(icon_name).size_4().text_color(color("textMuted")))
        .child(div().truncate().child(label.into()))
}

impl Desktop {
    /// Leaves settings for the conversation the import landed on.
    pub(super) fn finish_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.route = Route::Chat;
        self.sync_settings(window, cx);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::expand_local_directory;
    use std::path::Path;

    #[test]
    fn expands_configured_project_folder_against_the_local_home() {
        let home = Path::new("/Users/tester");
        assert_eq!(
            expand_local_directory("~/Development", Some(home)).as_deref(),
            Some(Path::new("/Users/tester/Development"))
        );
        assert_eq!(
            expand_local_directory("/Volumes/work", Some(home)).as_deref(),
            Some(Path::new("/Volumes/work"))
        );
        assert_eq!(expand_local_directory("  ", Some(home)), None);
    }
}

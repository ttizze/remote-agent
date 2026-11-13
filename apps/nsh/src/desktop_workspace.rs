//! Native workspace controls consume Core publications and keep Host operations in Core.
use crate::details::WorkspaceSummary;
use crate::l10n::tr;
use crate::{Connection, workbench::Workbench};
use agent_core::{
    panels::PanelKind,
    presentation::conversation::{DETAILS_CARD_GAP, DETAILS_CARD_WIDTH},
    state::{Intent, operations as op},
};
use agent_protocol::editors::EditorId;
use nsh_core::{
    overlay::{Align, Placement},
    prelude::*,
};
use serde_json::json;
mod actions;
mod card;

/// Starts a new conversation in a folder the user picks: in a WSL distro's
/// folder on that distro's Host, from which the dialog starts while it is chosen.
pub(crate) fn choose_folder(connection: Connection) {
    let dialog = nsh_core::file_dialog::OpenDialog::folders();
    let start =
        agent_local_host::wsl::host_distro(&connection.host.get_untracked()).and_then(|distro| {
            let cwd = connection
                .composer
                .with_untracked(|c| c.controls.navigation.cwd.clone());
            let cwd = if cwd.is_empty() {
                connection
                    .context
                    .with_untracked(|c| c.home())
                    .unwrap_or_else(|| "/home".into())
            } else {
                cwd
            };
            agent_local_host::wsl::to_unc(distro, &cwd).ok()
        });
    let choice = pick_paths(match start {
        Some(start) => dialog.directory(start),
        None => dialog,
    });
    spawn(async move {
        let picked = match choice.await {
            Ok(Some(paths)) => paths.into_iter().next(),
            Ok(None) => None,
            Err(reason) => return crate::native::message(tr("フォルダー"), reason),
        };
        let Some(path) = picked else {
            return;
        };
        match connection
            .application
            .with(|application| application.open_wsl_folder(&path, |cwd| Intent::NewChat { cwd }))
        {
            Ok(true) => {}
            Ok(false) => connection.dispatch(Intent::NewChat {
                cwd: path.to_string_lossy().into_owned(),
            }),
            Err(reason) => crate::native::message(tr("フォルダー"), reason),
        }
    });
}

/// The same details card stays available as a popover when the chat lane
/// cannot leave room for it. The inline visibility belongs to the shell;
/// dismissing a popover does not change that preference.
pub(crate) fn toggle(
    visible: Signal<bool>,
    inline: impl Fn() -> bool + Copy + 'static,
) -> impl IntoElement {
    use crate::ui::{self, Icon};
    view()
        .child(when(inline, move || {
            ui::toggle(
                Icon::DETAILS,
                tr("スレッド詳細を切り替え"),
                move || visible.get(),
                move || visible.update(|visible| *visible = !*visible),
            )
        }))
        .child(when(
            move || !inline(),
            move || {
                let open = signal(false);
                nsh_base::popover()
                    .open(move || Some(open.get()))
                    .on_open_change(move |event: &nsh_base::PopoverEvent| {
                        if let nsh_base::PopoverEvent::OpenChange(next, _) = event {
                            open.set(*next);
                        }
                    })
                    .placement(Placement::Bottom)
                    .align(Align::End)
                    .offset(px(DETAILS_CARD_GAP))
                    .margin(px(DETAILS_CARD_GAP))
                    .trigger(ui::toggle(
                        Icon::DETAILS,
                        tr("スレッド詳細を切り替え"),
                        move || open.get(),
                        || {},
                    ))
                    .child(when(move || open.get(), card))
            },
        ))
}

pub(crate) fn card() -> impl IntoElement {
    let connection = use_context::<Connection>().unwrap();
    let workspace = use_context::<Workbench>().unwrap();
    let files = use_context::<crate::files::Browser>().unwrap();
    let target = computed(move || {
        connection.composer.with(|p| {
            (
                connection.host.get(),
                p.controls.connected,
                connection.context.with(|p| p.selected_directory()),
            )
        })
    });
    effect(move || {
        let (host, connected, cwd) = target.get();
        if connected {
            if !cwd.is_empty() {
                connection.dispatch(Intent::ReviewWorkspace(op::ReviewWorkspace {
                    cwd,
                    base_ref: None,
                }));
            }
            if host == "local"
                && !connection
                    .publication
                    .with_untracked(|p| p.snapshot.files.file_open_capabilities_requested)
            {
                connection.dispatch(Intent::ReadFileOpenCapabilities(
                    op::ReadFileOpenCapabilities {},
                ));
            }
        }
    });
    let summary = computed(move || {
        connection
            .publication
            .with(|p| crate::details::workspace_summary(p, connection.host.with(|h| h == "local")))
    });
    let actions = actions::Actions::new(connection, summary);
    let act = move |action: Action| {
        let (cwd, home) = connection
            .publication
            .with_untracked(|p| (p.context.selected_directory(), p.context.home()));
        match action {
            Action::Branch => actions.branch(),
            Action::Push => actions.push(),
            Action::AddScript => actions.add_script(),
            Action::ManageScript(script) => actions.script_menu(script),
            Action::RunScript(command) if !cwd.is_empty() => workspace.run_script(cwd, command),
            Action::Refresh if !cwd.is_empty() => {
                connection.dispatch(Intent::ReviewWorkspace(op::ReviewWorkspace {
                    cwd,
                    base_ref: None,
                }))
            }
            Action::Diff if !cwd.is_empty() => workspace.open(PanelKind::Diff, Some(cwd), None),
            Action::RunScript(_) | Action::Refresh | Action::Diff => {}
            Action::Files if !cwd.is_empty() => workspace.open(PanelKind::Files, Some(cwd), None),
            Action::Files => choose_folder(connection),
            Action::Attach => crate::composer::pick_files(
                connection.application.get(),
                connection.host.get_untracked(),
                connection.pane,
                connection
                    .composer
                    .with_untracked(|p| p.controls.navigation.draft_key.clone()),
            ),
            Action::Source(path) => files.open(
                path,
                cwd,
                home,
                connection.host.get_untracked(),
                connection.client.get(),
            ),
            Action::PreferEditor(editor) => {
                connection.dispatch(Intent::SetPreferredEditor { editor })
            }
            Action::OpenEditor(editor) if !cwd.is_empty() => {
                connection.dispatch(Intent::OpenInEditor(op::OpenInEditor {
                    path: cwd,
                    line: None,
                    column: None,
                    editor,
                    reveal: false,
                }))
            }
            Action::OpenEditor(_) => {}
        }
    };
    view()
        .w(px(DETAILS_CARD_WIDTH))
        .min_h_0()
        .rounded(px(16.0))
        .border_1()
        .border_color(crate::ui::colors::line(0x292929))
        .bg(nsh_core::element::rgb(0x111111))
        .overflow_hidden()
        .test_id("thread-details-card")
        .label(tr("ワークスペース"))
        .child(
            scroll()
                .max_h(move || {
                    px(
                        (window().height.0 - crate::ui::HEADER_HEIGHT - DETAILS_CARD_GAP * 2.0)
                            .max(0.0),
                    )
                })
                .child(card::summary_card(summary, act)),
        )
        .child(actions.view())
}

enum Action {
    Branch,
    Push,
    AddScript,
    ManageScript(agent_protocol::models::ProjectScript),
    RunScript(String),
    Refresh,
    Diff,
    Files,
    Attach,
    Source(String),
    OpenEditor(EditorId),
    PreferEditor(EditorId),
}

fn separator() -> View {
    view()
        .h(px(1.0))
        .my_2()
        .bg(crate::ui::colors::line(0x232323))
}

/// T3's details row: 32 pt, 13 pt medium text, a 16 pt muted icon.
fn row(glyph: crate::ui::Icon, title: Text) -> nsh_base::ButtonElement {
    use crate::ui::{self, colors};
    nsh_base::button()
        .h(px(32.0))
        .px(px(10.0))
        .gap(px(10.0))
        .flex_row()
        .items_center()
        .rounded(px(8.0))
        .hover(|s| s.bg(nsh_core::element::rgba(0xffffff14)))
        .disabled_style(|s| s.opacity(0.64))
        .child(
            view()
                .text_color(colors::t3_muted())
                .child(ui::icon(glyph, 16.0)),
        )
        .child(
            title
                .flex_1()
                .min_w_0()
                .text_size(px(13.0))
                .font_weight(FontWeight::MEDIUM)
                .number_of_lines(1),
        )
}

/// Picks a folder rather than one of the projects in the draft's menu.
const CHOOSE_FOLDER: &str = "choose-folder";

/// A new conversation's headline above the centered composer, as T3 Code's
/// draft hero: the project is a menu of this Host's projects.
pub(crate) fn draft_hero() -> View {
    use crate::ui::{self, colors};
    let connection = use_context::<Connection>().unwrap();
    let directory = computed(move || connection.context.with(|p| p.selected_directory()));
    let projects = computed(move || {
        let host = connection.host.get();
        connection.state.with(|state| {
            state
                .list_inputs()
                .into_iter()
                .filter(|(list_host, _)| *list_host == host)
                .flat_map(|(_, list)| {
                    list.projects
                        .iter()
                        .filter_map(|project| {
                            Some((project.name.clone(), project.roots.first()?.path.clone()))
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        })
    });
    let project = move || {
        connection
            .context
            .with(|p| p.workspace_project().map(|project| project.name))
            .unwrap_or_else(|| {
                agent_core::presentation::markdown::file_basename(&directory.get()).to_owned()
            })
    };
    let choose = move || {
        let selected = directory.get_untracked();
        let mut items = projects
            .get_untracked()
            .into_iter()
            .map(|(name, root)| json!({"id": root, "title": name, "selected": root == selected}))
            .collect::<Vec<_>>();
        if !items.is_empty() {
            items.push(json!({"separator": true}));
        }
        items.push(json!({"id": CHOOSE_FOLDER, "title": tr("フォルダを選択…")}));
        ui::popup(&json!(items), move |id| {
            if id == CHOOSE_FOLDER {
                choose_folder(connection);
            } else if id != selected {
                connection.dispatch(Intent::NewChat { cwd: id.to_owned() });
            }
        });
    };
    let chat = move || directory.with(String::is_empty);
    // T3 Code's `h1`: `sm:text-3xl` (30 on 36), `tracking-tight`, the
    // foreground; the line under it `mt-2 h-6 text-sm`.
    let heading = |label: SharedString| {
        text(label)
            .text_size(px(30.0))
            .line_height(px(36.0))
            .letter_spacing(px(-0.75))
            .text_color(colors::t3_foreground())
    };
    view()
        .w_full()
        .items_center()
        .pb_4()
        .child(when(
            move || !chat(),
            move || {
                // The project menu takes the template's slot, wherever the language puts it.
                let headline = tr("{0}で何を作りますか？");
                let (before, after) = headline.split_once("{0}").unwrap_or((headline, ""));
                view()
                    .flex_row()
                    .items_baseline()
                    .justify_center()
                    .max_w(relative(1.0))
                    .child(heading(before.into()))
                    .child(project_menu(project, 30.0, colors::t3_foreground(), choose))
                    .child(heading(after.into()))
            },
        ))
        .child(when(
            move || !chat(),
            move || {
                view().mt_2().h(px(24.0)).justify_center().child(
                    // T3 Code's `muted` inline button.
                    nsh_base::button()
                        .text_color(colors::t3_muted())
                        .hover(|s| s.text_color(colors::t3_foreground()))
                        .on_press(move || {
                            connection.dispatch(Intent::NewChat { cwd: String::new() })
                        })
                        .child(
                            text(tr("プロジェクトなしで始める"))
                                .text_size(px(14.0))
                                .font_weight(FontWeight::MEDIUM),
                        ),
                )
            },
        ))
        .child(when(chat, move || {
            heading(tr("何に取り組みますか？").into())
        }))
        .child(when(chat, move || {
            project_menu(
                || tr("プロジェクトなし").to_owned(),
                14.0,
                colors::t3_muted(),
                choose,
            )
        }))
}

/// The draft's project, as T3 Code's `picker` inline button in the
/// headline's type: a choice marked by its dotted underline, at most 16rem.
fn project_menu(
    label: impl Fn() -> String + Copy + 'static,
    size: f32,
    color: Rgba,
    choose: impl Fn() + 'static,
) -> nsh_base::ButtonElement {
    nsh_base::button()
        .min_w_0()
        .max_w(px(256.0))
        .text_color(color)
        .accessibility_label(Some(SharedString::from(tr("プロジェクトを選択"))))
        .on_press(choose)
        .child(
            text(label)
                .text_size(px(size))
                .font_weight(FontWeight::MEDIUM)
                .letter_spacing(px(-0.025 * size))
                .underline()
                .text_decoration_dotted()
                .text_decoration_color(color.alpha(0.3))
                .number_of_lines(1)
                .min_w_0(),
        )
}

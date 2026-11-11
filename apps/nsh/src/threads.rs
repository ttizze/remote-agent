use crate::application::{ListRequest, ListScope, ListSource};
use crate::l10n::{localized, tr, tr_format};
use crate::shared::Shared;
use crate::{Connection, native};
use agent_core::{
    presentation::list::{ThreadCopyItem, ThreadList, ThreadSummary},
    state::{Intent, operations as op},
};
use nsh_core::prelude::*;
use serde_json::json;

#[derive(Clone, PartialEq, serde::Serialize)]
enum Action {
    Open {
        thread: agent_protocol::session::SessionRef,
        project: Option<String>,
    },
    Expand(String),
    New {
        project: String,
        cwd: String,
    },
    More(Option<String>),
    Projects,
    ProjectHeader,
    Heading(Option<String>),
    Notice(String),
    Loading(String),
    Retry(String),
}
#[derive(Clone, PartialEq)]
struct Row {
    scope: Option<ListScope>,
    title: String,
    png: Option<Vec<u8>>,
    glyph: Option<(String, u32)>,
    level: u32,
    disclosure: bool,
    expanded: bool,
    action: Action,
    compose: Option<Action>,
    menu: serde_json::Value,
    indicators: Option<native::RowIndicators>,
    loading: bool,
    enabled: bool,
}
impl Row {
    fn new(title: String, action: Action, scope: Option<ListScope>) -> Self {
        let enabled = !matches!(action, Action::Notice(_) | Action::Loading(_));
        let loading = matches!(action, Action::Loading(_));
        Self {
            scope,
            title,
            action,
            compose: None,
            menu: serde_json::Value::Null,
            png: None,
            glyph: None,
            level: 0,
            disclosure: false,
            expanded: false,
            indicators: None,
            loading,
            enabled,
        }
    }
}
/// A conversation's menu, as the desktop client's: a Copy submenu of its
/// path, branch, title and ID, each item off when the conversation lacks it.
fn copy_menu(items: &[ThreadCopyItem]) -> serde_json::Value {
    if items.is_empty() {
        return serde_json::Value::Null;
    }
    json!([{
        "title": tr("コピー"),
        "children": items.iter().map(|item| json!({
            "title": item.label.clone(),
            "id": item.value.as_ref().map(|value| json!({"copy": value}).to_string()).unwrap_or_default(),
            "enabled": item.value.is_some(),
        })).collect::<Vec<_>>(),
    }])
}

/// The value a Copy item of a conversation's menu puts on the clipboard.
fn copied(id: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(id).ok()?["copy"]
        .as_str()
        .map(str::to_owned)
}

fn thread_row(thread: &ThreadSummary, level: u32, project: Option<&str>, scope: &ListScope) -> Row {
    let activity = agent_core::presentation::thread_status::thread_activity(
        thread.waiting,
        thread.active,
        thread.background,
        thread.unread,
    );
    Row {
        level,
        menu: copy_menu(&thread.copy_items),
        indicators: (activity.is_some()
            || thread.attention.is_some()
            || thread.has_changes == Some(true))
        .then_some(native::RowIndicators {
            activity,
            attention: thread.attention,
            changes: thread.has_changes == Some(true),
        }),
        ..Row::new(
            thread.title.clone(),
            Action::Open {
                thread: thread.id.clone(),
                project: project.map(str::to_owned),
            },
            Some(scope.clone()),
        )
    }
}

fn rows(
    list: Option<&ThreadList>,
    dark: bool,
    sources: &[ListSource],
    selected_host: &str,
    desktop: bool,
) -> Vec<Row> {
    let mut rows = Vec::new();
    if desktop {
        rows.push(Row {
            menu: crate::projects::header_menu(
                list.map_or(&[], |list| list.hidden_projects.as_slice()),
                desktop,
                selected_host,
                sources,
            ),
            ..Row::new(tr("プロジェクト").into(), Action::ProjectHeader, None)
        });
    }
    let Some(list) = list else {
        return rows;
    };
    let more_title = if desktop {
        tr("もっと表示する")
    } else {
        tr("もっと見る")
    };
    if let Some(notice) = &list.notice {
        rows.push(Row::new(
            notice.clone(),
            Action::Notice("list".into()),
            None,
        ));
    }
    let sections = list
        .projects
        .iter()
        .filter_map(|project| project.section.clone())
        .collect::<Vec<_>>();
    let headings = agent_core::presentation::list::project_headings(
        list.projects
            .iter()
            .map(|project| (project.pinned, project.section.as_deref())),
        tr("プロジェクト"),
        tr("ピン留め"),
    );
    for (project, heading) in list.projects.iter().zip(headings) {
        let Some(source) = sources
            .iter()
            .find(|source| source.scope.host == project.host_id)
        else {
            continue;
        };
        if let Some(heading) = heading.filter(|_| desktop) {
            rows.push(Row::new(
                heading.into(),
                Action::Heading(Some(project.id.clone())),
                Some(source.scope.clone()),
            ));
        }
        rows.push(Row {
            // The desktop sidebar is drawn dark whatever the system's
            // appearance, as the existing desktop client's.
            png: project.icon.as_ref().map(|icon| {
                if dark || desktop {
                    &icon.dark_png
                } else {
                    &icon.light_png
                }
                .clone()
            }),
            glyph: Some((project.monogram.clone(), project.icon_color)),
            disclosure: true,
            expanded: project.expanded,
            compose: (!desktop || !project.roots.is_empty()).then(|| Action::New {
                project: project.id.clone(),
                cwd: project
                    .roots
                    .first()
                    .map(|root| root.path.clone())
                    .unwrap_or_default(),
            }),
            menu: crate::projects::menu(
                project,
                &sections,
                &source.scope,
                desktop && source.scope.host == "local",
            ),
            ..Row::new(
                project.name.clone(),
                Action::Expand(project.id.clone()),
                Some(source.scope.clone()),
            )
        });
        if project.expanded {
            rows.extend(project_children(project, &source.scope, more_title));
        }
    }
    if list.projects.is_empty() {
        let (title, action) = if list.loading_projects {
            (
                tr("プロジェクトを読み込み中…"),
                Action::Loading("projects".into()),
            )
        } else {
            (
                tr("登録されたプロジェクトはありません"),
                Action::Notice("projects".into()),
            )
        };
        rows.push(Row::new(title.into(), action, None));
    }
    if list.has_more_projects {
        rows.push(Row {
            loading: list.loading_projects,
            enabled: !list.loading_projects && !list.loading_chats,
            ..Row::new(
                tr("もっとプロジェクトを表示").into(),
                Action::Projects,
                None,
            )
        });
    }
    if !desktop || !list.threads.is_empty() {
        rows.push(Row::new(tr("チャット").into(), Action::Heading(None), None));
    }
    for thread in &list.threads {
        if let Some(source) = sources
            .iter()
            .find(|source| source.scope.host == thread.host_id)
        {
            rows.push(thread_row(thread, 0, None, &source.scope));
        }
    }
    if list.threads.is_empty() && !list.loading_chats && !desktop {
        rows.push(Row::new(
            tr("チャットがありません").into(),
            Action::Notice("chats".into()),
            None,
        ));
    }
    if list.has_more {
        rows.push(Row {
            loading: list.loading_chats,
            enabled: !list.loading_chats && !list.loading_projects,
            ..Row::new(more_title.into(), Action::More(None), None)
        });
    }
    rows
}

fn project_children(
    project: &agent_core::presentation::list::ProjectSummary,
    scope: &ListScope,
    more_title: &str,
) -> Vec<Row> {
    let mut rows = Vec::new();
    if project.loading && !project.has_more {
        rows.push(Row::new(
            tr("読み込み中…").into(),
            Action::Loading(project.id.clone()),
            Some(scope.clone()),
        ));
    }
    if let Some(error) = &project.error {
        rows.push(Row::new(
            error.to_string(),
            Action::Notice(project.id.clone()),
            Some(scope.clone()),
        ));
        rows.push(Row::new(
            tr("再試行").into(),
            Action::Retry(project.id.clone()),
            Some(scope.clone()),
        ));
    }
    rows.extend(
        project
            .threads
            .iter()
            .map(|thread| thread_row(thread, 0, Some(&project.id), scope)),
    );
    if project.has_more {
        rows.push(Row {
            loading: project.loading,
            enabled: !project.loading,
            ..Row::new(
                more_title.into(),
                Action::More(Some(project.id.clone())),
                Some(scope.clone()),
            )
        });
    }
    for row in &mut rows {
        row.level = 1;
    }
    rows
}

pub(crate) fn list(query: Signal<String>, open: impl Fn() + Copy + 'static) -> AnyElement {
    let connection = use_context::<Connection>().unwrap();
    let sources = computed(move || connection.state.with(|state| state.list_sources()));
    let inputs = computed(move || connection.state.with(|state| state.list_inputs()));
    let list = computed(move || {
        inputs.with(|inputs| {
            agent_core::presentation::connection_list::combine_connection_lists(
                inputs
                    .iter()
                    .map(|(host, value)| (host.clone(), (**value).clone())),
            )
            .map(|list| Shared(std::sync::Arc::new(list)))
        })
    });
    let projects =
        crate::projects::Projects::new(connection.application, connection.host, sources, list);
    effect(move || {
        let search_term = query.get();
        sources.with(|_| ());
        connection
            .application
            .with(|application| application.search_list(&search_term));
    });
    let entries = computed(move || {
        let host = connection.host.get();
        list.with(|list| rows(list.as_deref(), dark(), &sources.get(), &host, is_desktop()))
    });
    let activate = move |name: &str, id: &str, expanded: Option<bool>| {
        if name == "menu" {
            match copied(id) {
                Some(value) => set_clipboard(value),
                None => projects.select(id),
            }
            return;
        }
        let Some((action, was_expanded, scope)) = entries.with_untracked(|rows| {
            rows.iter().filter(|row| row.enabled).find_map(|row| {
                let action = if name == "compose" {
                    row.compose.as_ref()
                } else {
                    Some(&row.action)
                }?;
                (row_id(row.scope.as_ref(), action) == id)
                    .then(|| (action.clone(), row.expanded, row.scope.clone()))
            })
        }) else {
            return;
        };
        let global = match &action {
            Action::Projects => Some(ListRequest::MoreProjects),
            Action::More(None) => Some(ListRequest::MoreChats),
            _ => None,
        };
        if let Some(request) = global {
            connection
                .application
                .with(|application| application.request_list(request));
            return;
        }
        let Some(scope) = scope.as_ref() else {
            return;
        };
        let navigate = matches!(&action, Action::Open { .. } | Action::New { .. });
        let intent = match action {
            Action::Open { thread, .. } => Intent::ReadThread(op::ReadThread::open(thread)),
            Action::Expand(id) => Intent::SetProjectExpanded {
                project_id: id,
                expanded: expanded.unwrap_or(!was_expanded),
            },
            Action::New { cwd, .. } => Intent::NewChat { cwd },
            Action::More(project_id) => Intent::ExpandThreadList { project_id },
            Action::Retry(project_id) => Intent::RefreshProject { project_id },
            Action::Projects
            | Action::Heading(_)
            | Action::ProjectHeader
            | Action::Notice(_)
            | Action::Loading(_) => return,
        };
        let accepted = connection
            .application
            .with(|application| application.list_action(scope, intent, navigate));
        if navigate && accepted {
            open();
        }
    };
    if is_desktop() {
        let selected = computed(move || {
            let scope = ListScope {
                host: connection.host.get(),
                storage: connection.scope.with(|scope| scope.storage.clone()),
            };
            let thread = connection
                .composer
                .with(|c| c.controls.navigation.thread_id.clone());
            entries.with(|rows| selected_rows(rows, &scope, thread.as_ref()))
        });
        return view()
            .flex_1()
            .min_h_0()
            .child(desktop_rows(entries, sources, selected, activate))
            .child(projects.view())
            .into_element();
    }
    let table = native("bex.details")
        .prop("data", move || {
            let menu = list.with(|list| {
                crate::projects::header_menu(
                    list.as_ref()
                        .map_or(&[], |list| list.hidden_projects.as_slice()),
                    false,
                    &connection.host.get(),
                    &sources.get(),
                )
            });
            entries.with(|entries| task_list_data(entries, &sources.get(), menu))
        })
        .on_native_event(move |event: &NativeEvent| {
            let name = if &*event.name == "action" {
                event.data["action"].as_str().unwrap_or_default()
            } else {
                &*event.name
            };
            let Some(id) = event.data.as_str().or_else(|| event.data["id"].as_str()) else {
                return;
            };
            if matches!(name, "menu" | "select" | "expand" | "compose") {
                let expanded = event
                    .data
                    .get("expanded")
                    .and_then(|value| value.as_bool().or_else(|| value.as_str()?.parse().ok()));
                activate(name, id, expanded);
            }
        })
        .flex_1();
    view()
        .flex_1()
        .child(table)
        .child(projects.view())
        .into_element()
}

/// The desktop sidebar's rows in the existing desktop client's layout. The project
/// header's menu supplies folder addition and the hidden projects' restore rows.
fn desktop_rows(
    entries: Memo<Vec<Row>>,
    sources: Memo<Vec<ListSource>>,
    selected: Memo<Vec<String>>,
    activate: impl Fn(&str, &str, Option<bool>) + Copy + 'static,
) -> impl IntoElement {
    let restores = computed(move || {
        entries.with(|rows| {
            rows.iter()
                .filter(|row| matches!(row.action, Action::ProjectHeader))
                .flat_map(|row| row.menu.as_array().cloned().unwrap_or_default())
                .filter_map(|item| item.get("children").and_then(|c| c.as_array()).cloned())
                .flatten()
                .collect::<Vec<_>>()
        })
    });
    scroll().flex_1().child(
        view()
            .px_3()
            .py_3()
            .child(each(
                move || entries.get(),
                |row: &Row| row_id(row.scope.as_ref(), &row.action),
                move |row, position| {
                    // Items are 4 pt apart, as the desktop client's; a section's
                    // title sits flush against what it heads.
                    let after_section = move || {
                        let at = position.get();
                        at > 0
                            && entries.with(|rows| {
                                rows.get(at - 1).is_some_and(|row| {
                                    matches!(
                                        row.action,
                                        Action::ProjectHeader | Action::Heading(None)
                                    )
                                })
                            })
                    };
                    view()
                        .mt(move || {
                            let spaced = row.with(|row| {
                                !matches!(row.action, Action::ProjectHeader | Action::Heading(None))
                            });
                            px(if spaced && position.get() > 0 && !after_section() {
                                4.0
                            } else {
                                0.0
                            })
                        })
                        .child(dynamic(move || {
                            desktop_row(row.get(), sources, selected, activate)
                        }))
                },
            ))
            .child(each(
                move || restores.get(),
                |item: &serde_json::Value| item["id"].as_str().unwrap_or_default().to_owned(),
                move |item, _| {
                    let item = item.get_untracked();
                    let id = item["id"].as_str().unwrap_or_default().to_owned();
                    let title = tr_format(
                        "一覧に戻す: {0}",
                        [&item["title"].as_str().unwrap_or_default()],
                    );
                    menu_row()
                        .on_press(move || activate("menu", &id, None))
                        .mt_1()
                        .child(row_label(title))
                        .into_element()
                },
            )),
    )
}

fn desktop_row(
    row: Row,
    sources: Memo<Vec<ListSource>>,
    selected: Memo<Vec<String>>,
    activate: impl Fn(&str, &str, Option<bool>) + Copy + 'static,
) -> AnyElement {
    use crate::ui::{self, Enabled, Icon, colors};
    let id = row_id(row.scope.as_ref(), &row.action);
    match &row.action {
        Action::ProjectHeader => {
            let add = row
                .menu
                .as_array()
                .and_then(|items| items.iter().find(|item| item["id"] == "add"))
                .map(|item| item["enabled"].as_bool().unwrap_or(false));
            section(row.title.clone())
                .child(add.map(|enabled| {
                    ui::icon_button(
                        Icon::FOLDER_PLUS,
                        tr("プロジェクトを追加"),
                        move || activate("menu", "add", None),
                    )
                    .enabled(move || enabled)
                }))
                .into_element()
        }
        Action::Heading(None) => section(row.title.clone()).into_element(),
        Action::Heading(Some(_)) => text(row.title.clone())
            .px_3()
            .pt_3()
            .pb_1()
            .text_size(px(12.0))
            .text_color(colors::muted())
            .into_element(),
        Action::Notice(_) => text(row.title.clone())
            .px_3()
            .py_2()
            .text_size(px(14.0))
            .into_element(),
        Action::Expand(_) => {
            let compose = row.compose.as_ref().map(|action| {
                let compose_id = row_id(row.scope.as_ref(), action);
                let scope = row.scope.clone();
                let connected = computed(move || {
                    sources.with(|sources| {
                        sources
                            .iter()
                            .any(|source| Some(&source.scope) == scope.as_ref() && source.connected)
                    })
                });
                ui::small_icon_button(
                    Icon::NEW_CHAT,
                    tr("このプロジェクトで新しいチャット"),
                    move || activate("compose", &compose_id, None),
                )
                .enabled(move || connected.get())
            });
            let menu = row.menu.clone();
            let toggle = id.clone();
            view()
                .group()
                .flex_row()
                .items_center()
                .gap_1()
                .child(
                    nsh_base::button()
                        .accessibility_label(Some(SharedString::from(row.title.clone())))
                        .flex_1()
                        .min_w_0()
                        .h(px(44.0))
                        .px_2()
                        .gap_2()
                        .flex_row()
                        .items_center()
                        .rounded(px(ui::RADIUS))
                        .text_color(colors::foreground())
                        .hover(|s| s.bg(colors::GHOST_HOVER))
                        .active(|s| s.bg(colors::SELECTED))
                        .on_press(move || activate("expand", &toggle, None))
                        .child(project_icon(row.png.as_deref(), row.glyph.clone(), 16.0))
                        .child(row_label(row.title.clone())),
                )
                .child(
                    view()
                        .flex_row()
                        .gap_1()
                        .opacity(0.0)
                        .group_hover(|s| s.opacity(1.0))
                        .child(ui::small_icon_button(
                            Icon::ELLIPSIS,
                            tr("プロジェクトのメニュー"),
                            move || {
                                ui::popup(&menu, move |command| activate("menu", command, None))
                            },
                        ))
                        .child(compose),
                )
                .into_element()
        }
        _ => {
            let enabled = row.enabled;
            let selected_id = id.clone();
            let active = move || selected.with(|ids| ids.contains(&selected_id));
            let menu = row.menu.clone();
            let item = menu_row()
                .style_when(active, |s| {
                    s.bg(colors::SELECTED)
                        .text_color(colors::foreground())
                        .font_weight(FontWeight::MEDIUM)
                })
                .child((row.level > 0).then(|| view().w(px(16.0)).h(px(16.0))))
                .child(row_label(row.title.clone()))
                .child(row.loading.then(ui::spinner))
                .child(row.indicators.clone().map(indicators))
                .on_press(move || activate("select", &id, None))
                .enabled(move || enabled);
            // A secondary press opens the row's menu: a conversation's Copy.
            view()
                .on_context_menu(move |_: &ContextMenuEvent| {
                    if !menu.is_null() {
                        ui::popup(&menu, move |command| activate("menu", command, None));
                    }
                })
                .child(item)
                .into_element()
        }
    }
}

/// A sidebar section's title row: "プロジェクト", "チャット".
fn section(title: String) -> View {
    view().flex_row().items_center().h(px(32.0)).pl_2().child(
        text(title)
            .flex_1()
            .text_size(px(12.0))
            .text_color(crate::ui::colors::muted()),
    )
}

/// A sidebar menu item: 28 pt, a fill under the pointer.
fn menu_row() -> nsh_base::ButtonElement {
    use crate::ui::{self, colors};
    nsh_base::button()
        .flex_row()
        .items_center()
        .gap_2()
        .h(px(28.0))
        .px_2()
        .rounded(px(ui::RADIUS))
        .text_color(colors::foreground())
        .hover(|s| s.bg(colors::ROW_HOVER))
        .disabled_style(|s| s.opacity(0.5))
}

fn row_label(title: String) -> impl IntoElement {
    text(title.clone())
        .flex_1()
        .min_w_0()
        .text_size(px(14.0))
        .number_of_lines(1)
        .label(title)
}

/// A project's own icon, else its monogram on its colour, `size` points square.
pub(crate) fn project_icon(
    png: Option<&[u8]>,
    glyph: Option<(String, u32)>,
    size: f32,
) -> AnyElement {
    use base64::Engine;
    if let Some(png) = png {
        let source = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png)
        );
        // Named as an image, so assistive technologies and the acceptance
        // checks tell the project's own icon from the monogram.
        return image(source)
            .w(px(size))
            .h(px(size))
            .flex_shrink_0()
            .object_fit(ResizeMode::Contain)
            .rounded(px(4.0))
            .role(AccessibilityRole::Image)
            .label(tr("プロジェクトのアイコン"))
            .into_element();
    }
    let (monogram, color) = glyph.unwrap_or_default();
    let color = nsh_core::element::rgb(color);
    view()
        .w(px(size))
        .h(px(size))
        .flex_shrink_0()
        .rounded(px(4.0))
        .items_center()
        .justify_center()
        .bg(color.opacity(0.15))
        .child(
            text(monogram)
                .text_size(px(size / 2.0))
                .font_weight(FontWeight::BOLD)
                .text_color(color),
        )
        .into_element()
}

/// T3 Code's list badge: what the conversation holds for its user.
fn attention_badge(
    attention: agent_core::presentation::thread_status::ThreadAttention,
) -> impl IntoElement {
    use crate::ui::{self, Icon};
    use agent_core::presentation::thread_status::ThreadAttention;
    let (icon, color) = match attention {
        ThreadAttention::Approval => (Icon::STATUS_APPROVAL, 0xfbbf24),
        ThreadAttention::Input => (Icon::QUESTION, 0x60a5fa),
        ThreadAttention::Failed => (Icon::ALERT, 0xf87171),
    };
    let title =
        localized(agent_core::presentation::thread_status::thread_attention_label(attention));
    view()
        .flex_row()
        .items_center()
        .gap_1()
        .text_color(nsh_core::element::rgb(color))
        .child(ui::icon(icon, 14.0))
        .child(
            text(title)
                .text_size(px(12.0))
                .font_weight(FontWeight::MEDIUM),
        )
}

/// A chat's state after its title: what it holds for, running, waiting, unread,
/// a worktree to merge or merged.
fn indicators(indicators: native::RowIndicators) -> impl IntoElement {
    use crate::ui::{self, Icon};
    use agent_core::presentation::thread_status::ThreadActivity;
    let label = indicators.label();
    let badged = indicators.attention.is_some();
    view()
        .flex_row()
        .items_center()
        .gap_2()
        .label(label)
        .child(indicators.attention.map(attention_badge))
        .child(match indicators.activity {
            // A badge says what a waiting chat waits for.
            Some(ThreadActivity::Waiting) if badged => None,
            // T3 Code's Working, Waiting and Done icons, in its status colours.
            Some(ThreadActivity::Running | ThreadActivity::Waiting) => Some(
                view()
                    .text_color(nsh_core::element::rgb(0x60a5fa))
                    .child(ui::icon(Icon::STATUS_WORKING, 16.0)),
            ),
            Some(ThreadActivity::Background) => Some(
                view()
                    .text_color(crate::ui::colors::muted())
                    .child(ui::icon(Icon::STATUS_WAITING, 16.0)),
            ),
            Some(ThreadActivity::Unread) => Some(
                view()
                    .text_color(nsh_core::element::rgb(0x34d399))
                    .child(ui::icon(Icon::STATUS_DONE, 16.0)),
            ),
            None => None,
        })
        .child(indicators.changes.then(|| {
            view()
                .text_color(nsh_core::element::rgb(0xfb923c))
                .child(ui::icon(Icon::DIFF, 16.0))
        }))
}

fn native_rows(entries: &[Row], sources: &[ListSource]) -> serde_json::Value {
    json!(entries.iter().map(|row| json!({"id": row_id(row.scope.as_ref(), &row.action), "title": row.title, "png": row.png, "monogram":row.glyph.as_ref().map(|glyph| &glyph.0), "color":row.glyph.as_ref().map(|glyph| glyph.1), "level": row.level, "disclosure": row.disclosure, "group": matches!(&row.action, Action::Heading(_) | Action::ProjectHeader), "expanded": row.expanded, "menu": row.menu, "indicators": row.indicators, "loading":row.loading,"enabled":row.enabled,"passive":matches!(row.action,Action::Notice(_) | Action::Loading(_)), "statusLabel": row.indicators.as_ref().map(|data| data.label()), "compose": row.compose.as_ref().map(|action| json!({"id": row_id(row.scope.as_ref(), action), "enabled": sources.iter().any(|source| Some(&source.scope) == row.scope.as_ref() && source.connected)}))})).collect::<Vec<_>>())
}

fn task_list_data(
    entries: &[Row],
    sources: &[ListSource],
    menu: serde_json::Value,
) -> serde_json::Value {
    json!({"taskList":{"data":{"rows":native_rows(entries, sources),"menu":menu}}})
}

fn selected_rows(
    rows: &[Row],
    scope: &ListScope,
    thread: Option<&agent_protocol::session::SessionRef>,
) -> Vec<String> {
    rows.iter()
        .filter(|row| match &row.action {
            Action::Open {
                thread: candidate, ..
            } => row.scope.as_ref() == Some(scope) && thread == Some(candidate),
            _ => false,
        })
        .map(|row| row_id(row.scope.as_ref(), &row.action))
        .collect()
}

fn row_id(scope: Option<&ListScope>, action: &Action) -> String {
    serde_json::to_string(&(scope, action)).expect("native row actions serialize")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ConnectionList;

    impl Component for ConnectionList {
        type Props = ();
        type Event = NoEvent;

        fn view(_: (), _: Scope<NoEvent>) -> impl IntoElement {
            let connected = signal(false);
            let sources = computed(move || {
                vec![ListSource {
                    scope: scope("local", "scope"),
                    connected: connected.get(),
                }]
            });
            let entries = computed(move || {
                let mut list = fixture_list();
                list.projects[0].icon = None;
                list.projects[0].roots = vec![agent_protocol::models::ProjectRoot {
                    path: "/project".into(),
                }];
                fixture_rows(Some(&list), false, "local", "scope", true, connected.get())
            });
            view()
                .child(
                    nsh_base::button()
                        .accessibility_label("Toggle connection")
                        .on_press(move || connected.update(|value| *value = !*value)),
                )
                .child(desktop_rows(
                    entries,
                    sources,
                    computed(Vec::new),
                    |_, _, _| {},
                ))
        }
    }

    #[test]
    fn desktop_new_chat_and_add_project_follow_connection_without_reopening_projects() {
        use nsh_core::{ApproxTextEngine, Core, EventKind, EventPayload, MountItem, element::Root};
        let mut core = Core::new(
            ApproxTextEngine,
            nsh_core::geometry::size(px(900.0), px(700.0)),
        );
        core.set_components(nsh_base::registry());
        core.mount_root(&Root::new::<ConnectionList>(|| ()));
        let mut buttons = std::collections::BTreeMap::new();
        for connected in [false, true, false, true] {
            let batch = core.commit().unwrap();
            for item in batch.items {
                match item {
                    MountItem::ViewProps { id, props } => {
                        buttons.insert(id, props.accessibility);
                    }
                    MountItem::Delete { id } => {
                        buttons.remove(&id);
                    }
                    _ => {}
                }
            }
            for label in [
                tr("このプロジェクトで新しいチャット"),
                tr("プロジェクトを追加"),
            ] {
                let button = buttons
                    .values()
                    .find(|value| value.label.as_deref() == Some(label))
                    .unwrap();
                assert_eq!(button.state.disabled, !connected, "{label}");
            }
            let toggle = buttons
                .iter()
                .find(|(_, value)| value.label.as_deref() == Some("Toggle connection"))
                .unwrap()
                .0;
            assert!(core.component_event(*toggle, EventKind::Press, &EventPayload::None));
        }
    }

    fn scope(host: &str, storage: &str) -> ListScope {
        ListScope {
            host: host.into(),
            storage: storage.into(),
        }
    }

    fn fixture_rows(
        list: Option<&ThreadList>,
        dark: bool,
        host: &str,
        storage: &str,
        desktop: bool,
        connected: bool,
    ) -> Vec<Row> {
        let list = agent_core::presentation::connection_list::combine_connection_lists(
            list.into_iter().map(|list| (host.into(), list.clone())),
        );
        super::rows(
            list.as_ref(),
            dark,
            &[ListSource {
                scope: scope(host, storage),
                connected,
            }],
            host,
            desktop,
        )
    }

    #[test]
    fn desktop_header_keeps_folder_addition_local_and_available_without_a_loaded_list() {
        for (host, connected, enabled) in [
            ("local", false, false),
            ("local", true, true),
            ("remote", true, false),
        ] {
            let rows = fixture_rows(None, false, host, "scope", true, connected);
            assert_eq!(rows.len(), 1);
            assert!(matches!(rows[0].action, Action::ProjectHeader));
            assert_eq!(rows[0].menu[0]["id"], "add");
            assert_eq!(rows[0].menu[0]["enabled"], enabled);
        }
        assert!(fixture_rows(None, false, "local", "scope", false, true).is_empty());
    }

    fn fixture_list() -> ThreadList {
        let thread = ThreadSummary {
            attention: None,
            host_id: "host".into(),
            id: serde_json::from_value(json!({"provider": "codex", "id": "chat"})).unwrap(),
            title: "選択中の会話".into(),
            project_id: Some("project".into()),
            active: false,
            waiting: false,
            background: false,
            unread: false,
            has_changes: None,
            copy_items: Vec::new(),
        };
        ThreadList {
            loading_projects: false,
            loading_chats: false,
            notice: None,
            threads: Vec::new(),
            projects: vec![agent_core::presentation::list::ProjectSummary {
                host_id: "host".into(),
                id: "project".into(),
                name: "プロジェクト".into(),
                coordinator_url: None,
                roots: Vec::new(),
                icon: Some(agent_core::presentation::list::ProjectIconImages {
                    light_png: vec![1, 2],
                    dark_png: vec![3, 4],
                }),
                monogram: "PR".into(),
                icon_color: 0,
                pinned: false,
                section: None,
                chats_archived: false,
                expanded: false,
                loading: false,
                error: None,
                threads: vec![thread.clone()],
                has_more: false,
            }],
            hidden_projects: Vec::new(),
            has_more: false,
            has_more_projects: false,
        }
    }

    #[test]
    fn project_icons_follow_the_appearance_the_list_is_drawn_in() {
        let list = fixture_list();
        let icon = |dark, desktop| {
            fixture_rows(Some(&list), dark, "host", "scope", desktop, true)
                .into_iter()
                .find(|row| row.disclosure)
                .and_then(|row| row.png)
        };
        assert_eq!(icon(false, false), Some(vec![1, 2]));
        assert_eq!(icon(true, false), Some(vec![3, 4]));
        // The desktop sidebar is dark in a light system appearance too.
        assert_eq!(icon(false, true), Some(vec![3, 4]));
        assert_eq!(icon(true, true), Some(vec![3, 4]));
        let mut plain = list.clone();
        plain.projects[0].icon = None;
        let row = fixture_rows(Some(&plain), false, "host", "scope", true, true)
            .into_iter()
            .find(|row| row.disclosure)
            .unwrap();
        assert_eq!(row.png, None);
        assert_eq!(row.glyph, Some(("PR".into(), 0)));
    }

    #[test]
    fn native_item_identity_is_stable_and_unique_across_projects() {
        let mut list = fixture_list();
        let thread = list.projects[0].threads[0].clone();
        let collapsed = fixture_rows(Some(&list), false, "host", "scope", false, true);
        assert_eq!(collapsed.len(), 3);
        assert_eq!(collapsed[0].glyph, Some(("PR".into(), 0)));
        assert_eq!(collapsed[0].png, Some(vec![1, 2]));
        let dark = fixture_rows(Some(&list), true, "host", "scope", false, true);
        assert_eq!(dark[0].png, Some(vec![3, 4]));
        assert_eq!(
            row_id(collapsed[0].scope.as_ref(), &collapsed[0].action),
            row_id(dark[0].scope.as_ref(), &dark[0].action)
        );
        list.projects[0].expanded = true;
        let expanded = fixture_rows(Some(&list), false, "host", "scope", false, true);
        assert_eq!(
            row_id(collapsed[0].scope.as_ref(), &collapsed[0].action),
            row_id(expanded[0].scope.as_ref(), &expanded[0].action)
        );
        assert!(expanded[0].expanded);
        let selected = row_id(expanded[1].scope.as_ref(), &expanded[1].action);
        let mut new_thread = thread;
        new_thread.id = serde_json::from_value(json!({"provider": "codex", "id": "new"})).unwrap();
        list.projects[0].threads.insert(0, new_thread);
        let inserted = fixture_rows(Some(&list), false, "host", "scope", false, true);
        assert_eq!(
            selected,
            row_id(inserted[2].scope.as_ref(), &inserted[2].action)
        );
        assert_ne!(
            selected,
            row_id(inserted[1].scope.as_ref(), &inserted[1].action)
        );

        list.threads.push(list.projects[0].threads[1].clone());
        let placed = fixture_rows(Some(&list), false, "host", "scope", false, true);
        let ids = placed
            .iter()
            .map(|row| row_id(row.scope.as_ref(), &row.action))
            .collect::<Vec<_>>();
        assert_eq!(
            ids.len(),
            ids.iter().collect::<std::collections::HashSet<_>>().len()
        );
        let candidates = selected_rows(&placed, &scope("host", "scope"), Some(&list.threads[0].id));
        assert_eq!(candidates.len(), 2);
        assert_ne!(candidates[0], candidates[1]);

        let desktop = fixture_rows(Some(&list), false, "host", "scope", true, true);
        assert!(matches!(desktop[0].action, Action::ProjectHeader));
        assert!(!desktop[0].disclosure);
        assert_eq!(desktop[0].level, 0);
        assert_eq!(desktop[1].level, 0);
        assert_eq!(desktop[2].level, 1);
        let recent_heading = desktop
            .iter()
            .position(|row| row.action == Action::Heading(None))
            .unwrap();
        assert_eq!(desktop[recent_heading].title, "チャット");
        assert_eq!(desktop[recent_heading].level, 0);
        assert!(!desktop[recent_heading].disclosure);
        assert_eq!(desktop[recent_heading + 1].level, 0);
        assert_eq!(desktop[recent_heading + 1].title, list.threads[0].title);
        assert_eq!(
            desktop
                .iter()
                .map(|row| row_id(row.scope.as_ref(), &row.action))
                .collect::<std::collections::HashSet<_>>()
                .len(),
            desktop.len()
        );
        assert!(placed.iter().any(|row| row.action == Action::Heading(None)));
        list.has_more_projects = true;
        let paged = fixture_rows(Some(&list), false, "host", "scope", true, true);
        assert!(
            paged
                .iter()
                .position(|row| row.action == Action::Projects)
                .unwrap()
                < paged
                    .iter()
                    .position(|row| row.action == Action::Heading(None))
                    .unwrap()
        );
        list.has_more_projects = false;
        list.threads.clear();
        assert!(
            !fixture_rows(Some(&list), false, "host", "scope", true, true)
                .iter()
                .any(|row| row.action == Action::Heading(None))
        );

        // Two provider projects can refer to the same Host folder. They must
        // remain distinct native items even though both start a chat there.
        list.projects[0].threads.clear();
        list.projects[0]
            .roots
            .push(agent_protocol::models::ProjectRoot {
                path: "/project".into(),
            });
        let mut other = list.projects[0].clone();
        other.id = "other-provider-project".into();
        list.projects.push(other);
        let ids = fixture_rows(Some(&list), false, "host", "scope", false, true)
            .iter()
            .map(|row| row_id(row.scope.as_ref(), &row.action))
            .collect::<Vec<_>>();
        assert_eq!(ids.len(), 4);
        assert_eq!(
            ids.iter().collect::<std::collections::HashSet<_>>().len(),
            4
        );
        list.projects[0].has_more = true;
        let open_thread = agent_protocol::session::SessionRef::new(
            agent_protocol::session::ProviderKind::Codex,
            "chat".into(),
        )
        .unwrap();
        for (host, desktop, folder_enabled) in [
            ("local", true, true),
            ("remote", true, false),
            ("local", false, false),
            ("remote", false, false),
        ] {
            let rendered = fixture_rows(Some(&list), false, host, "scope", desktop, true);
            let project = rendered
                .iter()
                .find(|row| row.action == Action::Expand("project".into()))
                .unwrap();
            let folder = project
                .menu
                .as_array()
                .unwrap()
                .iter()
                .find(|item| {
                    item["title"]
                        == if cfg!(windows) {
                            "エクスプローラーで開く"
                        } else {
                            "フォルダを開く"
                        }
                })
                .unwrap();
            assert_eq!(folder["enabled"], folder_enabled);
            let more = rendered
                .iter()
                .find(|row| row.action == Action::More(Some("project".into())))
                .unwrap();
            assert_eq!(more.level, 1);
            let identities = rendered
                .iter()
                .filter_map(|row| {
                    row.compose
                        .as_ref()
                        .map(|action| row_id(row.scope.as_ref(), action))
                })
                .collect::<Vec<_>>();
            assert_eq!(identities.len(), 2);
            assert_eq!(
                identities
                    .iter()
                    .collect::<std::collections::HashSet<_>>()
                    .len(),
                2
            );
            assert!(
                matches!(&project.compose, Some(Action::New {project, cwd}) if project == "project" && cwd == "/project")
            );
            assert!(
                !rendered
                    .iter()
                    .any(|row| matches!(&row.action, Action::New { .. }))
            );
            assert!(selected_rows(&rendered, &scope(host, "scope"), None).is_empty());
            assert!(selected_rows(&rendered, &scope(host, "scope"), Some(&open_thread)).is_empty());
        }
    }

    #[test]
    fn conversation_row_preserves_activity_and_changes_independently() {
        use agent_core::presentation::thread_status::ThreadActivity;
        let mut thread = ThreadSummary {
            attention: None,
            host_id: "host".into(),
            id: serde_json::from_value(json!({"provider": "codex", "id": "chat"})).unwrap(),
            title: "会話".into(),
            project_id: None,
            active: true,
            waiting: true,
            background: false,
            unread: true,
            has_changes: Some(true),
            copy_items: Vec::new(),
        };
        let row = thread_row(&thread, 1, Some("project"), &scope("host", "scope"));
        let indicators = row.indicators.unwrap();
        assert_eq!(indicators.activity, Some(ThreadActivity::Waiting));
        assert!(indicators.changes);
        let encoded = serde_json::to_value(&indicators).unwrap();
        assert_eq!(encoded, json!({"activity": "waiting", "changes": true}));
        thread.active = false;
        thread.waiting = false;
        thread.unread = false;
        assert!(
            thread_row(&thread, 0, None, &scope("host", "scope"))
                .indicators
                .unwrap()
                .changes
        );
        thread.has_changes = Some(false);
        assert!(
            thread_row(&thread, 0, None, &scope("host", "scope"))
                .indicators
                .is_none()
        );
    }

    #[test]
    fn a_conversations_copy_menu_copies_what_it_has_and_disables_the_rest() {
        let menu = copy_menu(&[
            ThreadCopyItem {
                label: "パス".into(),
                value: Some("/work/日本語 👋".into()),
            },
            ThreadCopyItem {
                label: "ブランチ".into(),
                value: None,
            },
        ]);
        let items = menu[0]["children"].as_array().unwrap();
        assert_eq!(menu[0]["title"], "コピー");
        assert_eq!(
            copied(items[0]["id"].as_str().unwrap()).as_deref(),
            Some("/work/日本語 👋")
        );
        assert_eq!(items[1]["enabled"], false);
        assert_eq!(copied(items[1]["id"].as_str().unwrap()), None);
        assert_eq!(
            copied(r#"{"host":"h","action":"Hide"}"#),
            None,
            "project commands stay theirs"
        );
        assert!(copy_menu(&[]).is_null());
    }

    #[test]
    fn native_task_list_contract_keeps_hierarchy_scoped_actions_and_status_labels() {
        let owner = scope("host", "scope");
        let mut project = Row::new(
            "日本語 👋".into(),
            Action::Expand("project".into()),
            Some(owner.clone()),
        );
        project.menu = json!([]);
        project.glyph = Some(("日".into(), 0x123456));
        project.disclosure = true;
        project.expanded = true;
        project.compose = Some(Action::New {
            project: "project".into(),
            cwd: "/project".into(),
        });
        let thread = ThreadSummary {
            attention: None,
            host_id: "host".into(),
            id: serde_json::from_value(json!({"provider":"codex","id":"chat"})).unwrap(),
            title: "会話 👋".into(),
            project_id: None,
            active: true,
            waiting: true,
            background: false,
            unread: false,
            has_changes: Some(true),
            copy_items: Vec::new(),
        };
        let entries = [
            project,
            thread_row(&thread, 1, Some("project"), &owner),
            Row::new("チャット".into(), Action::Heading(None), None),
            thread_row(&thread, 0, None, &owner),
        ];
        let menu = json!([{"title":"非表示のプロジェクトを戻す","children":[{"id":"restore 👋","title":"日本語","enabled":true}]}]);
        let actual = task_list_data(
            &entries,
            &[ListSource {
                scope: owner,
                connected: false,
            }],
            menu,
        );
        let expected: serde_json::Value = serde_json::from_str(include_str!(
            "../apple/Tests/BexNshNativeTests/Fixtures/task-list.json"
        ))
        .unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn project_loading_errors_and_paging_recover_without_losing_the_owner() {
        let mut list = fixture_list();
        list.projects[0].expanded = true;
        list.projects[0].loading = true;
        list.projects[0].error = Some(agent_core::presentation::error::ErrorText {
            message: "一覧を取得できませんでした".into(),
            details: Some("fixture failure".into()),
        });
        for desktop in [false, true] {
            let rendered = fixture_rows(Some(&list), false, "host", "scope", desktop, true);
            let loading = rendered
                .iter()
                .find(|row| matches!(row.action, Action::Loading(_)))
                .unwrap();
            assert!(loading.loading);
            assert!(!loading.enabled);
            assert_eq!(loading.level, 1);
            let retry = rendered
                .iter()
                .find(|row| matches!(row.action, Action::Retry(_)))
                .unwrap();
            assert!(retry.enabled);
            assert_eq!(retry.scope, Some(scope("host", "scope")));
            assert_eq!(retry.level, 1);
            let error = rendered
                .iter()
                .find(|row| row.action == Action::Notice("project".into()))
                .unwrap();
            assert_eq!(error.title, "一覧を取得できませんでした\nfixture failure");
            assert!(!error.enabled);
        }
        list.projects[0].has_more = true;
        let paged = fixture_rows(Some(&list), false, "host", "scope", false, true);
        assert!(
            !paged
                .iter()
                .any(|row| matches!(row.action, Action::Loading(_)))
        );
        let more = paged
            .iter()
            .find(|row| row.action == Action::More(Some("project".into())))
            .unwrap();
        assert_eq!(more.title, "もっと見る");
        assert!(more.loading);
        assert!(!more.enabled);
        let identity = row_id(more.scope.as_ref(), &more.action);
        list.projects[0].loading = false;
        list.projects[0].error = None;
        let recovered = fixture_rows(Some(&list), false, "host", "scope", false, true);
        let more = recovered
            .iter()
            .find(|row| row_id(row.scope.as_ref(), &row.action) == identity)
            .unwrap();
        assert!(more.enabled);
        assert!(!more.loading);
        assert!(!recovered.iter().any(|row| matches!(row.action, Action::Retry(ref id) | Action::Notice(ref id) if id == "project")));

        list.has_more = true;
        list.has_more_projects = true;
        list.loading_chats = true;
        list.loading_projects = true;
        let loading = fixture_rows(Some(&list), false, "host", "scope", true, true);
        for action in [Action::Projects, Action::More(None)] {
            let row = loading.iter().find(|row| row.action == action).unwrap();
            assert!(row.loading);
            assert!(!row.enabled);
        }
    }

    #[test]
    fn identical_host_rows_and_menus_keep_their_owner_and_account_scope() {
        let mut list = fixture_list();
        list.projects[0].expanded = true;
        list.projects[0]
            .roots
            .push(agent_protocol::models::ProjectRoot {
                path: "/same".into(),
            });
        list.threads = list.projects[0].threads.clone();
        let mut hidden = list.projects[0].clone();
        hidden.id = "hidden".into();
        list.hidden_projects.push(hidden);
        let combined = agent_core::presentation::connection_list::combine_connection_lists([
            ("one".into(), list.clone()),
            ("two".into(), list),
        ])
        .unwrap();
        let sources = vec![
            ListSource {
                scope: scope("one", "first-account"),
                connected: true,
            },
            ListSource {
                scope: scope("two", "second-account"),
                connected: false,
            },
        ];
        let rendered = rows(Some(&combined), false, &sources, "one", true);
        let native = native_rows(&rendered, &sources);
        let ids: std::collections::HashSet<_> = rendered
            .iter()
            .map(|row| row_id(row.scope.as_ref(), &row.action))
            .collect();
        assert_eq!(ids.len(), rendered.len());
        let mut compose = std::collections::HashSet::new();
        for row in rendered.iter().filter(|row| row.disclosure) {
            let source = row.scope.as_ref().unwrap();
            compose.insert(row_id(Some(source), row.compose.as_ref().unwrap()));
            let item = native
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["id"] == row_id(Some(source), &row.action))
                .unwrap();
            assert_eq!(item["compose"]["enabled"], source.host == "one");
            for item in row
                .menu
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|item| item["id"].as_str())
            {
                let command: serde_json::Value = serde_json::from_str(item).unwrap();
                assert_eq!(command["host"], source.host);
                assert_eq!(command["storage"], source.storage);
            }
        }
        assert_eq!(compose.len(), 2);
        let selected_one =
            selected_rows(&rendered, &sources[0].scope, Some(&combined.threads[0].id));
        let selected_two =
            selected_rows(&rendered, &sources[1].scope, Some(&combined.threads[1].id));
        assert_eq!(selected_one.len(), 2);
        assert_eq!(selected_two.len(), 2);
        assert!(selected_one.iter().all(|id| !selected_two.contains(id)));
        let hidden = rendered[0].menu[1]["children"].as_array().unwrap();
        let owners: Vec<_> = hidden
            .iter()
            .map(|item| {
                let command: serde_json::Value =
                    serde_json::from_str(item["id"].as_str().unwrap()).unwrap();
                (
                    command["host"].as_str().unwrap().to_owned(),
                    command["storage"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        assert_eq!(
            owners,
            [
                ("one".into(), "first-account".into()),
                ("two".into(), "second-account".into())
            ]
        );
        let mut replaced = sources.clone();
        replaced[0].scope.storage = "new-account".into();
        let new_rows = rows(Some(&combined), false, &replaced, "one", true);
        let new_selection =
            selected_rows(&new_rows, &replaced[0].scope, Some(&combined.threads[0].id));
        assert!(selected_one.iter().all(|id| !new_selection.contains(id)));
        assert_eq!(
            selected_rows(&new_rows, &sources[1].scope, Some(&combined.threads[1].id)),
            selected_two
        );
    }
}

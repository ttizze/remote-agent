//! The desktop sidebar: the window's leading column in the existing desktop client's layout.
use crate::l10n::{tr, tr_format};
use crate::{
    Connection,
    application::{Host, ListRequest, Navigation},
    ui::{self, Enabled, HEADER_HEIGHT, Icon, colors},
};
use nsh_core::prelude::*;
use serde_json::json;

pub(crate) const WIDTH: f32 = 272.0;

/// The sidebar fills its native split pane or narrow-window sheet.
pub(crate) fn sidebar(hosts: Memo<Vec<Host>>, compose: impl Fn() + 'static) -> impl IntoElement {
    let connection = use_context::<Connection>().unwrap();
    let query = signal(String::new());
    let navigate = move |action| {
        connection
            .application
            .with(|application| application.navigate(action))
    };
    let connected = move || connection.settings.with(|p| p.connected());
    // Going somewhere from the sidebar closes its sheet, as T3 Code's.
    let leave = move || navigate(Navigation::LeftSidebar(window().width.0));
    view()
        .size_full()
        .bg(colors::SIDEBAR)
        .border_r_1()
        .border_r_color(colors::border())
        .child(header(true, move || {
            navigate(Navigation::ToggleSidebar(window().width.0))
        }))
        .child(
            view()
                .pt_3()
                .px_3()
                .gap_3()
                // The desktop client keeps the menu's place while one Host is listed.
                .child(view().child(when(
                    move || {
                        hosts.with(|hosts| hosts.len() > 1)
                            || connection.state.with(|state| state.all_hosts)
                    },
                    move || connection_menu(hosts),
                )))
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .child(ui::search(query, tr("会話を検索")).flex_1().min_w_0())
                        .child(
                            ui::icon_button(
                                Icon::NEW_CHAT,
                                tr("新しいチャット"),
                                move || {
                                    leave();
                                    compose();
                                },
                            )
                            .enabled(connected),
                        ),
                ),
        )
        .child(crate::threads::list(query, leave))
        .child(
            view()
                .flex_row()
                .items_center()
                .justify_between()
                .px_3()
                .pt_2()
                .pb(px(20.0))
                .child(ui::icon_button(
                    Icon::SETTINGS,
                    tr("設定を開く"),
                    move || {
                        leave();
                        navigate(Navigation::Settings(true));
                    },
                ))
                .child(ui::icon_button(
                    Icon::REFRESH,
                    tr("会話を更新"),
                    move || {
                        connection.application.with(|application| {
                            application.request_list(ListRequest::Refresh);
                        })
                    },
                )),
        )
}

/// The sidebar over the content in a narrow window, as T3 Code's sheet: from the
/// window's leading edge to 12 points short of its other, over a dimmed
/// backdrop that, as Escape, closes it.
pub(crate) fn sheet(
    hosts: Memo<Vec<Host>>,
    compose: impl Fn() + 'static,
    dismiss: impl Fn() + Copy + 'static,
) -> impl IntoElement {
    on_key("escape", move |_| dismiss());
    let width = move || px((window().width.0 - SHEET_INSET).max(0.0));
    view()
        .absolute()
        .top(px(0.0))
        .left(px(0.0))
        .right(px(0.0))
        .bottom(px(0.0))
        .flex_row()
        .child(view().w(width).flex_row().child(sidebar(hosts, compose)))
        // T3's backdrop: the content dimmed to 60% and blurred by 4 points
        // (`bg-background/60 backdrop-blur-xs`). The sheet hides the rest of
        // it, so it lies beside the sheet, after it for assistive technologies.
        .child(
            view()
                .absolute()
                .top(px(0.0))
                .left(width)
                .right(px(0.0))
                .bottom(px(0.0))
                .bg(rgba(0x08080899))
                .backdrop_blur(px(4.0))
                .role(AccessibilityRole::Button)
                .label(SharedString::from(tr("サイドバーを閉じる")))
                .on_press(dismiss),
        )
}

/// How far short of the window's other edge the sheet ends (T3's
/// `calc(100vw - var(--spacing(3)))`).
const SHEET_INSET: f32 = 12.0;

/// The title bar's leading part: the app icon after the window controls, and the
/// sidebar's toggle.
pub(crate) fn header(expanded: bool, toggle: impl Fn() + 'static) -> View {
    use base64::Engine;
    let logo = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(include_bytes!("../assets/icon.png"))
    );
    let (glyph, label) = if expanded {
        (Icon::PANEL_LEFT_CLOSE, tr("サイドバーを閉じる"))
    } else {
        (Icon::PANEL_LEFT_OPEN, tr("サイドバーを開く"))
    };
    view()
        .h(px(HEADER_HEIGHT))
        .flex_shrink_0()
        .flex_row()
        .items_center()
        .gap_2()
        // The Mac's window controls lead the title bar; Windows' end it.
        .pl(px(if cfg!(target_os = "macos") {
            88.0
        } else {
            12.0
        }))
        .pr_3()
        .window_drag(true)
        .child(
            image(logo)
                .w(px(22.0))
                .h(px(22.0))
                .rounded(px(5.0))
                .flex_shrink_0(),
        )
        .child(expanded.then(|| view().flex_1()))
        .child(
            view()
                .window_drag(false)
                .child(ui::icon_button(glyph, label, toggle)),
        )
}

/// The conversation's top bar, as T3 Code's (`ChatHeader`): its breadcrumb,
/// then its panel toggles at the window's end; the sidebar's header first
/// when the sidebar is closed.
pub(crate) fn conversation_header(
    sidebar: Memo<bool>,
    toggle_sidebar: impl Fn() + Copy + 'static,
    breadcrumb: impl IntoElement,
    toggles: impl IntoElement,
) -> impl IntoElement {
    view()
        // T3's `--workspace-topbar-height`.
        .h(px(HEADER_HEIGHT))
        .flex_shrink_0()
        .flex_row()
        .items_center()
        .gap_3()
        // T3's `--workspace-gutter` before the breadcrumb.
        .pl(px(20.0))
        .pr(ui::titlebar_end_inset)
        .window_drag(true)
        .style_when(move || !sidebar.get(), |s| s.pl_0())
        .child(when(
            move || !sidebar.get(),
            move || header(false, toggle_sidebar),
        ))
        .child(breadcrumb)
        .child(toggles)
}

/// T3 Code's panel layout controls: the thread details beside the timeline
/// (while there is room for them), then the terminal and the right panel,
/// which the panel's own top-right controls replace while it is open.
pub(crate) fn panel_toggles(
    details: Signal<bool>,
    details_inline: impl Fn() -> bool + Copy + 'static,
    panel_open: impl Fn() -> bool + Copy + 'static,
    open_panel: impl Fn() + Copy + 'static,
) -> impl IntoElement {
    let workbench = use_context::<crate::workbench::Workbench>().unwrap();
    view()
        .flex_row()
        .items_center()
        .gap_1()
        .window_drag(false)
        .child(when(details_inline, move || {
            ui::toggle(
                Icon::DETAILS,
                tr("スレッド詳細を切り替え"),
                move || details.get(),
                move || details.update(|open| *open = !*open),
            )
        }))
        .child(when(
            move || !panel_open(),
            move || {
                view()
                    .flex_row()
                    .gap_1()
                    .child(crate::workbench::terminal_toggle(workbench))
                    .child(ui::toggle(
                        Icon::PANEL_RIGHT,
                        tr("右パネルを切り替え"),
                        panel_open,
                        open_panel,
                    ))
            },
        ))
}

/// T3 Code's thread breadcrumb: the project, which starts a new chat in it,
/// a slash, then the chat's title.
pub(crate) fn breadcrumb(
    title: Memo<String>,
    project: Memo<Option<agent_core::presentation::list::ProjectSummary>>,
    compose: impl Fn() + Copy + 'static,
) -> impl IntoElement {
    // Each crumb in T3's `text-sm font-medium`, on one line.
    fn crumb(content: nsh_core::element::Text) -> nsh_core::element::Text {
        content
            .text_size(px(14.0))
            .line_height(px(20.0))
            .font_weight(FontWeight::MEDIUM)
            .number_of_lines(1)
    }
    view()
        .flex_1()
        .min_w_0()
        .flex_row()
        .items_center()
        .gap_3()
        .child(when(
            move || project.with(Option::is_some),
            move || {
                let name = move || {
                    project.with(|project| {
                        project
                            .as_ref()
                            .map(|project| project.name.clone())
                            .unwrap_or_default()
                    })
                };
                view().window_drag(false).flex_shrink(1.0).min_w_0().child(
                    ui::bare_button(
                        move || SharedString::from(tr_format("{0}で新しいチャット", [&name()])),
                        compose,
                    )
                    .min_w_0()
                    .gap(px(6.0))
                    .text_color(ui::colors::t3_muted())
                    .hover(|s| s.text_color(ui::colors::t3_foreground()))
                    .child(dynamic(move || {
                        project.with(|project| {
                            project.as_ref().map(|project| {
                                crate::threads::project_icon(
                                    project.icon.as_ref().map(|icon| icon.dark_png.as_slice()),
                                    Some((project.monogram.clone(), project.icon_color)),
                                    14.0,
                                )
                            })
                        })
                    }))
                    // T3's `max-w-40`.
                    .child(crumb(text(name)).min_w_0().max_w(px(160.0))),
                )
            },
        ))
        .child(when(
            move || project.with(Option::is_some),
            move || {
                view()
                    .flex_shrink_0()
                    .accessibility_elements_hidden(true)
                    .child(crumb(text("/")).text_color(ui::colors::t3_muted()))
            },
        ))
        .child(
            crumb(text(move || title.get()))
                .flex_1()
                .min_w_0()
                .text_color(ui::colors::t3_foreground()),
        )
}

/// The list's connection: all connections or one, chosen from a popup menu.
fn connection_menu(hosts: Memo<Vec<Host>>) -> impl IntoElement {
    let connection = use_context::<Connection>().unwrap();
    let all = move || connection.state.with(|state| state.all_hosts);
    let title = move || {
        if all() {
            return tr("すべての接続先").to_owned();
        }
        let selected = connection.host.get();
        hosts.with(|hosts| {
            hosts
                .iter()
                .find(|host| host.id == selected)
                .map(|host| host.label())
                .unwrap_or_else(|| tr("このPC").into())
        })
    };
    let items = move || {
        let all = all();
        let selected = connection.host.get_untracked();
        let mut entries = vec![
            json!({"id": "", "title": tr("すべての接続先"), "selected": all}),
            json!({"separator": true}),
        ];
        entries.extend(hosts.with_untracked(|hosts| {
            hosts
                .iter()
                .map(|host| json!({"id": host.id, "title": host.name, "selected": !all && host.id == selected}))
                .collect::<Vec<_>>()
        }));
        json!(entries)
    };
    view()
        .flex_row()
        .items_center()
        .justify_center()
        .gap_1()
        .h(px(32.0))
        .px_3()
        .rounded(px(ui::RADIUS))
        .text_color(colors::foreground())
        .role(AccessibilityRole::Button)
        .label(tr("接続先"))
        .hover(|s| s.bg(colors::GHOST_HOVER))
        .active(|s| s.bg(colors::SELECTED))
        .on_press(move || {
            ui::popup(&items(), move |id: &str| {
                connection.application.with(|application| {
                    if id.is_empty() {
                        application.select_all_hosts();
                    } else {
                        application.select_host(id.to_owned());
                    }
                })
            })
        })
        .child(text(title).text_size(px(14.0)).number_of_lines(1))
        .child(ui::icon(Icon::CHEVRON_DOWN, 14.0))
}

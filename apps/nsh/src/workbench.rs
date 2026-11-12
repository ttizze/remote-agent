use crate::l10n::{localized, tr, tr_format};
use crate::{Connection, application::Application, shared::Shared, ui};
use agent_core::panels::{PanelKind, PanelScope, PanelState, PanelSurface, PanelTool};
use nsh_core::prelude::*;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Clone, Copy)]
pub(crate) struct Workbench {
    application: Stored<Arc<Application>>,
    pub scope: Memo<PanelScope>,
    pub state: Memo<PanelState>,
    /// The browser URL to load once its panel is ready; it clears it then.
    reveal: Signal<Option<String>>,
}

impl Workbench {
    pub fn new() -> Self {
        let connection = use_context::<Connection>().unwrap();
        let panels = computed(move || connection.state.with(|state| Shared(state.panels.clone())));
        let scope = connection.scope;
        let state = computed(move || panels.with(|panels| panels.get(scope.get())));
        Self {
            application: connection.application,
            scope,
            state,
            reveal: signal(None),
        }
    }

    pub fn open(self, kind: PanelKind, path: Option<String>, line: Option<u32>) {
        self.application.with(|application| {
            application.open_panel(self.scope.get_untracked(), kind, path, line)
        });
    }

    pub fn run_script(self, cwd: String, command: String) {
        self.application
            .with(|application| application.open_script(self.scope.get_untracked(), cwd, command));
    }

    /// Opens `url` in the in-app browser.
    pub fn browse(self, url: String) {
        self.open(PanelKind::Browser, None, None);
        self.reveal.set(Some(url));
    }

    /// Takes the URL held for the newly opened browser panel.
    pub fn take_reveal(self) -> Option<String> {
        let taken = self.reveal.get();
        self.reveal.set(None);
        taken
    }

    pub fn hide(self) {
        self.application
            .with(|application| application.set_panel_open(self.scope.get_untracked(), false));
    }

    fn activate(self, id: u64) {
        self.application
            .with(|application| application.activate_panel(self.scope.get_untracked(), id));
    }

    fn close(self, id: u64) {
        self.application
            .with(|application| application.close_panel(self.scope.get_untracked(), id));
    }

    pub fn terminal_handle(self, id: u64) -> String {
        self.application
            .with(|application| application.terminal_handle(id))
    }
}

pub(crate) fn title() -> String {
    let workspace = use_context::<Workbench>().unwrap();
    workspace.state.with(|state| {
        state
            .selected()
            .map(|surface| surface_title(surface.kind, surface.path.as_deref()))
            .unwrap_or_else(|| tr("ツール").into())
    })
}

fn surface_title(kind: PanelKind, path: Option<&str>) -> String {
    if kind == PanelKind::File {
        path.map(agent_core::presentation::markdown::file_basename)
            .unwrap_or(tr("ファイル"))
            .to_owned()
    } else {
        localized(agent_core::panels::panel_title(kind))
    }
}

/// A tool's SF Symbol in the system's menus.
fn symbol(kind: PanelKind) -> &'static str {
    match kind {
        PanelKind::Browser => "globe",
        PanelKind::Terminal => "terminal",
        PanelKind::Files => "folder",
        PanelKind::File => "doc",
        PanelKind::Diff => "plus.forwardslash.minus",
        PanelKind::SideChat => "bubble.left.and.bubble.right",
        PanelKind::Agents => "network",
        PanelKind::Home => "plus",
    }
}

/// A tool's icon in the panel, as the desktop client's.
fn tool_icon(kind: PanelKind) -> ui::Icon {
    use ui::Icon;
    match kind {
        PanelKind::Browser => Icon::GLOBE,
        PanelKind::Terminal => Icon::TERMINAL,
        PanelKind::Files => Icon::FOLDER,
        PanelKind::File => Icon::FILE_TEXT,
        PanelKind::Diff => Icon::DIFF,
        PanelKind::SideChat => Icon::BOT,
        PanelKind::Agents => Icon::NETWORK,
        PanelKind::Home => Icon::PLUS,
    }
}

/// The tools as menu items, each with its symbol for the system's menus.
pub(crate) fn tool_items() -> Vec<Value> {
    tools()
        .into_iter()
        .map(|tool| {
            json!({"id": tool.key, "title": tool.label, "symbol": symbol(tool.kind), "enabled": tool.enabled})
        })
        .collect()
}

fn tools() -> Vec<PanelTool> {
    let connection = use_context::<Connection>().unwrap();
    connection
        .publication
        .with(|p| {
            agent_core::panels::panel_tools(
                p.settings.connected(),
                !p.context.selected_directory().is_empty(),
            )
        })
        .into_iter()
        .map(|tool| PanelTool {
            label: localized(tool.label),
            ..tool
        })
        .collect()
}

/// Whether the tool `key` names can open now.
#[cfg(desktop)]
pub(crate) fn tool_enabled(key: &str) -> bool {
    tools().iter().any(|tool| tool.key == key && tool.enabled)
}

pub(crate) fn open_tool(key: &str) {
    let workspace = use_context::<Workbench>().unwrap();
    let connection = use_context::<Connection>().unwrap();
    if let Some(tool) = tools()
        .into_iter()
        .find(|tool| tool.key == key && tool.enabled)
    {
        let directory = connection.publication.with_untracked(|p| {
            agent_core::presentation::markdown::file_host_path(
                p.context.selected_directory(),
                p.context.home(),
            )
        });
        if tool.kind == PanelKind::SideChat {
            let provider = connection.composer.with_untracked(|p| p.controls.provider);
            workspace.application.with(|app| {
                app.open_side_chat(
                    workspace.scope.get_untracked(),
                    directory,
                    provider,
                    agent_core::store::SideChatStart::Quote(String::new()),
                )
            });
        } else {
            workspace.open(
                tool.kind,
                (tool.kind != PanelKind::Browser).then_some(directory),
                None,
            );
        }
    }
}

/// The panel, as T3 Code's: its tab bar over the selected surface, or the tool
/// picker while none is selected. `leading` starts the tab bar: the window's
/// title bar controls while the panel has taken the conversation's place.
pub(crate) fn screen(leading: Option<AnyElement>) -> impl IntoElement {
    let workspace = use_context::<Workbench>().unwrap();
    view()
        .flex_1()
        .min_h_0()
        .pb_keyboard()
        .accessibility_elements_hidden(move || !workspace.state.with(|state| state.open))
        .child(header(workspace, leading))
        .child(each(
            move || {
                workspace
                    .state
                    .with(|s| s.selected().cloned().into_iter().collect::<Vec<_>>())
            },
            |surface: &PanelSurface| surface.id,
            move |surface, _| {
                let host = workspace.scope.get_untracked().host;
                let PanelSurface { id, kind, path, .. } = surface.get_untracked();
                match kind {
                    PanelKind::Files => crate::files::screen(id).into_element(),
                    PanelKind::File => file(id),
                    PanelKind::Terminal => {
                        crate::terminal::screen(surface.get_untracked(), host).into_element()
                    }
                    PanelKind::Diff => {
                        crate::details::diff(path.unwrap_or_default(), host).into_element()
                    }
                    PanelKind::Agents => crate::details::agents(host).into_element(),
                    PanelKind::SideChat => crate::side_chat::screen(id, host).into_element(),
                    PanelKind::Browser => crate::browser::screen().into_element(),
                    PanelKind::Home => view().into_element(),
                }
            },
        ))
        .child(when(
            move || workspace.state.with(|s| s.selected().is_none()),
            picker,
        ))
}

/// An open file; on the desktop, the workspace's file list beside it, as the
/// desktop client's explorer: its width the user's, within what leaves the
/// file its room.
#[cfg(desktop)]
fn file(id: u64) -> AnyElement {
    use agent_core::presentation::files::{
        EXPLORER_CONTENT_MIN_WIDTH, EXPLORER_MIN_WIDTH, explorer_max_width, explorer_width,
    };
    let connection = use_context::<Connection>().unwrap();
    let available = signal(f32::INFINITY);
    let preferred = computed(move || connection.state.with(|state| state.explorer_width));
    view()
        .flex_1()
        .min_h_0()
        .on_layout(move |frame: &nsh_core::Bounds<Points>| available.set(frame.size.width.0))
        .child(
            nsh_base::h_resizable()
                .handle_class("w-px bg-border group-hover:bg-primary group-active:bg-primary")
                .handle_accessibility_label(Some(SharedString::from(tr("ファイル一覧の幅を変更"))))
                .child(
                    nsh_base::resizable_panel()
                        .min_size(px(EXPLORER_CONTENT_MIN_WIDTH))
                        .child(crate::editor::editors()),
                )
                .child(
                    nsh_base::resizable_panel()
                        .size(move || Some(px(explorer_width(preferred.get(), available.get()))))
                        .min_size(px(EXPLORER_MIN_WIDTH))
                        .max_size(move || Some(px(explorer_max_width(available.get()))))
                        .on_resize(move |size: &Points| {
                            let width = explorer_width(size.0, available.get_untracked());
                            connection
                                .application
                                .with(|application| application.set_explorer_width(width));
                        })
                        .child(crate::files::explorer(id)),
                ),
        )
        .into_element()
}

#[cfg(not(desktop))]
fn file(_: u64) -> AnyElement {
    crate::editor::editors().into_element()
}

/// T3 Code's panel tab bar: a tab for each open surface and the add-tool menu,
/// then the panel's controls.
fn header(workspace: Workbench, leading: Option<AnyElement>) -> View {
    let bar = view()
        .h(px(ui::HEADER_HEIGHT))
        .flex_shrink_0()
        .flex_row()
        .items_center()
        .gap_1()
        .pl_2()
        .window_drag(true)
        .child(leading)
        .child(
            scroll().horizontal().flex_1().min_w_0().child(
                view()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .child(each(
                        move || workspace.state.with(|s| s.surfaces.clone()),
                        |surface: &PanelSurface| surface.id,
                        move |surface, _| tab(workspace, surface),
                    ))
                    .child(view().window_drag(false).child(add_tool())),
            ),
        );
    #[cfg(desktop)]
    let bar = bar
        .pr(ui::titlebar_end_inset)
        .child(panel_controls(workspace));
    #[cfg(not(desktop))]
    let bar = bar.pr_3();
    bar
}

/// T3 Code's controls of an open panel: maximizing it, then the terminal and
/// right panel toggles where the conversation's header has them while it is closed.
#[cfg(desktop)]
fn panel_controls(workspace: Workbench) -> impl IntoElement {
    use ui::Icon;
    let browser = use_context::<crate::files::Browser>().unwrap();
    view()
        .flex_row()
        .items_center()
        .gap_1()
        .window_drag(false)
        .child(dynamic(move || {
            let maximized = workspace.state.with(|state| state.maximized);
            let (glyph, label) = if maximized {
                (Icon::MINIMIZE, tr("元のサイズに戻す"))
            } else {
                (Icon::MAXIMIZE, tr("拡大表示"))
            };
            ui::toggle(
                glyph,
                label,
                move || maximized,
                move || {
                    workspace.application.with(|app| {
                        app.set_panel_maximized(workspace.scope.get_untracked(), !maximized)
                    })
                },
            )
        }))
        .child(terminal_toggle(workspace))
        .child(ui::toggle(
            Icon::PANEL_RIGHT,
            tr("ツールを閉じる"),
            || true,
            move || browser.close(),
        ))
}

/// T3 Code's terminal toggle, at the same place with the panel open or closed.
#[cfg(desktop)]
pub(crate) fn terminal_toggle(workbench: Workbench) -> impl IntoElement {
    use ui::Enabled;
    let open = move || {
        workbench.state.with(|state| {
            state.open
                && state
                    .selected()
                    .is_some_and(|surface| surface.kind == PanelKind::Terminal)
        })
    };
    ui::toggle(
        ui::Icon::PANEL_BOTTOM,
        tr("ターミナルを切り替え"),
        open,
        move || {
            if open() {
                workbench.hide();
            } else {
                open_tool("terminal");
            }
        },
    )
    .enabled(|| tool_enabled("terminal"))
}

/// The add-tool menu: a popup from a small button on the desktop; the system's
/// menu button on a phone, which shows no popups.
fn add_tool() -> AnyElement {
    if is_desktop() {
        ui::icon_button(ui::Icon::PLUS, tr("ツールを追加"), || {
            ui::popup(&json!(tool_items()), open_tool)
        })
        .into_element()
    } else {
        crate::native::menu(
            "plus",
            tr("ツールを追加"),
            || json!(tool_items()),
            open_tool,
        )
        .w(px(44.0))
        .h(px(44.0))
        .into_element()
    }
}

/// A surface's tab: its tool's icon and title, filled while it is selected, with
/// its close button while it is selected or under the pointer.
fn tab(workspace: Workbench, surface: ReadSignal<PanelSurface>) -> View {
    use ui::{Icon, colors};
    let (id, kind) = surface.with_untracked(|s| (s.id, s.kind));
    let title = move || surface.with(|s| surface_title(s.kind, s.path.as_deref()));
    let active = move || workspace.state.with(|s| s.active == Some(id));
    view()
        .group()
        .flex_row()
        .items_center()
        .flex_shrink_0()
        .h(px(24.0))
        .max_w(px(180.0))
        .pr_1()
        .rounded(px(ui::RADIUS))
        .text_color(colors::muted())
        .hover(|s| s.bg(colors::GHOST_HOVER).text_color(colors::foreground()))
        .style_when(active, |s| {
            s.bg(colors::SELECTED).text_color(colors::foreground())
        })
        .child(
            nsh_base::button()
                .flex_row()
                .items_center()
                .gap_1()
                .min_w_0()
                .h_full()
                .pl(px(6.0))
                .pr_1()
                .accessibility_label(move || Some(SharedString::from(title())))
                .on_press(move || workspace.activate(id))
                .child(ui::icon(tool_icon(kind), 12.0))
                .child(text(title).text_size(px(12.0)).number_of_lines(1).min_w_0()),
        )
        .child(
            view()
                .window_drag(false)
                .opacity(0.0)
                .group_hover(|s| s.opacity(1.0))
                .style_when(active, |s| s.opacity(1.0))
                .child(ui::small_icon_button(
                    Icon::CLOSE,
                    move || SharedString::from(tr_format("{0}を閉じる", [&title()])),
                    move || workspace.close(id),
                )),
        )
}

/// T3 Code's empty panel: the tools as rows under a heading, each with its
/// key. As T3 Code's launcher it takes the keyboard, so a tool's key opens it
/// until a text field is focused again.
fn picker() -> impl IntoElement {
    use ui::colors;
    if is_desktop() {
        dismiss_keyboard();
        // Which tools are enabled changes; their keys do not.
        for tool in agent_core::panels::panel_tools(false, false) {
            on_key_with(
                &tool.shortcut.to_lowercase(),
                KeyOptions {
                    when: nsh_core::keymap::When::NotEditing,
                    repeat: false,
                    ..Default::default()
                },
                move |_| open_tool(&tool.key),
            );
        }
    }
    view()
        .flex_1()
        .min_h_0()
        .items_center()
        .justify_center()
        .px_6()
        .child(
            view()
                .w_full()
                .max_w(px(320.0))
                .gap(px(2.0))
                .child(
                    text(tr("機能を開く"))
                        .mb_3()
                        .text_center()
                        .text_size(px(14.0))
                        .font_weight(FontWeight::MEDIUM),
                )
                .child(each(
                    tools,
                    |tool: &PanelTool| tool.key.clone(),
                    move |tool, _| {
                        let PanelTool {
                            kind,
                            label,
                            shortcut,
                            ..
                        } = tool.get_untracked();
                        nsh_base::button()
                            .flex_row()
                            .items_center()
                            .gap(px(10.0))
                            .h(px(32.0))
                            .px(px(10.0))
                            .rounded(px(ui::RADIUS))
                            .text_color(colors::foreground())
                            .hover(|s| s.bg(colors::GHOST_HOVER))
                            .disabled_style(|s| s.opacity(0.5))
                            .disabled(move || !tool.with(|t| t.enabled))
                            .on_press(move || open_tool(&tool.with_untracked(|t| t.key.clone())))
                            .child(ui::icon(tool_icon(kind), 16.0))
                            .child(
                                text(label)
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(14.0))
                                    .number_of_lines(1),
                            )
                            .child(when(is_desktop, move || {
                                text(shortcut.clone())
                                    .text_size(px(12.0))
                                    .text_color(colors::muted())
                            }))
                    },
                )),
        )
}

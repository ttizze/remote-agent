//! Settings: the navigation that replaces the sidebar, and its pages
//! (Project, General, Appearance, Keybindings, Providers, Connections,
//! Archive).
mod add_project;
mod appearance;
mod archived;
mod general;
mod import;
mod keybindings;
mod projects;
mod providers;
mod scheduled_tasks;
mod scripts;
mod usage;

use super::{
    Desktop, Route,
    sidebar::window_drag,
    ui::{self, color, icon, tint},
};
use agent_core::state::Intent;
use gpui_kit::{
    component::{
        Sizable, StyledExt,
        button::{Button, ButtonVariants},
        h_flex,
        menu::{DropdownMenu, PopupMenuItem},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::rc::Rc;

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum SettingsPage {
    General,
    Appearance,
    Keybindings,
    Projects { project_id: Option<String> },
    Providers,
    Connections,
    Archived,
    ScheduledTasks,
    Usage,
    About,
    Diagnostics,
    Licenses,
}

impl SettingsPage {
    /// The navigation entries in their order.
    fn sections() -> [SettingsPage; 12] {
        [
            SettingsPage::Projects { project_id: None },
            SettingsPage::General,
            SettingsPage::Appearance,
            SettingsPage::Keybindings,
            SettingsPage::Providers,
            SettingsPage::Connections,
            SettingsPage::Archived,
            SettingsPage::ScheduledTasks,
            SettingsPage::Usage,
            SettingsPage::About,
            SettingsPage::Diagnostics,
            SettingsPage::Licenses,
        ]
    }
    fn label(&self) -> &'static str {
        match self {
            SettingsPage::General => "General",
            SettingsPage::Appearance => "Appearance",
            SettingsPage::Keybindings => "Keybindings",
            SettingsPage::Projects { .. } => "Project",
            SettingsPage::Providers => "Providers",
            SettingsPage::Connections => "Connections",
            SettingsPage::Archived => "Archive",
            SettingsPage::ScheduledTasks => "Scheduled tasks",
            SettingsPage::Usage => "Usage",
            SettingsPage::About => "About",
            SettingsPage::Diagnostics => "Diagnostics",
            SettingsPage::Licenses => "Licenses",
        }
    }
    fn icon(&self) -> &'static str {
        match self {
            SettingsPage::General => "settings-2",
            SettingsPage::Appearance => "palette",
            SettingsPage::Keybindings => "keyboard",
            SettingsPage::Projects { .. } => "panels-top-left",
            SettingsPage::Providers => "bot",
            SettingsPage::Connections => "link-2",
            SettingsPage::Archived => "archive",
            SettingsPage::ScheduledTasks => "calendar-clock",
            SettingsPage::Usage => "chart-no-axes-combined",
            SettingsPage::About => "info",
            SettingsPage::Diagnostics => "activity",
            SettingsPage::Licenses => "scroll-text",
        }
    }
    fn same_section(&self, other: &SettingsPage) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }
}

pub(crate) struct SettingsState {
    pub(crate) page: SettingsPage,
    focus: FocusHandle,
    /// The store whose archive subscription is open.
    archive_store: Option<String>,
    /// The page and store whose data was last requested.
    loaded: Option<(SettingsPage, String)>,
    general: general::GeneralState,
    appearance: appearance::AppearanceState,
    keybindings: keybindings::KeybindingsState,
    providers: providers::ProvidersState,
    scheduled_tasks: scheduled_tasks::ScheduledTasksState,
    usage: usage::UsageState,
    /// The stores whose onboarding already ran in this app session.
    onboarded: std::collections::HashSet<String>,
}
impl SettingsState {
    /// A shortcut is being recorded, so keys go to it and run nothing.
    pub(crate) fn recording_shortcut(&self) -> bool {
        self.keybindings.recording()
    }

    pub(crate) fn new(window: &mut Window, cx: &mut Context<Desktop>) -> Self {
        Self {
            page: SettingsPage::General,
            focus: cx.focus_handle(),
            archive_store: None,
            loaded: None,
            general: general::GeneralState::new(window, cx),
            appearance: appearance::AppearanceState::new(window, cx),
            keybindings: keybindings::KeybindingsState::new(window, cx),
            providers: providers::ProvidersState::new(window, cx),
            scheduled_tasks: scheduled_tasks::ScheduledTasksState::new(window, cx),
            usage: usage::UsageState::new(window, cx),
            onboarded: Default::default(),
        }
    }
}

impl Desktop {
    pub(crate) fn render_settings_nav(
        &mut self,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = self.settings.page.clone();
        let mut menu = v_flex().gap_1();
        for (index, page) in SettingsPage::sections().into_iter().enumerate() {
            let active = page.same_section(&current);
            let target = page.clone();
            menu = menu.child(
                h_flex()
                    .id(("settings-nav", index))
                    .h_8()
                    .px(px(10.))
                    .gap_2()
                    .rounded(px(8.))
                    .text_sm()
                    .cursor_pointer()
                    .when(active, |row| {
                        row.bg(color("sidebarRowSelected"))
                            .font_medium()
                            .text_color(color("sidebarForeground"))
                    })
                    .when(!active, |row| {
                        row.font_medium()
                            .text_color(tint("sidebarMutedForeground", 0.8))
                            .hover(|row| {
                                row.bg(color("sidebarRowHover"))
                                    .text_color(color("sidebarForeground"))
                            })
                    })
                    .child(icon(page.icon()).size_4().text_color(if active {
                        color("sidebarForeground")
                    } else {
                        tint("sidebarMutedForeground", 0.8)
                    }))
                    .child(div().truncate().child(page.label()))
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.open_settings(target.clone(), window, cx);
                    })),
            );
        }
        v_flex()
            .w(px(ui::metrics().sidebar_width))
            .h_full()
            .flex_shrink_0()
            .bg(color("sidebar"))
            .border_r_1()
            .border_color(color("sidebarBorder"))
            .text_color(color("sidebarForeground"))
            .child(
                window_drag(
                    h_flex()
                        .h(px(ui::metrics().header_height))
                        .flex_shrink_0()
                        .pr_3()
                        .pl(px(if cfg!(target_os = "macos") { 90. } else { 12. })),
                )
                .child(
                    div()
                        .id("settings-brand")
                        .text_sm()
                        .font_medium()
                        .cursor_pointer()
                        .text_color(color("text"))
                        .child("Bex")
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(|view, _, window, cx| {
                            view.close_settings(window, cx);
                        })),
                ),
            )
            .child(
                div()
                    .id("settings-nav-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_2()
                    .child(menu),
            )
            .child(
                div().px_2().py_1().child(
                    h_flex()
                        .id("settings-back")
                        .h_8()
                        .px(px(10.))
                        .gap_2()
                        .rounded(px(8.))
                        .text_sm()
                        .font_medium()
                        .cursor_pointer()
                        .text_color(tint("sidebarMutedForeground", 0.8))
                        .hover(|row| {
                            row.bg(color("sidebarRowHover"))
                                .text_color(color("sidebarForeground"))
                        })
                        .child(icon("arrow-left").size_4())
                        .child("Back")
                        .on_click(cx.listener(|view, _, window, cx| {
                            view.close_settings(window, cx);
                        })),
                ),
            )
            .into_any_element()
    }

    pub(crate) fn render_settings(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let page = self.settings.page.clone();
        let body = match &page {
            SettingsPage::General => self.render_general(window, cx),
            SettingsPage::Appearance => self.render_appearance(window, cx),
            SettingsPage::Keybindings => self.render_keybindings(window, cx),
            SettingsPage::Projects { project_id: None } => self.render_projects(cx),
            SettingsPage::Projects {
                project_id: Some(project_id),
            } => self.render_project(project_id, window, cx),
            SettingsPage::Providers => self.render_providers(window, cx),
            SettingsPage::Connections => {
                let current = self.remote.as_ref().map(|remote| remote.id.clone());
                let connected = self.snapshot.connected;
                self.hosts
                    .update(cx, |hosts, cx| hosts.set_current(current, connected, cx));
                page_container(
                    1024.,
                    vec![
                        self.hosts.clone().into_any_element(),
                        self.render_environment_overview(cx),
                    ],
                )
            }
            SettingsPage::Archived => self.render_archived(window, cx),
            SettingsPage::ScheduledTasks => self.render_scheduled_tasks(window, cx),
            SettingsPage::Usage => self.render_usage(window, cx),
            SettingsPage::About => self.render_about(),
            SettingsPage::Diagnostics => self.render_diagnostics(),
            SettingsPage::Licenses => self.render_licenses(),
        };
        v_flex()
            .id("settings")
            .track_focus(&self.settings.focus)
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    view.close_settings(window, cx);
                    cx.stop_propagation();
                }
            }))
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(color("canvas"))
            .child(
                window_drag(
                    h_flex()
                        .h(px(ui::metrics().header_height))
                        .flex_shrink_0()
                        .px(px(20.))
                        .when(self.sidebar_hidden && cfg!(target_os = "macos"), |header| {
                            header.pl(px(90.))
                        })
                        .gap_3()
                        .items_center(),
                )
                .child(
                    h_flex()
                        .min_w_0()
                        .gap_3()
                        .text_sm()
                        .font_medium()
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_color(color("textMuted"))
                                .child("Settings"),
                        )
                        .child(div().text_color(color("iconMuted")).child("/"))
                        .child(
                            div()
                                .truncate()
                                .text_color(color("text"))
                                .child(page.label()),
                        ),
                ),
            )
            .child(body)
            .into_any_element()
    }

    fn render_environment_overview(&mut self, cx: &mut Context<Desktop>) -> AnyElement {
        let entries = self.views.environment_settings.entries.clone();
        v_flex()
            .gap_3()
            .child(
                h_flex().gap_2().child(icon("layers").size_4()).child(
                    div()
                        .text_sm()
                        .font_medium()
                        .child("Connected environments"),
                ),
            )
            .children(entries.into_iter().map(|entry| {
                let id = entry.summary.descriptor.environment_id.clone();
                let label = entry.summary.descriptor.label.clone();
                let platform = format!(
                    "{} / {} · {} · server {}",
                    entry.summary.descriptor.platform.os,
                    entry.summary.descriptor.platform.arch,
                    entry
                        .summary
                        .descriptor
                        .platform
                        .machine
                        .as_deref()
                        .unwrap_or("unknown machine"),
                    entry.summary.descriptor.server_version,
                );
                let connection = match entry.summary.connection {
                    agent_core::environment::EnvironmentConnectionState::Connected => "Connected",
                    agent_core::environment::EnvironmentConnectionState::Connecting => {
                        "Connecting…"
                    }
                    agent_core::environment::EnvironmentConnectionState::Disconnected => "Offline",
                };
                let connection = entry.summary.reconnect_reason.as_deref().map_or_else(
                    || connection.to_owned(),
                    |reason| format!("{connection} · {reason}"),
                );
                let selected = self.environment_registry.selected() == Some(id.as_str());
                h_flex()
                    .w_full()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded(px(8.))
                    .when(selected, |row| row.bg(color("sidebarRowSelected")))
                    .child(icon("monitor").size_4())
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().truncate().text_sm().child(label))
                            .child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(tint("textMuted", 0.8))
                                    .child(format!("{connection} · {platform}")),
                            )
                            .child(div().text_2xs().text_color(tint("textMuted", 0.7)).child(
                                format!(
                                        "{} host settings · {} capabilities",
                                        entry.settings.sections.len(),
                                        agent_core::environment::capability_names(
                                            &entry.summary.descriptor.capabilities
                                        )
                                        .len()
                                    ),
                            )),
                    )
                    .when(!selected, |row| {
                        row.cursor_pointer()
                            .on_click(cx.listener(move |view, _, _, cx| {
                                if view.promote_environment(&id) {
                                    view.environment_registry.select(&id);
                                    cx.notify();
                                }
                            }))
                    })
            }))
            .into_any_element()
    }

    pub(crate) fn open_settings(
        &mut self,
        page: SettingsPage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.page = page;
        self.route = Route::Settings;
        window.focus(&self.settings.focus, cx);
        self.sync_settings(window, cx);
        cx.notify();
    }

    /// Back to the conversation the settings were opened over.
    fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.route = Route::Chat;
        self.sync_settings(window, cx);
        cx.notify();
    }

    pub(crate) fn open_add_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        add_project::open(self, window, cx);
    }

    /// Onboarding: a Host with no projects and no threads yet offers, once,
    /// to import the projects and conversations of its agent sessions.
    pub(crate) fn offer_onboarding_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let store = self.snapshot.store_id.clone();
        if self.session.is_none()
            || !self.snapshot.connected
            || self.settings.onboarded.contains(&store)
        {
            return;
        }
        if self.snapshot.shell_status() != agent_core::sync::shell::ShellStatus::Live {
            return;
        }
        let Some(shell) = self.snapshot.shell_view() else {
            return;
        };
        let fresh = shell.projects.is_empty() && shell.threads.is_empty();
        self.settings.onboarded.insert(store);
        if fresh {
            import::open(self, window, cx);
        }
    }

    /// Follows the snapshot: the archive subscription and loaded settings.
    pub(crate) fn sync_settings(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let showing = self.route == Route::Settings;
        let store = self
            .session
            .is_some()
            .then(|| self.snapshot.store_id.clone());
        let archive = store
            .clone()
            .filter(|_| showing && self.settings.page == SettingsPage::Archived);
        if archive != self.settings.archive_store {
            if archive.is_some() {
                self.perform(Intent::ShowArchived { open: true });
            } else if self.settings.archive_store == store {
                self.perform(Intent::ShowArchived { open: false });
            }
            self.settings.archive_store = archive;
        }
        let loaded = store
            .filter(|_| showing && self.snapshot.connected)
            .map(|store| (self.settings.page.clone(), store));
        if loaded != self.settings.loaded {
            if let Some((page, _)) = &loaded {
                self.load_settings_page(page, cx);
            }
            self.settings.loaded = loaded;
        }
    }

    fn load_settings_page(&mut self, page: &SettingsPage, cx: &mut Context<Self>) {
        match page {
            SettingsPage::General => {
                self.perform(Intent::LoadSettings);
                self.perform(Intent::LoadWorktreeSettings);
                self.perform(Intent::ListWorktrees);
            }
            SettingsPage::Projects { .. } => self.perform(Intent::LoadSettings),
            SettingsPage::Providers => {
                self.perform(Intent::LoadAccounts);
                self.perform(Intent::LoadProviders);
                self.perform(Intent::LoadSettings);
            }
            SettingsPage::Connections => self.hosts.update(cx, |hosts, _| hosts.refresh()),
            SettingsPage::Archived
            | SettingsPage::Appearance
            | SettingsPage::Keybindings
            | SettingsPage::ScheduledTasks => {}
            SettingsPage::Usage => self.perform(Intent::LoadUsageSummary {
                input: usage::summary_input(&self.snapshot),
            }),
            SettingsPage::About
            | SettingsPage::Diagnostics
            | SettingsPage::Licenses => {}
        }
    }

    fn render_about(&self) -> AnyElement {
        page_container(
            896.,
            vec![section(
                Some("About".into()),
                None,
                None,
                vec![
                    Row::new("Version")
                        .description(env!("CARGO_PKG_VERSION"))
                        .render(),
                    Row::new("Privacy")
                        .description("The Host and clients keep conversation data in their local stores.")
                        .render(),
                ],
            )
            .into_any_element()],
        )
    }

    fn render_diagnostics(&self) -> AnyElement {
        page_container(
            896.,
            vec![
                section(
                    Some("Diagnostics".into()),
                    None,
                    None,
                    vec![
                        Row::new("Connection")
                            .description(if self.snapshot.connected {
                                "Connected"
                            } else {
                                "Disconnected"
                            })
                            .render(),
                        Row::new("Logs")
                            .description(
                                crate::platform::state_dir()
                                    .map(|path| path.join("logs").display().to_string())
                                    .unwrap_or_else(|error| format!("Unavailable: {error}")),
                            )
                            .render(),
                    ],
                )
                .into_any_element(),
            ],
        )
    }

    fn render_licenses(&self) -> AnyElement {
        page_container(
            896.,
            vec![
                section(
                    Some("Licenses and legal".into()),
                    None,
                    None,
                    vec![
                        Row::new("Open-source notices")
                            .description(
                                "Third-party license notices are included with this release.",
                            )
                            .render(),
                    ],
                )
                .into_any_element(),
            ],
        )
    }
}

/// The scrolling column a settings page lays its sections in.
fn page_container(max_width: f32, sections: Vec<AnyElement>) -> AnyElement {
    div()
        .id("settings-page")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .child(
            v_flex()
                .w_full()
                .max_w(px(max_width))
                .mx_auto()
                .px_6()
                .pt_6()
                .pb_12()
                .gap_8()
                .children(sections),
        )
        .into_any_element()
}

/// A titled group of rows.
fn section(
    title: Option<SharedString>,
    leading: Option<AnyElement>,
    action: Option<AnyElement>,
    rows: Vec<AnyElement>,
) -> Div {
    v_flex()
        .gap(px(10.))
        .when_some(title, |section, title| {
            section.child(
                h_flex()
                    .min_h_7()
                    .px_4()
                    .gap_4()
                    .justify_between()
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap_2()
                            .text_sm()
                            .text_color(tint("text", 0.7))
                            .children(leading)
                            .child(div().truncate().child(title)),
                    )
                    .child(h_flex().min_h_7().min_w_7().justify_end().children(action)),
            )
        })
        .child(group(rows))
}

/// The rounded card of a section, a line between its rows.
fn group(rows: Vec<AnyElement>) -> Div {
    v_flex()
        .rounded(px(14.))
        .border_1()
        .border_color(tint("border", 0.6))
        .bg(tint("surface", 0.4))
        .children(rows.into_iter().enumerate().map(|(index, row)| {
            div()
                .when(index > 0, |line| {
                    line.border_t_1().border_color(tint("border", 0.5))
                })
                .child(row)
        }))
}

/// One setting: its title and description, the control on the right.
struct Row {
    title: AnyElement,
    description: Option<AnyElement>,
    status: Option<AnyElement>,
    reset: Option<AnyElement>,
    control: Option<AnyElement>,
    below: Vec<AnyElement>,
}

impl FluentBuilder for Row {}

impl Row {
    fn new(title: impl IntoElement) -> Self {
        Self {
            title: title.into_any_element(),
            description: None,
            status: None,
            reset: None,
            control: None,
            below: vec![],
        }
    }
    fn description(mut self, description: impl IntoElement) -> Self {
        self.description = Some(description.into_any_element());
        self
    }
    fn status(mut self, status: impl IntoElement) -> Self {
        self.status = Some(status.into_any_element());
        self
    }
    fn reset(mut self, reset: Option<AnyElement>) -> Self {
        self.reset = reset;
        self
    }
    fn control(mut self, control: impl IntoElement) -> Self {
        self.control = Some(control.into_any_element());
        self
    }
    fn below(mut self, element: impl IntoElement) -> Self {
        self.below.push(element.into_any_element());
        self
    }
    fn render(self) -> AnyElement {
        v_flex()
            .px_4()
            .py_3()
            .gap_2()
            .child(
                h_flex()
                    .gap_8()
                    .items_center()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                h_flex()
                                    .min_h_5()
                                    .gap(px(6.))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .text_sm()
                                            .font_medium()
                                            .text_color(color("text"))
                                            .child(self.title),
                                    )
                                    .when_some(self.reset, |title, reset| {
                                        title.child(
                                            h_flex()
                                                .size_5()
                                                .flex_shrink_0()
                                                .justify_center()
                                                .child(reset),
                                        )
                                    }),
                            )
                            .when_some(self.description, |text, description| {
                                text.child(
                                    div()
                                        .max_w(px(576.))
                                        .text_xs()
                                        .line_height(px(18.))
                                        .text_color(tint("textMuted", 0.8))
                                        .child(description),
                                )
                            })
                            .when_some(self.status, |text, status| {
                                text.child(
                                    div()
                                        .pt(px(2.))
                                        .text_xs()
                                        .text_color(color("textMuted"))
                                        .child(status),
                                )
                            }),
                    )
                    .when_some(self.control, |row, control| {
                        row.child(
                            h_flex()
                                .flex_shrink_0()
                                .gap_2()
                                .justify_end()
                                .child(control),
                        )
                    }),
            )
            .children(self.below)
            .into_any_element()
    }
}

/// The arrow beside a changed setting's title.
fn reset_button(
    id: impl Into<ElementId>,
    label: &str,
    tooltip: &'static str,
    on_click: impl Fn(&mut Desktop, &mut Window, &mut Context<Desktop>) + 'static,
    cx: &mut Context<Desktop>,
) -> AnyElement {
    Button::new(id)
        .icon(icon("undo-2"))
        .ghost()
        .xsmall()
        .size_5()
        .text_color(color("textMuted"))
        .tooltip(tooltip)
        .accessibility_label(format!("Reset {label} to default"))
        .on_click(cx.listener(move |view, _, window, cx| on_click(view, window, cx)))
        .into_any_element()
}

/// One entry of a select menu.
struct Choice {
    id: String,
    label: String,
    description: Option<String>,
    icon: Option<&'static str>,
    selected: bool,
}

/// A select trigger showing the chosen label; picking an entry calls `pick`.
fn select(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    choices: Vec<Choice>,
    pick: impl Fn(&mut Desktop, String, &mut Window, &mut Context<Desktop>) + 'static,
    cx: &mut Context<Desktop>,
) -> impl IntoElement {
    select_sized(id, label, choices, 176., pick, cx)
}

/// A select trigger `width` wide.
fn select_sized(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    choices: Vec<Choice>,
    width: f32,
    pick: impl Fn(&mut Desktop, String, &mut Window, &mut Context<Desktop>) + 'static,
    cx: &mut Context<Desktop>,
) -> impl IntoElement {
    let owner = cx.entity().downgrade();
    let pick = Rc::new(pick);
    let glyph = choices
        .iter()
        .find(|choice| choice.selected)
        .and_then(|choice| choice.icon);
    let choices = Rc::new(choices);
    Button::new(id)
        .outline()
        .small()
        .w(px(width))
        .when_some(glyph, |button, glyph| button.icon(icon(glyph)))
        .label(label)
        .dropdown_caret(true)
        .dropdown_menu_with_anchor(Anchor::TopRight, move |mut menu, _, _| {
            for choice in choices.iter() {
                let owner = owner.clone();
                let pick = pick.clone();
                let id = choice.id.clone();
                let item = match &choice.description {
                    None => PopupMenuItem::new(choice.label.clone()),
                    Some(description) => {
                        let label = choice.label.clone();
                        let description = description.clone();
                        let glyph = choice.icon;
                        PopupMenuItem::element(move |_, _| {
                            v_flex()
                                .min_w(px(256.))
                                .gap(px(2.))
                                .child(
                                    h_flex()
                                        .gap(px(6.))
                                        .font_medium()
                                        .when_some(glyph, |row, glyph| {
                                            row.child(
                                                icon(glyph)
                                                    .size(px(14.))
                                                    .text_color(color("textMuted")),
                                            )
                                        })
                                        .child(label.clone()),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(color("textMuted"))
                                        .child(description.clone()),
                                )
                        })
                    }
                };
                menu = menu.item(
                    item.checked(choice.selected)
                        .on_click(move |_, window, cx| {
                            let _ = owner.update(cx, |view, cx| pick(view, id.clone(), window, cx));
                        }),
                );
            }
            menu
        })
}

/// The muted line a page shows while it has nothing else to show.
fn notice(text: impl Into<SharedString>) -> AnyElement {
    div()
        .text_sm()
        .text_color(color("textMuted"))
        .child(text.into())
        .into_any_element()
}

//! Settings: the navigation that replaces the sidebar, and its pages
//! (General, Projects, Providers, Connections, Archived).
use super::Desktop;
use gpui_kit::*;

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum SettingsPage {
    General,
    Projects { project_id: Option<String> },
    Providers,
    Connections,
    Archived,
}

pub(crate) struct SettingsState {
    pub(crate) page: SettingsPage,
}
impl SettingsState {
    pub(crate) fn new(_: &mut Window, _: &mut Context<Desktop>) -> Self {
        Self {
            page: SettingsPage::General,
        }
    }
}

impl Desktop {
    pub(crate) fn render_settings_nav(
        &mut self,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> AnyElement {
        div().into_any_element()
    }

    pub(crate) fn render_settings(&mut self, _: &mut Window, _: &mut Context<Self>) -> AnyElement {
        div().flex_1().into_any_element()
    }

    pub(crate) fn open_settings(
        &mut self,
        page: SettingsPage,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.page = page;
        self.route = super::Route::Settings;
        cx.notify();
    }

    /// Starts adding a project, then offers to import its agent sessions.
    pub(crate) fn open_add_project(&mut self, _: &mut Window, _: &mut Context<Self>) {}

    /// Follows the snapshot: the archive subscription and loaded settings.
    pub(crate) fn sync_settings(&mut self, _: &mut Window, _: &mut Context<Self>) {}
}

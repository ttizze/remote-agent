//! Where the desktop sidebar shows, as T3 Code's (`components/ui/sidebar.tsx`):
//! beside the content from a 768 point wide window, and below that as a sheet
//! over the content, closed until opened, that closes again once the reader
//! goes somewhere from it. The two keep their own open state, so a window
//! that narrows and widens again finds each as it was.

mod projects;
pub use projects::{
    ProjectKey, ProjectReveal, ProjectRoute, matching_projects, order_projects, resolve_projects,
    scoped_list, select_projects, widens_scope,
};

/// The narrowest window that keeps the sidebar beside the content (T3's
/// `max-md` breakpoint, `(max-width: 767px)`).
pub const SHEET_BREAKPOINT: f32 = 768.;

/// How the sidebar shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarPresentation {
    /// A column beside the content.
    Beside,
    /// A sheet over the content.
    Sheet,
    Hidden,
}

/// Whether the column and the sheet are open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidebarState {
    pub open: bool,
    pub sheet_open: bool,
}

impl Default for SidebarState {
    fn default() -> Self {
        Self {
            open: true,
            sheet_open: false,
        }
    }
}

/// Whether a window this wide shows the sidebar as a sheet.
pub fn shows_sheet(window_width: f32) -> bool {
    window_width < SHEET_BREAKPOINT
}

impl SidebarState {
    pub fn presentation(self, window_width: f32) -> SidebarPresentation {
        match (shows_sheet(window_width), self.open, self.sheet_open) {
            (false, true, _) => SidebarPresentation::Beside,
            (true, _, true) => SidebarPresentation::Sheet,
            _ => SidebarPresentation::Hidden,
        }
    }

    /// The sidebar's toggle: the sheet in a narrow window, else the column.
    pub fn toggled(self, window_width: f32) -> Self {
        if shows_sheet(window_width) {
            Self {
                sheet_open: !self.sheet_open,
                ..self
            }
        } else {
            Self {
                open: !self.open,
                ..self
            }
        }
    }

    /// After the reader chose a conversation, started one or opened settings:
    /// the sheet closes; the column stays.
    pub fn navigated(self, window_width: f32) -> Self {
        if shows_sheet(window_width) {
            Self {
                sheet_open: false,
                ..self
            }
        } else {
            self
        }
    }

    /// The sheet's backdrop pressed, or Escape.
    pub fn sheet_dismissed(self) -> Self {
        Self {
            sheet_open: false,
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sidebar_becomes_a_closed_sheet_below_768_points() {
        let state = SidebarState::default();
        assert_eq!(state.presentation(768.), SidebarPresentation::Beside);
        assert_eq!(state.presentation(767.9), SidebarPresentation::Hidden);
        let opened = state.toggled(500.);
        assert_eq!(opened.presentation(500.), SidebarPresentation::Sheet);
        // The column was untouched: widening shows it beside the content.
        assert_eq!(opened.presentation(900.), SidebarPresentation::Beside);
        assert_eq!(opened.toggled(500.), state);
    }

    #[test]
    fn a_wide_toggle_closes_the_column_and_leaves_the_sheet() {
        let closed = SidebarState::default().toggled(1200.);
        assert_eq!(closed.presentation(1200.), SidebarPresentation::Hidden);
        assert_eq!(closed.presentation(500.), SidebarPresentation::Hidden);
        let sheet = closed.toggled(500.);
        assert_eq!(sheet.presentation(500.), SidebarPresentation::Sheet);
        assert_eq!(sheet.presentation(1200.), SidebarPresentation::Hidden);
    }

    #[test]
    fn going_somewhere_closes_only_a_narrow_windows_sheet() {
        let sheet = SidebarState::default().toggled(500.);
        assert_eq!(
            sheet.navigated(500.).presentation(500.),
            SidebarPresentation::Hidden
        );
        assert_eq!(sheet.navigated(900.), sheet);
        assert_eq!(
            SidebarState::default().navigated(900.).presentation(900.),
            SidebarPresentation::Beside
        );
        assert_eq!(sheet.sheet_dismissed(), SidebarState::default());
    }
}

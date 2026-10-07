//! Dragging a row's Settle, Un-settle or Wake button across its section: the
//! rows it passes take the action when the pointer is released.
use super::super::{Desktop, menus::MenuSurface};
use agent_core::{
    state::{Intent, ThreadAction},
    view::sidebar::{SidebarDropVerb, SidebarSection, sidebar_thread_key_at_y},
};
use gpui_kit::*;
use std::collections::HashMap;

/// The action a sweep applies.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SweepAction {
    Settle,
    Unsettle,
    Unsnooze,
}
impl SweepAction {
    /// Sweeps show the badge a row drop with the same outcome shows.
    pub(super) fn verb(self) -> SidebarDropVerb {
        match self {
            Self::Settle => SidebarDropVerb::Settle,
            Self::Unsettle => SidebarDropVerb::Unsettle,
            Self::Unsnooze => SidebarDropVerb::Wake,
        }
    }
}

/// Dragged from a row's action button.
#[derive(Clone)]
pub(crate) struct SweepDrag {
    pub(super) origin: String,
    pub(super) action: SweepAction,
}

/// Nothing follows the pointer: the swept rows show the action instead.
pub(super) struct SweepPreview;
impl Render for SweepPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

pub(super) struct SweepState {
    pub(super) action: SweepAction,
    origin: String,
    /// The pressed row's section; a sweep never leaves it.
    section: Option<SidebarSection>,
    target: Option<String>,
    /// The rows the action will reach.
    pub(super) keys: Vec<String>,
    /// Each row's top edge as last seen, for finding the row at the pointer.
    tops: HashMap<String, Pixels>,
}

impl Desktop {
    pub(super) fn start_sweep(&mut self, drag: &SweepDrag, cx: &mut Context<Self>) {
        let section = self.views.sidebar.row(&drag.origin).map(|row| row.section);
        self.sidebar.sweep = Some(SweepState {
            action: drag.action,
            origin: drag.origin.clone(),
            section,
            target: None,
            keys: vec![],
            tops: HashMap::new(),
        });
        self.sweep_to(&drag.origin.clone(), cx);
    }

    /// Records where a row is while a sweep runs.
    pub(super) fn sweep_row_moved(&mut self, key: &str, top: Pixels) {
        if let Some(sweep) = self.sidebar.sweep.as_mut() {
            sweep.tops.insert(key.to_owned(), top);
        }
    }

    /// The pointer moved to height `y` over a list showing `visible`.
    pub(super) fn sweep_moved(
        &mut self,
        y: Pixels,
        visible: Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(sweep) = self.sidebar.sweep.as_ref() else {
            return;
        };
        let rows: Vec<(String, f32)> = self
            .views
            .sidebar
            .thread_ids()
            .into_iter()
            .filter_map(|key| {
                let top = f32::from(*sweep.tops.get(&key)?);
                Some((key, top))
            })
            .collect();
        if let Some(key) = sidebar_thread_key_at_y(
            &rows,
            f32::from(y),
            f32::from(visible.top()),
            f32::from(visible.bottom()),
        ) {
            self.sweep_to(&key, cx);
        }
    }

    fn sweep_to(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(sweep) = self.sidebar.sweep.as_mut() else {
            return;
        };
        if sweep.target.as_deref() == Some(key) {
            return;
        }
        sweep.target = Some(key.to_owned());
        sweep.keys = self.views.sidebar.sweep_keys(&sweep.origin, key);
        cx.notify();
    }

    /// The release applies the action to the swept rows still in the section.
    pub(crate) fn finish_sweep(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sweep) = self.sidebar.sweep.take() else {
            return;
        };
        let keys: Vec<String> = sweep
            .keys
            .into_iter()
            .filter(|key| {
                sweep.section.is_some()
                    && self.views.sidebar.row(key).map(|row| row.section) == sweep.section
            })
            .collect();
        cx.notify();
        if keys.is_empty() {
            return;
        }
        let action = match sweep.action {
            SweepAction::Settle => {
                self.park_threads(keys, ThreadAction::Settle, MenuSurface::Sidebar, window, cx);
                return;
            }
            SweepAction::Unsettle => ThreadAction::Unsettle,
            SweepAction::Unsnooze => ThreadAction::Unsnooze,
        };
        for thread_id in keys {
            self.perform(Intent::Thread {
                thread_id,
                action: action.clone(),
            });
        }
    }

    /// Escape ends a sweep without applying it; false when none runs.
    pub(crate) fn cancel_sweep(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.sidebar.sweep.take().is_none() {
            return false;
        }
        cx.stop_active_drag(window);
        cx.notify();
        true
    }
}

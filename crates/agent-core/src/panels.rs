//! Conversation-owned surface tabs. Native views own the resources addressed by ids.
use crate::state::{DraftKey, Snapshot};
use std::{collections::BTreeMap, sync::Arc};

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct PanelScope {
    pub host: String,
    pub storage: String,
    pub draft: DraftKey,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelKind {
    Home,
    Browser,
    Terminal,
    Files,
    File,
    Diff,
    SideChat,
    Agents,
}
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelSurface {
    pub id: u64,
    pub kind: PanelKind,
    pub path: Option<String>,
    pub command: Option<String>,
    #[cfg_attr(feature = "bindings", uniffi(default = None))]
    pub terminal_handle: Option<String>,
}
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PanelState {
    pub open: bool,
    pub maximized: bool,
    pub surfaces: Vec<PanelSurface>,
    pub active: Option<u64>,
}
impl PanelState {
    pub fn selected(&self) -> Option<&PanelSurface> {
        self.surfaces
            .iter()
            .find(|surface| Some(surface.id) == self.active)
    }
    pub fn kind(&self) -> PanelKind {
        self.selected()
            .map_or(PanelKind::Home, |surface| surface.kind)
    }
}
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Clone, PartialEq)]
pub struct PanelTool {
    pub kind: PanelKind,
    pub key: String,
    pub label: String,
    pub shortcut: String,
    pub enabled: bool,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn panel_tools(connected: bool, has_directory: bool) -> Vec<PanelTool> {
    [
        (PanelKind::Browser, "browser", "ブラウザ", "B"),
        (PanelKind::Terminal, "terminal", "ターミナル", "T"),
        (PanelKind::Files, "files", "ファイル", "F"),
        (PanelKind::Diff, "diff", "変更", "D"),
        (PanelKind::SideChat, "sideChat", "サイドチャット", "S"),
        (PanelKind::Agents, "agents", "エージェント", "A"),
    ]
    .into_iter()
    .map(|(kind, key, label, shortcut)| PanelTool {
        kind,
        key: key.into(),
        label: label.into(),
        shortcut: shortcut.into(),
        enabled: matches!(kind, PanelKind::Browser | PanelKind::Agents)
            || (connected && has_directory),
    })
    .collect()
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn panel_title(kind: PanelKind) -> String {
    panel_tools(false, false)
        .into_iter()
        .find(|tool| {
            tool.kind
                == if kind == PanelKind::File {
                    PanelKind::Files
                } else {
                    kind
                }
        })
        .map_or_else(|| "機能を開く".into(), |tool| tool.label)
}

/// Immutable conversation-scoped surface state. Native owners adopt the result
/// of an action, which gives their own observation system the changed value.
#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
#[derive(Debug, Clone, Default)]
pub struct Panels {
    threads: BTreeMap<PanelScope, PanelState>,
    next_id: u64,
}
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct OpenedPanel {
    pub panels: Arc<Panels>,
    pub id: u64,
}
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ActivatedPanel {
    pub panels: Arc<Panels>,
    pub activated: bool,
}
#[cfg_attr(feature = "bindings", uniffi::export)]
impl Panels {
    #[cfg_attr(feature = "bindings", uniffi::constructor)]
    pub fn new() -> Arc<Self> {
        Arc::default()
    }
    pub fn get(&self, key: PanelScope) -> PanelState {
        self.threads.get(&key).cloned().unwrap_or_default()
    }
    pub fn scope(&self, id: u64) -> Option<PanelScope> {
        self.threads
            .iter()
            .find(|(_, panel)| panel.surfaces.iter().any(|surface| surface.id == id))
            .map(|(key, _)| key.clone())
    }
    pub fn open(
        self: Arc<Self>,
        key: PanelScope,
        kind: PanelKind,
        path: Option<String>,
    ) -> OpenedPanel {
        let mut next = self.as_ref().clone();
        let panel = next.threads.entry(key).or_default();
        panel.open = true;
        if !matches!(
            kind,
            PanelKind::Browser | PanelKind::Terminal | PanelKind::SideChat
        ) && let Some(surface) = panel.surfaces.iter_mut().find(|surface| {
            surface.kind == kind && (kind == PanelKind::Files || surface.path == path)
        }) {
            if path.is_some() || kind != PanelKind::Files {
                surface.path = path;
            }
            panel.active = Some(surface.id);
            let id = surface.id;
            return OpenedPanel {
                panels: Arc::new(next),
                id,
            };
        }
        let id = next.next_id;
        next.next_id += 1;
        panel.surfaces.push(PanelSurface {
            id,
            kind,
            path,
            command: None,
            terminal_handle: None,
        });
        panel.active = Some(id);
        OpenedPanel {
            panels: Arc::new(next),
            id,
        }
    }
    pub fn open_script(
        self: Arc<Self>,
        key: PanelScope,
        cwd: String,
        command: String,
    ) -> OpenedPanel {
        let mut opened = self.open(key.clone(), PanelKind::Terminal, Some(cwd));
        let panels = Arc::make_mut(&mut opened.panels);
        let surface = panels
            .threads
            .get_mut(&key)
            .unwrap()
            .surfaces
            .last_mut()
            .unwrap();
        surface.command = Some(command);
        opened
    }
    pub fn activate(self: Arc<Self>, key: PanelScope, id: u64) -> ActivatedPanel {
        if self
            .threads
            .get(&key)
            .is_none_or(|panel| !panel.surfaces.iter().any(|surface| surface.id == id))
        {
            return ActivatedPanel {
                panels: self,
                activated: false,
            };
        }
        let mut next = self.as_ref().clone();
        let panel = next.threads.get_mut(&key).unwrap();
        panel.active = Some(id);
        panel.open = true;
        ActivatedPanel {
            panels: Arc::new(next),
            activated: true,
        }
    }

    pub fn open_setup_terminal(
        self: Arc<Self>,
        key: PanelScope,
        terminal: crate::presentation::conversation::SetupTerminal,
    ) -> OpenedPanel {
        if let Some(surface) = self
            .get(key.clone())
            .surfaces
            .iter()
            .find(|surface| surface.terminal_handle.as_deref() == Some(&terminal.handle))
        {
            let id = surface.id;
            return OpenedPanel {
                panels: self.activate(key, id).panels,
                id,
            };
        }
        let mut opened = self.open(key.clone(), PanelKind::Terminal, Some(terminal.cwd));
        Arc::make_mut(&mut opened.panels)
            .threads
            .get_mut(&key)
            .unwrap()
            .surfaces
            .iter_mut()
            .find(|surface| surface.id == opened.id)
            .unwrap()
            .terminal_handle = Some(terminal.handle);
        opened
    }
    pub fn close(self: Arc<Self>, key: PanelScope, id: u64) -> Arc<Self> {
        let Some(index) = self
            .threads
            .get(&key)
            .and_then(|panel| panel.surfaces.iter().position(|surface| surface.id == id))
        else {
            return self;
        };
        let mut next = self.as_ref().clone();
        let panel = next.threads.get_mut(&key).unwrap();
        panel.surfaces.remove(index);
        if panel.active == Some(id) {
            panel.active = panel
                .surfaces
                .get(index.min(panel.surfaces.len().saturating_sub(1)))
                .map(|surface| surface.id);
        }
        Arc::new(next)
    }
    pub fn set_open(self: Arc<Self>, key: PanelScope, open: bool) -> Arc<Self> {
        if self.get(key.clone()).open == open {
            return self;
        }
        let mut next = self.as_ref().clone();
        next.threads.entry(key).or_default().open = open;
        Arc::new(next)
    }
    pub fn set_maximized(self: Arc<Self>, key: PanelScope, maximized: bool) -> Arc<Self> {
        if self.get(key.clone()).maximized == maximized {
            return self;
        }
        let mut next = self.as_ref().clone();
        next.threads.entry(key).or_default().maximized = maximized;
        Arc::new(next)
    }
    /// A new draft keeps the panel visible without inheriting another
    /// conversation's tools. Returning to a visited draft keeps its own choice.
    pub fn inherit_visibility(
        self: Arc<Self>,
        before: PanelScope,
        after: PanelScope,
        new_draft: bool,
        welcome: bool,
    ) -> Arc<Self> {
        if before == after || before.host != after.host || before.storage != after.storage {
            return self;
        }
        // The welcome placeholder's first edit moves its tools to the new draft.
        if welcome {
            let mut next = self.as_ref().clone();
            if let Some(source) = next.threads.remove(&before) {
                let target = next.threads.entry(after).or_default();
                target.surfaces.extend(source.surfaces);
                target.active = source.active.or(target.active);
                target.open |= source.open;
                target.maximized |= source.maximized;
            }
            return Arc::new(next);
        }
        if !new_draft || self.threads.contains_key(&after) {
            return self;
        }
        let open = self.get(before).open;
        let mut next = self.as_ref().clone();
        next.threads.insert(
            after,
            PanelState {
                open,
                ..Default::default()
            },
        );
        Arc::new(next)
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    pub fn panel_scope(&self, host: String, draft: crate::state::DraftKey) -> PanelScope {
        PanelScope {
            host,
            storage: self.connection.storage_scope.clone(),
            draft,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn open(
        panels: &mut Arc<Panels>,
        key: PanelScope,
        kind: PanelKind,
        path: Option<String>,
    ) -> u64 {
        let opened = panels.clone().open(key, kind, path);
        *panels = opened.panels;
        opened.id
    }
    fn activate(panels: &mut Arc<Panels>, key: PanelScope, id: u64) -> bool {
        let activated = panels.clone().activate(key, id);
        *panels = activated.panels;
        activated.activated
    }

    fn scope(draft: impl Into<DraftKey>) -> PanelScope {
        PanelScope {
            host: "host".into(),
            storage: "storage".into(),
            draft: draft.into(),
        }
    }
    #[test]
    fn scripts_open_independent_terminals_and_preserve_the_shell() {
        let key = scope("chat");
        let shell = Panels::new().open(key.clone(), PanelKind::Terminal, Some("/project".into()));
        let first =
            shell
                .panels
                .clone()
                .open_script(key.clone(), "/project".into(), "npm test".into());
        let second =
            first
                .panels
                .clone()
                .open_script(key.clone(), "/project".into(), "npm test".into());
        let panel = second.panels.get(key.clone());
        assert_eq!(panel.surfaces.len(), 3);
        assert_eq!(panel.surfaces[0].command, None);
        assert_eq!(panel.surfaces[1].command.as_deref(), Some("npm test"));
        assert_eq!(panel.selected().unwrap().id, second.id);
        assert_ne!(first.id, second.id);
        assert_eq!(second.panels.scope(second.id), Some(key));
        assert_eq!(shell.panels.get(scope("chat")).surfaces.len(), 1);
        assert!(second.panels.get(scope("other")).surfaces.is_empty());
    }

    #[test]
    fn a_new_conversation_starts_at_the_launcher_without_starting_a_tool() {
        let mut panels = Panels::new();
        let key = scope("new");
        panels = panels.clone().set_open(key.clone(), true);
        let empty = panels.get(key.clone());
        assert!(empty.open);
        assert!(empty.surfaces.is_empty());
        assert_eq!(empty.kind(), PanelKind::Home);
        assert_eq!(empty.active, None);
        let terminal = open(&mut panels, key.clone(), PanelKind::Terminal, None);
        assert!(
            empty.surfaces.is_empty(),
            "Reading state must return an immutable snapshot"
        );
        assert_eq!(panels.get(key).active, Some(terminal));
    }

    #[test]
    fn new_drafts_keep_panel_visibility_and_visited_drafts_keep_their_choice() {
        let mut panels = Panels::new();
        let original = scope("original");
        let draft = scope("new");
        let terminal = open(&mut panels, original.clone(), PanelKind::Terminal, None);
        panels = panels.clone().set_maximized(original.clone(), true);
        let previous = panels.get(original.clone());
        panels = panels.inherit_visibility(original.clone(), draft.clone(), true, false);
        assert_eq!(
            panels.get(draft.clone()),
            PanelState {
                open: true,
                ..Default::default()
            }
        );
        assert_eq!(panels.get(original.clone()), previous);
        assert_eq!(panels.scope(terminal), Some(original.clone()));
        panels = panels.set_open(draft.clone(), false);
        panels = panels.inherit_visibility(original.clone(), draft.clone(), true, false);
        assert!(!panels.get(draft.clone()).open);

        let another = scope("another");
        panels = panels.inherit_visibility(draft.clone(), another.clone(), true, false);
        panels = panels.inherit_visibility(original.clone(), another.clone(), true, false);
        assert!(
            !panels.get(another).open,
            "a visited closed draft stays closed"
        );
        panels = panels.inherit_visibility(original.clone(), original.clone(), true, false);
        assert_eq!(panels.get(original.clone()), previous);

        for destination in [
            scope(crate::session::SessionRef {
                provider: Some(crate::session::ProviderKind::Codex),
                id: "existing".into(),
            }),
            PanelScope {
                host: "other".into(),
                ..scope("new-host")
            },
            PanelScope {
                storage: "replacement".into(),
                ..scope("new-storage")
            },
        ] {
            panels = panels.inherit_visibility(
                original.clone(),
                destination.clone(),
                matches!(destination.draft, DraftKey::Local { .. }),
                false,
            );
            assert_eq!(panels.get(destination), PanelState::default());
        }
    }

    #[test]
    fn the_launcher_and_add_menu_share_order_titles_shortcuts_and_availability() {
        for connected in [false, true] {
            for has_directory in [false, true] {
                let tools = panel_tools(connected, has_directory);
                assert_eq!(
                    tools
                        .iter()
                        .map(|tool| tool.key.as_str())
                        .collect::<Vec<_>>(),
                    ["browser", "terminal", "files", "diff", "sideChat", "agents"]
                );
                assert_eq!(
                    tools
                        .iter()
                        .map(|tool| tool.shortcut.as_str())
                        .collect::<Vec<_>>(),
                    ["B", "T", "F", "D", "S", "A"]
                );
                for tool in tools {
                    let available = match tool.kind {
                        PanelKind::Browser | PanelKind::Agents => true,
                        _ => connected && has_directory,
                    };
                    assert_eq!(tool.enabled, available, "{}", tool.key);
                    assert_eq!(panel_title(tool.kind), tool.label);
                }
            }
        }
        assert_eq!(panel_title(PanelKind::Home), "機能を開く");
        assert_eq!(panel_title(PanelKind::File), "ファイル");
    }

    #[test]
    fn the_same_session_id_on_different_hosts_and_storage_scopes_has_independent_panels() {
        let mut panels = Panels::new();
        let a = scope("session");
        let b = PanelScope {
            host: "other-host".into(),
            ..a.clone()
        };
        let c = PanelScope {
            storage: "replacement-host-storage".into(),
            ..a.clone()
        };
        let browser = open(&mut panels, a.clone(), PanelKind::Browser, None);
        assert!(panels.get(b.clone()).surfaces.is_empty());
        assert!(panels.get(c.clone()).surfaces.is_empty());
        open(&mut panels, b.clone(), PanelKind::Terminal, None);
        assert_eq!(panels.get(a.clone()).active, Some(browser));
        assert_ne!(panels.get(a.clone()).active, panels.get(b.clone()).active);
        assert!(!activate(&mut panels, b.clone(), browser));
    }
    #[test]
    fn explorer_navigation_reuses_its_surface_and_invalid_closes_leave_the_selection_alone() {
        let mut panels = Panels::new();
        let key = scope("a");
        let directory = open(
            &mut panels,
            key.clone(),
            PanelKind::Files,
            Some("/project".into()),
        );
        assert_eq!(
            open(
                &mut panels,
                key.clone(),
                PanelKind::Files,
                Some("/project/src".into())
            ),
            directory
        );
        assert_eq!(
            panels.get(key.clone()).selected().unwrap().path.as_deref(),
            Some("/project/src")
        );
        assert_eq!(
            open(&mut panels, key.clone(), PanelKind::Files, None),
            directory
        );
        assert_eq!(
            panels.get(key.clone()).selected().unwrap().path.as_deref(),
            Some("/project/src")
        );
        let browser = open(&mut panels, key.clone(), PanelKind::Browser, None);
        panels = panels.clone().close(key.clone(), directory);
        assert_eq!(panels.get(key.clone()).active, Some(browser));
        let before = panels.get(key.clone());
        panels = panels.clone().close(key.clone(), u64::MAX);
        panels = panels.clone().close(scope("missing"), browser);
        assert_eq!(panels.get(key.clone()), before);
        assert!(!activate(&mut panels, scope("missing"), browser));
        assert_eq!(panels.scope(u64::MAX), None);
    }

    proptest::proptest! {
        #[test]
        fn changes_to_one_conversation_never_change_another(actions in proptest::collection::vec((0u8..5, 0u8..20), 0..100)) {
            let mut panels = Panels::new();
            let a = scope("a");
            let b = scope("b");
            open(&mut panels, b.clone(), PanelKind::Terminal, None);
            let expected = panels.get(b.clone());
            for (action, value) in actions {
                match action {
                    0 => { open(&mut panels, a.clone(), PanelKind::Browser, None); }
                    1 => panels = panels.clone().set_open(a.clone(), value % 2 == 0),
                    2 => panels = panels.clone().close(a.clone(), u64::from(value)),
                    3 => { activate(&mut panels, a.clone(), u64::from(value)); }
                    _ => panels = panels.clone().set_maximized(a.clone(), value % 2 == 0),
                }
                proptest::prop_assert_eq!(panels.get(b.clone()), expected.clone());
                let panel = panels.get(a.clone());
                proptest::prop_assert_eq!(panel.active.is_none(), panel.surfaces.is_empty());
                if let Some(id) = panel.active { proptest::prop_assert!(panel.surfaces.iter().any(|s| s.id == id)); }
            }
        }
    }
    #[test]
    fn restoring_a_maximized_panel_returns_to_the_state_before_it() {
        let key = scope("chat");
        let panels = Panels::new().set_open(key.clone(), true);
        let maximized = panels.clone().set_maximized(key.clone(), true);
        assert!(maximized.get(key.clone()).maximized);
        assert_eq!(
            maximized.set_maximized(key.clone(), false).get(key.clone()),
            panels.get(key)
        );
    }

    #[test]
    fn conversations_keep_their_surfaces_selection_visibility_and_size() {
        let mut panels = Panels::new();
        let a = scope("draft-a");
        let b = scope("draft-b");
        let browser = open(&mut panels, a.clone(), PanelKind::Browser, None);
        let terminal = open(&mut panels, a.clone(), PanelKind::Terminal, None);
        panels = panels.clone().set_maximized(a.clone(), true);
        panels = panels.clone().set_open(a.clone(), false);
        let other = open(&mut panels, b.clone(), PanelKind::SideChat, None);
        assert_eq!(panels.get(a.clone()).active, Some(terminal));
        assert!(panels.get(a.clone()).maximized);
        assert!(!panels.get(a.clone()).open);
        assert_eq!(panels.get(b.clone()).active, Some(other));
        assert!(!activate(&mut panels, b.clone(), browser));
        assert_eq!(panels.get(b.clone()).surfaces.len(), 1);
        assert!(activate(&mut panels, a.clone(), browser));
        assert!(panels.get(a.clone()).open);
        assert_eq!(panels.get(a.clone()).surfaces.len(), 2);
    }
    #[test]
    fn closing_a_surface_selects_its_neighbor_and_last_close_keeps_the_launcher_open() {
        let mut panels = Panels::new();
        let key = scope("a");
        let a = open(&mut panels, key.clone(), PanelKind::Terminal, None);
        let b = open(&mut panels, key.clone(), PanelKind::Terminal, None);
        let c = open(&mut panels, key.clone(), PanelKind::Browser, None);
        activate(&mut panels, key.clone(), b);
        panels = panels.clone().close(key.clone(), b);
        assert_eq!(panels.get(key.clone()).active, Some(c));
        panels = panels.clone().close(key.clone(), c);
        assert_eq!(panels.get(key.clone()).active, Some(a));
        panels = panels.clone().close(key.clone(), a);
        assert!(panels.get(key.clone()).open);
        assert_eq!(panels.get(key.clone()).kind(), PanelKind::Home);
    }
    #[test]
    fn files_deduplicate_by_path_with_a_stable_conversation_address() {
        let mut panels = Panels::new();
        let draft = scope(crate::session::SessionRef {
            provider: None,
            id: uuid::Uuid::new_v4().to_string(),
        });
        let file = open(
            &mut panels,
            draft.clone(),
            PanelKind::File,
            Some("/a".into()),
        );
        assert_eq!(
            open(
                &mut panels,
                draft.clone(),
                PanelKind::File,
                Some("/a".into())
            ),
            file
        );
        assert_ne!(
            open(
                &mut panels,
                draft.clone(),
                PanelKind::File,
                Some("/b".into())
            ),
            file
        );
        assert_eq!(panels.get(draft.clone()).surfaces.len(), 2);
        assert_eq!(panels.scope(file), Some(draft));
    }
}

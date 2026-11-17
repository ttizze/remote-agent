//! Terminal display reads only terminal resources, independent of chat updates.
use crate::localization::{UiLanguage, localized_ui_format};
use crate::state::{Terminal, TerminalPhase, TerminalView};
use std::{collections::BTreeMap, sync::Arc};

/// ChatTerminalDrawers keeps ten recently visited open drawers hidden, in
/// visit order. The active native frame also contains the conversation slot,
/// so it exists even before that conversation opens a terminal.
pub fn mounted_terminal_scopes(
    previous: &[crate::panels::PanelScope],
    open: &[crate::panels::PanelScope],
    active: &crate::panels::PanelScope,
) -> Vec<crate::panels::PanelScope> {
    let mut next: Vec<_> = previous
        .iter()
        .filter(|scope| *scope != active && open.contains(scope))
        .cloned()
        .collect();
    next.drain(..next.len().saturating_sub(10));
    next.push(active.clone());
    next
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalCloseConfirmation {
    pub title: String,
    pub description: String,
    pub cancel_label: String,
    pub confirm_label: String,
}

/// terminalCloseConfirm's single-resource prompt. The UI captures the
/// resource separately, so the answer cannot resolve against a later tab.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn terminal_close_confirmation(
    language: UiLanguage,
    label: String,
) -> TerminalCloseConfirmation {
    TerminalCloseConfirmation {
        title: localized_ui_format(
            language,
            "ターミナル「{0}」を閉じますか？".into(),
            vec![label],
        ),
        description: language
            .text("実行中のプロセスを停止し、このターミナルの履歴を消去します。")
            .into(),
        cancel_label: language.text("キャンセル").into(),
        confirm_label: language.text("閉じる").into(),
    }
}

/// Native views can briefly report a zero grid while being laid out.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn terminal_grid(cols: i32, rows: i32) -> Option<agent_protocol::operations::TerminalSize> {
    if cols <= 0 || rows <= 0 {
        return None;
    }
    Some(agent_protocol::operations::TerminalSize {
        cols: cols.min(500) as u16,
        rows: rows.min(250) as u16,
    })
}

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct Terminals {
    pub(crate) states: Arc<BTreeMap<String, Arc<Terminal>>>,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Terminals {
    #[cfg_attr(feature = "bindings", uniffi::constructor)]
    pub fn empty() -> Arc<Self> {
        Arc::new(Self {
            states: Arc::default(),
        })
    }
    pub fn view(&self, language: UiLanguage, handle: String) -> Option<TerminalView> {
        self.states
            .get(&handle)
            .map(|terminal| view(language, terminal))
    }
}

pub fn terminal_status(language: UiLanguage, phase: &TerminalPhase) -> String {
    match phase {
        TerminalPhase::Starting => language.text("起動中").into(),
        TerminalPhase::Running => language.text("実行中").into(),
        TerminalPhase::Suspended => language.text("再接続を待っています").into(),
        TerminalPhase::Detached => language.text("切断済み").into(),
        TerminalPhase::Exited(code) => {
            localized_ui_format(language, "終了 · {0}".into(), vec![code.to_string()])
        }
        TerminalPhase::Failed(error) => super::error::error_text(language, error).to_string(),
    }
}

pub fn view(language: UiLanguage, terminal: &Terminal) -> TerminalView {
    TerminalView {
        status: match terminal.phase {
            TerminalPhase::Starting | TerminalPhase::Running => None,
            _ => Some(terminal_status(language, &terminal.phase)),
        },
        loading: matches!(terminal.phase, TerminalPhase::Starting),
        accepts_input: terminal.phase.accepts_input(),
        output: terminal
            .output
            .iter()
            .map(|chunk| (**chunk).clone())
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Snapshot, TerminalOutput};

    #[test]
    fn mounted_drawers_keep_ten_recent_visits_and_move_a_revisited_scope_to_the_end() {
        let scope = |index| crate::panels::PanelScope {
            host: "host".into(),
            storage: "account".into(),
            draft: format!("conversation-{index}").into(),
        };
        let open: Vec<_> = (0..=12).map(scope).collect();
        let previous: Vec<_> = (0..=10).map(scope).collect();
        let mounted = mounted_terminal_scopes(&previous, &open, &scope(11));
        assert_eq!(mounted, (1..=11).map(scope).collect::<Vec<_>>());
        let revisited = mounted_terminal_scopes(&mounted, &open, &scope(1));
        assert_eq!(
            revisited,
            (2..=11).chain([1]).map(scope).collect::<Vec<_>>()
        );
        let next = mounted_terminal_scopes(&revisited, &open, &scope(12));
        assert_eq!(next, (3..=11).chain([1, 12]).map(scope).collect::<Vec<_>>());
        assert_eq!(next.len(), 11, "ten hidden drawers plus the active frame");
    }

    #[test]
    fn mounted_drawers_discard_closed_scopes_and_do_not_mount_unvisited_ones() {
        let scope = |draft: &str| crate::panels::PanelScope {
            host: "host".into(),
            storage: "account".into(),
            draft: draft.into(),
        };
        let previous = vec![scope("closed"), scope("open")];
        let open = vec![scope("open"), scope("never-visited")];
        assert_eq!(
            mounted_terminal_scopes(&previous, &open, &scope("current")),
            [scope("open"), scope("current")],
        );
        let replacement = crate::panels::PanelScope {
            storage: "another-account".into(),
            ..scope("open")
        };
        assert_eq!(
            mounted_terminal_scopes(&previous, std::slice::from_ref(&replacement), &replacement),
            [replacement],
            "a matching draft name in another storage is a different resource owner",
        );
    }

    #[test]
    fn terminal_close_confirmation_names_the_resource_and_localizes_its_actions() {
        let prompt = terminal_close_confirmation(UiLanguage::English, "Terminal 2".into());
        assert_eq!(prompt.title, "Close terminal \"Terminal 2\"?");
        assert_eq!(
            prompt.description,
            "This stops the running process and clears its history."
        );
        assert_eq!(
            (prompt.cancel_label.as_str(), prompt.confirm_label.as_str()),
            ("Cancel", "Close")
        );
        let japanese = terminal_close_confirmation(UiLanguage::Japanese, "build".into());
        assert_eq!(japanese.title, "ターミナル「build」を閉じますか？");
        assert_eq!(
            (
                japanese.cancel_label.as_str(),
                japanese.confirm_label.as_str()
            ),
            ("キャンセル", "閉じる")
        );
    }

    #[test]
    fn native_grids_ignore_unlaid_out_views_and_fit_host_limits() {
        for (cols, rows) in [(0, 1), (1, 0), (-1, 1), (1, -1), (i32::MIN, i32::MAX)] {
            assert!(terminal_grid(cols, rows).is_none());
        }
        for (cols, rows, expected_cols, expected_rows) in [
            (1, 1, 1, 1),
            (500, 250, 500, 250),
            (501, 20, 500, 20),
            (80, 251, 80, 250),
            (i32::MAX, i32::MAX, 500, 250),
        ] {
            let size = terminal_grid(cols, rows).unwrap();
            assert_eq!((size.cols, size.rows), (expected_cols, expected_rows));
        }
    }
    #[test]
    fn terminal_resources_retain_their_output_and_state_across_other_updates() {
        let mut state = Snapshot::default();
        Arc::make_mut(&mut state.terminals).insert(
            "shell".into(),
            Arc::new(Terminal {
                target: crate::state::TerminalTarget::Shell {
                    command: None,
                    cwd: "/project".into(),
                },
                size: agent_protocol::operations::TerminalSize { cols: 80, rows: 24 },
                phase: TerminalPhase::Starting,
                output: [Arc::new(TerminalOutput {
                    sequence: 1,
                    data: b"ready\r\n".to_vec(),
                    reset_size: None,
                })]
                .into(),
                sequence: 1,
            }),
        );
        let before = state.terminals();
        assert!(
            before
                .view(UiLanguage::Japanese, "shell".into())
                .unwrap()
                .loading
        );
        assert!(state.terminal_accepts_input("shell".into()));
        assert!(
            before
                .view(UiLanguage::Japanese, "missing".into())
                .is_none()
        );
        Arc::make_mut(&mut state.input.drafts).insert(Default::default(), Arc::default());
        assert!(Arc::ptr_eq(&before.states, &state.terminals));
        Arc::make_mut(
            Arc::make_mut(&mut state.terminals)
                .get_mut("shell")
                .unwrap(),
        )
        .phase = TerminalPhase::Suspended;
        let suspended = state
            .terminals()
            .view(UiLanguage::Japanese, "shell".into())
            .unwrap();
        assert!(!suspended.loading && !suspended.accepts_input);
        assert!(!state.terminal_accepts_input("shell".into()));
        assert_eq!(suspended.status.as_deref(), Some("再接続を待っています"));
        assert_eq!(suspended.output[0].data, b"ready\r\n");
        let english = state
            .terminals()
            .view(UiLanguage::English, "shell".into())
            .unwrap();
        assert_eq!(english.status.as_deref(), Some("Waiting to reconnect"));
        assert_eq!(english.output[0].data, b"ready\r\n");
        let retained = before.view(UiLanguage::Japanese, "shell".into()).unwrap();
        assert!(retained.loading && retained.accepts_input);
        assert_eq!(retained.output[0].sequence, 1);
    }
    #[test]
    fn terminal_status_formats_phase_facts_without_changing_output() {
        for (phase, japanese, english) in [
            (TerminalPhase::Starting, "起動中", "Starting"),
            (TerminalPhase::Running, "実行中", "Running"),
            (
                TerminalPhase::Suspended,
                "再接続を待っています",
                "Waiting to reconnect",
            ),
            (TerminalPhase::Detached, "切断済み", "Disconnected"),
            (TerminalPhase::Exited(-1), "終了 · -1", "Exited · -1"),
            (
                TerminalPhase::Failed("request 42 timed out".into()),
                "ホストから応答がありませんでした。\nrequest 42 timed out",
                "The Host did not respond.\nrequest 42 timed out",
            ),
        ] {
            assert_eq!(terminal_status(UiLanguage::Japanese, &phase), japanese);
            assert_eq!(terminal_status(UiLanguage::English, &phase), english);
        }
    }
}

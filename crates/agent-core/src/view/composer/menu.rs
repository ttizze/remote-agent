//! The composer's `/`, `$` and `@` menu for the current draft.
use super::commands::{
    ComposerCommandItem, ComposerCommandMenuInput, ComposerTrigger, ComposerTriggerKind,
    composer_command_items, detect_composer_trigger, has_compactable_conversation,
};
use crate::state::Snapshot;
use agent_domain::ThreadShell;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ComposerMenuView {
    pub trigger: Option<ComposerTrigger>,
    pub items: Vec<ComposerCommandItem>,
    /// What the open menu says when nothing matches.
    pub empty_label: Option<String>,
}

/// The empty menu's text for a trigger. The Host looks up no pull requests,
/// so that trigger never has a project to search.
pub fn composer_menu_empty_label(kind: ComposerTriggerKind) -> &'static str {
    match kind {
        ComposerTriggerKind::Skill => "No skills found. Try / to browse provider commands.",
        ComposerTriggerKind::Path => "No matching files or folders.",
        ComposerTriggerKind::PullRequest => "Pull requests are not available for this project.",
        ComposerTriggerKind::SlashCommand | ComposerTriggerKind::SlashModel => {
            "No matching command."
        }
    }
}

/// The menu for `text` with the cursor at `cursor` (UTF-16). The Host lists
/// no provider skills, slash commands or workspace paths yet.
pub fn composer_menu(snapshot: &Snapshot, text: &str, cursor: u32) -> ComposerMenuView {
    let Some(trigger) = detect_composer_trigger(text, cursor) else {
        return ComposerMenuView::default();
    };
    let items = composer_menu_items(snapshot, &trigger);
    ComposerMenuView {
        empty_label: items
            .is_empty()
            .then(|| composer_menu_empty_label(trigger.kind).into()),
        trigger: Some(trigger),
        items,
    }
}

pub(crate) fn composer_menu_items(
    snapshot: &Snapshot,
    trigger: &ComposerTrigger,
) -> Vec<ComposerCommandItem> {
    let draft = snapshot.current_draft();
    let thread = snapshot.selected_thread.as_ref();
    let compactable = thread.is_some_and(|id| {
        let sync = snapshot.thread(id);
        let latest_user_message_at = snapshot
            .thread_row(id)
            .and_then(|row| row.latest_user_message_at.as_ref());
        sync.and_then(|sync| sync.state.as_deref())
            .is_some_and(|state| {
                has_compactable_conversation(
                    state,
                    sync.is_some_and(|sync| sync.history.has_more),
                    latest_user_message_at,
                )
            })
    });
    let threads: Vec<ThreadShell> = snapshot
        .shell_view()
        .map(|shell| shell.threads.clone())
        .unwrap_or_default();
    composer_command_items(&ComposerCommandMenuInput {
        trigger,
        driver: (!draft.instance_id.is_empty()).then_some(draft.driver),
        has_thread: thread.is_some(),
        has_compactable_conversation: compactable,
        allow_interaction_mode: true,
        skills: &[],
        slash_commands: &[],
        path_entries: &[],
        threads: &threads,
        current_thread: thread.map(|id| id.as_str()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_menu_names_what_its_trigger_searched() {
        let snapshot = Snapshot::default();
        let path = composer_menu(&snapshot, "see @nothing-here", 17);
        assert_eq!(
            path.trigger.map(|trigger| trigger.kind),
            Some(ComposerTriggerKind::Path)
        );
        assert!(path.items.is_empty());
        assert_eq!(
            path.empty_label.as_deref(),
            Some("No matching files or folders.")
        );
        assert_eq!(
            composer_menu_empty_label(ComposerTriggerKind::Skill),
            "No skills found. Try / to browse provider commands."
        );
        assert_eq!(
            composer_menu_empty_label(ComposerTriggerKind::SlashCommand),
            "No matching command."
        );
        assert_eq!(
            composer_menu(&snapshot, "plain text", 10),
            ComposerMenuView::default()
        );
    }

    #[test]
    fn a_menu_with_items_has_no_empty_label() {
        let menu = composer_menu(&Snapshot::default(), "/", 1);
        assert!(!menu.items.is_empty());
        assert_eq!(menu.empty_label, None);
    }
}

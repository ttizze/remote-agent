//! The new-thread screen: the hero headline over the draft's composer.
use crate::state::Snapshot;
use crate::view::composer::hero::{DraftHeroHeadline, DraftHeroInput, draft_hero_state};
use crate::view::composer::view::{ComposerOptions, ComposerView, composer_view};

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct NewThreadView {
    pub hero: DraftHeroHeadline,
    /// The composer sits centred under the headline.
    pub show_hero: bool,
    pub composer: ComposerView,
    pub project_id: Option<String>,
    /// The thread this draft's send is creating; it opens once the Host has it.
    pub launching_thread_id: Option<String>,
}

pub fn new_thread_view(snapshot: &Snapshot, options: &ComposerOptions) -> NewThreadView {
    let key = snapshot.new_thread_draft_key();
    let launching = snapshot
        .outbox
        .pending_launches()
        .find(|entry| {
            entry.navigate
                && entry
                    .restore
                    .as_ref()
                    .is_some_and(|restore| restore.draft_key == key)
        })
        .map(|entry| entry.thread.to_string());
    NewThreadView {
        hero: snapshot.draft_hero_headline(),
        show_hero: draft_hero_state(DraftHeroInput {
            is_draft: true,
            dock_requested: launching.is_some(),
            ..DraftHeroInput::default()
        }),
        composer: composer_view(snapshot, None, options),
        project_id: snapshot.selected_project.clone(),
        launching_thread_id: launching,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Project, ProjectRoot};
    use crate::view::composer::hero::DraftHeroHeadlineKind;
    use crate::view::composer::prompt::DEFAULT_COMPOSER_PLACEHOLDER;

    #[test]
    fn a_new_thread_draft_shows_the_hero_over_its_composer() {
        let mut snapshot = Snapshot {
            selected_project: Some("app".into()),
            ..Snapshot::default()
        };
        snapshot.shell = std::sync::Arc::new(crate::sync::ShellCache::from_cache(
            agent_protocol::conversation::ShellSnapshot {
                snapshot_sequence: 1,
                projects: vec![Project {
                    id: "app".into(),
                    name: "App".into(),
                    roots: vec![ProjectRoot {
                        path: "/repo".into(),
                    }],
                    scripts: vec![],
                    repository_identity: None,
                    favicon_path: None,
                    created_at: None,
                    updated_at: None,
                }],
                threads: vec![],
            },
        ));
        snapshot.drafts.insert(
            "new:app".into(),
            crate::state::Draft {
                text: "Sketch the API".into(),
                ..Default::default()
            },
        );
        let view = new_thread_view(&snapshot, &ComposerOptions::default());
        assert!(view.show_hero);
        assert_eq!(view.hero.kind, DraftHeroHeadlineKind::BuildIn);
        assert_eq!(view.hero.heading_label, "What should we build in App?");
        assert_eq!(view.project_id.as_deref(), Some("app"));
        assert_eq!(view.composer.draft_key, "new:app");
        assert_eq!(view.composer.text, "Sketch the API");
        assert_eq!(
            view.composer.editor.placeholder,
            DEFAULT_COMPOSER_PLACEHOLDER
        );
        assert_eq!(view.composer.context_meter, None);
        assert_eq!(view.launching_thread_id, None);
    }
}

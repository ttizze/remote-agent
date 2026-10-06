//! The new-thread draft's hero layout: the composer centred under a headline
//! until the first send docks it to the bottom.
use crate::models::Project;
use crate::state::{CHATS_PROJECT, Snapshot};

/// How a send was triggered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SubmissionIntent {
    Foreground,
    /// Start the thread and stay on the draft.
    Background,
    /// The other follow-up of a running turn.
    Alternate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DraftHeroInput {
    /// The view shows a new-thread draft, not a Host thread.
    pub is_draft: bool,
    pub has_timeline_entries: bool,
    pub is_working: bool,
    /// The composer docked for a foreground send of this draft.
    pub dock_requested: bool,
    pub background_submission_pending: bool,
    /// A worktree setup card is on the timeline, which must stay visible.
    pub has_worktree_setup_card: bool,
}

/// Whether the composer sits in the hero layout.
pub fn draft_hero_state(input: DraftHeroInput) -> bool {
    if input.has_worktree_setup_card {
        return false;
    }
    if input.background_submission_pending {
        return true;
    }
    input.is_draft && !input.has_timeline_entries && !input.is_working && !input.dock_requested
}

/// A foreground send docks the hero composer before the thread starts; a
/// background send keeps the hero for the next draft.
pub fn dock_draft_hero_for_submission(
    hero: bool,
    has_active_draft: bool,
    intent: SubmissionIntent,
) -> bool {
    intent == SubmissionIntent::Foreground && hero && has_active_draft
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum DraftHeroHeadlineKind {
    /// "What should we work on?" for a draft without a project.
    WorkOn,
    /// "What should we build in {project}?"
    BuildIn,
    /// "{picker} to start" before a project is chosen.
    ChooseProject,
    /// "Add a project to start" when there is none.
    AddProject,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DraftHeroProjectChoice {
    pub project_id: String,
    pub name: String,
    pub selected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DraftHeroHeadline {
    pub kind: DraftHeroHeadlineKind,
    /// The heading as one sentence, for accessibility.
    pub heading_label: String,
    /// The project picker's text, or the add-project button's when there is no menu.
    pub project_label: String,
    pub project_tooltip: Option<String>,
    /// The picker opens a project menu; otherwise it adds a project.
    pub project_menu: bool,
    /// The menu's "No project" entry, when threads can skip a project.
    pub no_project_choice: bool,
    pub no_project_selected: bool,
    pub project_choices: Vec<DraftHeroProjectChoice>,
    /// The second line holds the picker instead of the heading.
    pub picker_below_heading: bool,
    /// The second line offers "or start without a project".
    pub start_without_project: bool,
    /// Threads can skip a project, so the second line is reserved.
    pub reserve_second_line: bool,
}

/// The headline above a new-thread draft whose target is `selected_project`
/// (none means the chats project).
pub fn draft_hero_headline(
    projects: &[Project],
    selected_project: Option<&str>,
) -> DraftHeroHeadline {
    let target = selected_project.unwrap_or(CHATS_PROJECT);
    let active = projects.iter().find(|project| project.id == target);
    let scratch_available = projects.iter().any(|project| project.id == CHATS_PROJECT);
    let scratch = active.is_some_and(|project| project.id == CHATS_PROJECT);
    let title = active.map(|project| project.name.clone());
    let can_choose = !projects.is_empty();
    let (kind, heading_label) = if scratch {
        (
            DraftHeroHeadlineKind::WorkOn,
            "What should we work on?".to_owned(),
        )
    } else if let Some(title) = &title {
        (
            DraftHeroHeadlineKind::BuildIn,
            format!("What should we build in {title}?"),
        )
    } else if can_choose {
        (
            DraftHeroHeadlineKind::ChooseProject,
            "Choose a project to start".to_owned(),
        )
    } else {
        (
            DraftHeroHeadlineKind::AddProject,
            "Add a project to start".to_owned(),
        )
    };
    let project_label = if !can_choose {
        title.clone().unwrap_or_else(|| "Add a project".into())
    } else if scratch {
        "No project".into()
    } else {
        title.clone().unwrap_or_else(|| "Choose a project".into())
    };
    DraftHeroHeadline {
        kind,
        heading_label,
        project_label,
        project_tooltip: title.filter(|_| !scratch && can_choose),
        project_menu: can_choose,
        no_project_choice: scratch_available,
        no_project_selected: scratch,
        project_choices: projects
            .iter()
            .filter(|project| project.id != CHATS_PROJECT)
            .map(|project| DraftHeroProjectChoice {
                project_id: project.id.clone(),
                name: project.name.clone(),
                selected: !scratch && project.id == target,
            })
            .collect(),
        picker_below_heading: scratch,
        start_without_project: scratch_available && !scratch && (active.is_some() || can_choose),
        reserve_second_line: scratch_available,
    }
}

impl Snapshot {
    /// The headline above the current new-thread draft.
    pub fn draft_hero_headline(&self) -> DraftHeroHeadline {
        draft_hero_headline(self.shell_projects(), self.selected_project.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ProjectRoot;
    use rstest::rstest;

    fn input() -> DraftHeroInput {
        DraftHeroInput {
            is_draft: true,
            ..DraftHeroInput::default()
        }
    }

    #[test]
    fn does_not_dock_the_composer_before_a_background_submission() {
        assert!(!dock_draft_hero_for_submission(
            true,
            true,
            SubmissionIntent::Background
        ));
        assert!(dock_draft_hero_for_submission(
            true,
            true,
            SubmissionIntent::Foreground
        ));
    }

    #[test]
    fn leaves_the_hero_layout_while_a_worktree_setup_card_is_on_the_timeline() {
        assert!(!draft_hero_state(DraftHeroInput {
            has_worktree_setup_card: true,
            ..input()
        }));
        // A background submission normally pins the hero, but never over the card.
        assert!(!draft_hero_state(DraftHeroInput {
            has_worktree_setup_card: true,
            background_submission_pending: true,
            ..input()
        }));
    }

    #[test]
    fn keeps_the_composer_in_the_hero_layout_until_navigation_after_server_promotion() {
        assert!(draft_hero_state(DraftHeroInput {
            is_draft: false,
            has_timeline_entries: true,
            is_working: true,
            dock_requested: false,
            background_submission_pending: true,
            has_worktree_setup_card: false,
        }));
    }

    #[rstest]
    #[case::fresh_draft(input(), true)]
    #[case::docked(DraftHeroInput { dock_requested: true, ..input() }, false)]
    #[case::working(DraftHeroInput { is_working: true, ..input() }, false)]
    #[case::with_timeline(DraftHeroInput { has_timeline_entries: true, ..input() }, false)]
    #[case::host_thread(DraftHeroInput { is_draft: false, ..input() }, false)]
    fn shows_the_hero_only_for_an_untouched_draft(
        #[case] input: DraftHeroInput,
        #[case] hero: bool,
    ) {
        assert_eq!(draft_hero_state(input), hero);
    }

    fn project(id: &str, name: &str) -> Project {
        Project {
            id: id.into(),
            name: name.into(),
            roots: vec![ProjectRoot {
                path: format!("/workspace/{id}"),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn asks_what_to_build_in_the_selected_project() {
        let projects = [project("chats", "Chats"), project("app", "App")];
        let headline = draft_hero_headline(&projects, Some("app"));
        assert_eq!(headline.kind, DraftHeroHeadlineKind::BuildIn);
        assert_eq!(headline.heading_label, "What should we build in App?");
        assert_eq!(headline.project_label, "App");
        assert_eq!(headline.project_tooltip.as_deref(), Some("App"));
        assert!(headline.start_without_project);
        assert!(!headline.picker_below_heading);
        assert_eq!(
            headline.project_choices,
            vec![DraftHeroProjectChoice {
                project_id: "app".into(),
                name: "App".into(),
                selected: true,
            }]
        );
    }

    #[test]
    fn a_draft_without_a_project_asks_what_to_work_on_and_moves_the_picker_below() {
        let projects = [project("chats", "Chats"), project("app", "App")];
        let headline = draft_hero_headline(&projects, None);
        assert_eq!(headline.kind, DraftHeroHeadlineKind::WorkOn);
        assert_eq!(headline.heading_label, "What should we work on?");
        assert_eq!(headline.project_label, "No project");
        assert_eq!(headline.project_tooltip, None);
        assert!(headline.picker_below_heading);
        assert!(headline.no_project_selected);
        assert!(!headline.start_without_project);
        assert!(!headline.project_choices[0].selected);
    }

    #[test]
    fn asks_to_choose_or_add_a_project_when_the_target_is_unknown() {
        let choose = draft_hero_headline(&[project("app", "App")], Some("gone"));
        assert_eq!(choose.kind, DraftHeroHeadlineKind::ChooseProject);
        assert_eq!(choose.heading_label, "Choose a project to start");
        assert_eq!(choose.project_label, "Choose a project");
        assert!(!choose.reserve_second_line);
        assert!(!choose.start_without_project);

        let add = draft_hero_headline(&[], None);
        assert_eq!(add.kind, DraftHeroHeadlineKind::AddProject);
        assert_eq!(add.heading_label, "Add a project to start");
        assert_eq!(add.project_label, "Add a project");
        assert!(!add.project_menu);
    }
}

//! The conversation views apps render. Each getter calls one `view` function
//! with the device state the snapshot holds.
use crate::commands::build::FollowUpBehavior;
use crate::state::{DraftAttachment, Snapshot};
use crate::view::{
    archived::{ArchivedOptions, ArchivedView, archived_view},
    attachments::{AttachmentAdmission, AttachmentCandidate},
    checkpoints::{DiffPanelSelection, DiffPanelView, checkpoint_summaries, diff_panel},
    composer::{
        menu::{ComposerMenuView, composer_menu},
        stash::{StashEntryView, stash_menu},
        view::ComposerOptions,
    },
    models::{
        ordering::FavoriteModel,
        picker::{ModelPickerOptions, ModelPickerView, PickerRail, model_picker},
        traits::{TraitsView, traits},
    },
    new_thread::{NewThreadView, new_thread_view},
    projects::{
        import::{ImportToast, SessionImportView, session_import_view},
        scripts::{ProjectScriptsView, project_scripts},
    },
    search::{SearchOptions, SearchView, search_view},
    settings::{SettingsScope, SettingsView, settings_view},
    sidebar::{SidebarOptions, SidebarThreadDropPlan, SidebarView, plan_sidebar_drop, sidebar},
    snooze::{SnoozePreset, resolve_snooze_presets},
    terminals::{TerminalTab, TerminalView, terminal_tabs, terminal_view},
    thread::{ThreadView, ThreadViewOptions, selected_thread_view, thread_view},
    thread_list::{ThreadListHolds, ThreadListOptions, ThreadListView, thread_list},
    thread_menu::{ThreadMenuOptions, ThreadMenuView, thread_menu},
    time::TimestampFormat,
    timeline::rows::{TimelineRow, TimelineUpdate},
};
use agent_domain::ThreadId;
use chrono::{Local, TimeZone};

/// One provider instance's model order, by slug.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ModelOrder {
    pub instance_id: String,
    pub models: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct PreferencesView {
    pub timestamp_format: TimestampFormat,
    pub favorite_models: Vec<FavoriteModel>,
    pub model_order: Vec<ModelOrder>,
    /// The Working section (beta).
    pub working_section: bool,
    pub diff_ignore_whitespace: bool,
    pub follow_up: FollowUpBehavior,
}

impl Snapshot {
    fn sidebar_options(&self, options: SidebarOptions) -> SidebarOptions {
        SidebarOptions {
            working_section: self.preferences.working_section,
            ..options
        }
    }
}

fn thread(id: String) -> Option<ThreadId> {
    ThreadId::new(id).ok()
}

#[uniffi::export]
impl Snapshot {
    pub fn sidebar(&self, now_ms: i64, options: SidebarOptions) -> SidebarView {
        sidebar(
            self,
            now_ms,
            &self.sidebar_options(options),
            &self.inbox_returns,
        )
    }
    /// What dropping `active_key` on `over_id` does in the sidebar these options render.
    pub fn sidebar_drop(
        &self,
        now_ms: i64,
        options: SidebarOptions,
        active_key: String,
        over_id: String,
    ) -> SidebarThreadDropPlan {
        let view = self.sidebar(now_ms, options);
        plan_sidebar_drop(self, &view, &active_key, &over_id)
    }
    pub fn thread_list(&self, now_ms: i64, options: ThreadListOptions) -> ThreadListView {
        let options = ThreadListOptions {
            working_shelf_enabled: self.preferences.working_section,
            timestamp_format: self.preferences.timestamp_format,
            ..options
        };
        thread_list(
            self,
            now_ms,
            &options,
            ThreadListHolds {
                inbox_returns: Some(&self.inbox_returns),
                pending_order: self.thread_order.as_ref().map(|hold| &hold.order),
            },
        )
    }
    pub fn archived(&self, now_ms: i64, options: ArchivedOptions) -> ArchivedView {
        archived_view(self, now_ms, &options)
    }
    pub fn thread_menu(
        &self,
        thread_id: String,
        now_ms: i64,
        options: ThreadMenuOptions,
    ) -> Option<ThreadMenuView> {
        let options = ThreadMenuOptions {
            timestamp_format: self.preferences.timestamp_format,
            ..options
        };
        thread_menu(self, &thread_id, now_ms, &options)
    }
    #[uniffi::method(name = "thread")]
    pub fn thread_screen(
        &self,
        thread_id: String,
        now_ms: i64,
        options: ThreadViewOptions,
    ) -> Option<ThreadView> {
        thread_view(self, &thread(thread_id)?, now_ms, &options)
    }
    pub fn selected_thread(&self, now_ms: i64, options: ThreadViewOptions) -> Option<ThreadView> {
        selected_thread_view(self, now_ms, &options)
    }
    pub fn new_thread(&self, options: ComposerOptions) -> NewThreadView {
        new_thread_view(self, &options)
    }
    /// The `/`, `$` and `@` menu with the cursor at `cursor` (UTF-16).
    pub fn composer_menu(&self, text: String, cursor: u32) -> ComposerMenuView {
        composer_menu(self, &text, cursor)
    }
    pub fn search(&self, now_ms: i64, options: SearchOptions) -> SearchView {
        search_view(self, now_ms, &options)
    }
    pub fn settings(&self, scope: SettingsScope) -> SettingsView {
        settings_view(
            self,
            self.conversation_settings.as_ref(),
            &scope,
            self.preferences.timestamp_format,
        )
    }
    pub fn model_picker(&self, query: String, rail: Option<PickerRail>) -> ModelPickerView {
        model_picker(
            self,
            &self.current_draft(),
            &ModelPickerOptions {
                query,
                rail,
                favorites: self.preferences.favorite_models.clone(),
            },
        )
    }
    pub fn traits(&self) -> TraitsView {
        traits(self, &self.current_draft(), true)
    }
    pub fn terminals(&self, thread_id: String) -> Vec<TerminalTab> {
        thread(thread_id).map_or_else(Vec::new, |thread| terminal_tabs(self, &thread))
    }
    /// One terminal's output after sequence `after`.
    pub fn terminal(&self, thread_id: String, terminal_id: String, after: u64) -> TerminalView {
        match thread(thread_id) {
            Some(thread) => terminal_view(self, &thread, &terminal_id, after),
            None => TerminalView {
                status: None,
                loading: false,
                accepts_input: false,
                output: vec![],
            },
        }
    }
    pub fn diff(&self, thread_id: String) -> DiffPanelView {
        let thread = thread(thread_id);
        let selection = thread
            .as_ref()
            .and_then(|thread| self.diff_panels.get(thread))
            .cloned()
            .unwrap_or_else(DiffPanelSelection::default);
        let checkpoints = thread
            .as_ref()
            .and_then(|thread| self.thread_state(thread))
            .map(checkpoint_summaries)
            .unwrap_or_default();
        diff_panel(
            &checkpoints,
            &selection,
            self.preferences.diff_ignore_whitespace,
        )
    }
    pub fn project_scripts(&self, project_id: String) -> Option<ProjectScriptsView> {
        project_scripts(
            self,
            &project_id,
            self.preferences
                .last_run_scripts
                .get(&project_id)
                .map(String::as_str),
        )
    }
    pub fn session_import(&self, now_ms: i64) -> SessionImportView {
        let import = &self.session_import;
        session_import_view(
            import.scan.as_ref(),
            import.scan_pending,
            import.scan_error.as_deref(),
            import.selection.as_ref(),
            import.importing,
            now_ms,
        )
    }
    pub fn import_toast(&self) -> Option<ImportToast> {
        self.session_import.toast.clone()
    }
    pub fn stash(&self) -> Vec<StashEntryView> {
        stash_menu(&self.stash)
    }
    pub fn preferences(&self) -> PreferencesView {
        let preferences = &self.preferences;
        PreferencesView {
            timestamp_format: preferences.timestamp_format,
            favorite_models: preferences.favorite_models.clone(),
            model_order: preferences
                .model_order
                .iter()
                .map(|(instance_id, models)| ModelOrder {
                    instance_id: instance_id.clone(),
                    models: models.clone(),
                })
                .collect(),
            working_section: preferences.working_section,
            diff_ignore_whitespace: preferences.diff_ignore_whitespace,
            follow_up: self.follow_up,
        }
    }
    pub fn conversation_settings_loaded(&self) -> bool {
        self.conversation_settings.is_some()
    }
}

/// How to bring a list showing `previous` rows to `next`.
#[uniffi::export]
pub fn timeline_update(previous: Vec<TimelineRow>, next: Vec<TimelineRow>) -> TimelineUpdate {
    crate::view::timeline::rows::timeline_update(&previous, &next)
}

/// The draft key holding the files attached to one question's answer.
#[uniffi::export]
pub fn answer_draft_key(request_id: String, question_id: String) -> String {
    crate::state::answer_draft_key(&request_id, &question_id)
}

/// Which picked files join a draft holding `existing`; accepted images marked
/// `needs_compression` are downscaled before `Intent::AttachFiles`.
#[uniffi::export]
pub fn admit_attachments(
    existing: Vec<DraftAttachment>,
    candidates: Vec<AttachmentCandidate>,
) -> AttachmentAdmission {
    crate::view::attachments::admit_attachments(&existing, &candidates)
}

/// The snooze choices at `now_ms` in the device's time zone.
#[uniffi::export]
pub fn snooze_presets(now_ms: i64, format: TimestampFormat) -> Vec<SnoozePreset> {
    Local
        .timestamp_millis_opt(now_ms)
        .single()
        .map(|now| resolve_snooze_presets(&now, format))
        .unwrap_or_default()
}

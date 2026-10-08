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
        catalog,
        ordering::FavoriteModel,
        picker::{ModelPickerOptions, ModelPickerView, PickerRail, model_picker},
        staging::{self, StagedModel},
        traits::{TraitsView, traits},
    },
    new_thread::{NewThreadView, new_thread_view},
    projects::{
        add::{AddProjectTarget, FolderBrowserView, add_project_target, folder_browser},
        import::{ImportToast, SessionImportView, session_import_view},
        picker::{ProjectPickerView, project_picker},
        scripts::{ProjectScriptsView, project_scripts},
    },
    search::{SearchOptions, SearchView, search_view},
    scheduled_tasks::{
        ScheduledTaskBranchView, ScheduledTaskListView, branch_view as scheduled_task_branch_view,
        draft as scheduled_task_draft, list as scheduled_task_list,
    },
    settings::{
        SettingId, SettingValue, SettingsRow, SettingsScope, SettingsView, default_model_picker,
        setting_intent, setting_reset_intent, settings_view,
    },
    sidebar::{SidebarOptions, SidebarThreadDropPlan, SidebarView, plan_sidebar_drop, sidebar},
    snooze::{CustomSnoozeInput, SnoozePreset, resolve_custom_snooze, resolve_snooze_presets},
    terminals::{
        TerminalTab, TerminalView, terminal_tabs, terminal_view,
        text_size::{TerminalTextSize, terminal_text_size},
    },
    thread::{ThreadView, ThreadViewOptions, selected_thread_view, thread_view},
    thread_arrangement::{ArrangementDrop, ArrangementOptions},
    thread_list::{ThreadListHolds, ThreadListOptions, ThreadListView, thread_list},
    thread_menu::{ThreadMenuOptions, ThreadMenuView, thread_menu},
    time::TimestampFormat,
    timeline::mobile_follow::{LiveFollowEvent, StreamHaptic, StreamingMessageMark},
    timeline::rows::{TimelineRow, TimelineUpdate},
};
use agent_domain::ThreadId;
use chrono::{Local, TimeZone};

/// A project icon's image bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ProjectIconView {
    pub hash: String,
    pub mime_type: String,
    pub data: Vec<u8>,
}

/// One provider instance's model order, by slug.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ModelOrder {
    pub instance_id: String,
    pub models: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
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
    fn picker_options(
        &self,
        query: String,
        rail: Option<PickerRail>,
        toggled_legacy: Vec<String>,
    ) -> ModelPickerOptions {
        ModelPickerOptions {
            query,
            rail,
            toggled_legacy,
            favorites: self.preferences.favorite_models.clone(),
            model_order: self
                .preferences
                .model_order
                .iter()
                .map(|(instance, models)| (instance.clone(), models.clone()))
                .collect(),
        }
    }
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

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    pub fn scheduled_tasks(&self) -> ScheduledTaskListView {
        scheduled_task_list(self)
    }
    pub fn scheduled_task_draft(
        &self,
        id: Option<String>,
    ) -> crate::state::ScheduledTaskDraft {
        scheduled_task_draft(self, id.as_deref())
    }
    pub fn scheduled_task_branches(
        &self,
        project_id: String,
        selected_branch: String,
    ) -> ScheduledTaskBranchView {
        scheduled_task_branch_view(self, &project_id, &selected_branch)
    }
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
    /// The "Arrange threads" sheet.
    pub fn thread_arrangement(
        &self,
        now_ms: i64,
        options: crate::view::thread_arrangement::ArrangementOptions,
    ) -> crate::view::thread_arrangement::ThreadArrangementView {
        crate::view::thread_arrangement::thread_arrangement(self, now_ms, options)
    }
    /// Where the sheet's `thread_id` lands dropped before row `to_index`.
    pub fn thread_arrangement_move(
        &self,
        now_ms: i64,
        options: crate::view::thread_arrangement::ArrangementOptions,
        thread_id: String,
        to_index: u32,
    ) -> Option<crate::view::thread_arrangement::ArrangementDrop> {
        crate::view::thread_arrangement::thread_arrangement_move(
            self, now_ms, options, &thread_id, to_index,
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
    /// What giving a settings row a new value does; `None` when it does not fit.
    pub fn setting_intent(
        &self,
        scope: SettingsScope,
        id: SettingId,
        value: SettingValue,
    ) -> Option<crate::state::Intent> {
        setting_intent(self, &scope, id, &value)
    }
    /// What a settings row's reset does; `None` when the row offers none.
    pub fn setting_reset(
        &self,
        scope: SettingsScope,
        row: SettingsRow,
    ) -> Option<crate::state::Intent> {
        setting_reset_intent(&scope, &row)
    }
    /// `toggled_legacy` names the instances whose "Legacy models" row the
    /// user toggled since the picker opened.
    pub fn model_picker(
        &self,
        query: String,
        rail: Option<PickerRail>,
        toggled_legacy: Vec<String>,
    ) -> ModelPickerView {
        model_picker(
            self,
            &self.current_draft(),
            &self.picker_options(query, rail, toggled_legacy),
        )
    }
    /// The settings Model row's picker over the new-thread default.
    pub fn default_model_picker(
        &self,
        query: String,
        rail: Option<PickerRail>,
        toggled_legacy: Vec<String>,
    ) -> ModelPickerView {
        default_model_picker(self, &self.picker_options(query, rail, toggled_legacy))
    }
    /// The mobile thread settings sheet's model catalogue.
    pub fn catalog_sheet(
        &self,
        options: crate::view::models::catalog_sheet::CatalogSheetOptions,
    ) -> crate::view::models::catalog_sheet::CatalogSheetView {
        crate::view::models::catalog_sheet::catalog_sheet(self, &options)
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
        let git = thread.as_ref().map(|thread| {
            crate::view::checkpoints::git_diff_view(self, &self.thread_cwd(thread), &selection)
        });
        DiffPanelView {
            git,
            ..diff_panel(
                &checkpoints,
                &selection,
                self.preferences.diff_ignore_whitespace,
            )
        }
    }
    /// The files of the thread's diff while it is shown file by file.
    pub fn review_files(
        &self,
        thread_id: String,
    ) -> Option<crate::view::review_files::ReviewFilesView> {
        let thread = thread(thread_id)?;
        let unselected = DiffPanelSelection::default();
        let selection = self.diff_panels.get(&thread).unwrap_or(&unselected);
        crate::view::review_files::lazy_entry(self, &self.thread_cwd(&thread), selection)
            .map(crate::view::review_files::review_files_view)
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
    /// The Undo offered for the latest thread actions, until it expires.
    pub fn thread_undo_notice(
        &self,
        now_ms: i64,
    ) -> Option<crate::commands::undo::ThreadUndoNotice> {
        self.thread_undo.notice(now_ms)
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
    /// The project's icon; `None` shows its initials. Clients cache the image
    /// by `hash`.
    pub fn project_icon(&self, project_id: String) -> Option<ProjectIconView> {
        self.project_icons
            .get(&project_id)
            .and_then(|entry| entry.icon.as_ref())
            .map(|icon| ProjectIconView {
                hash: icon.hash.clone(),
                mime_type: icon.mime_type.clone(),
                data: icon.data.as_ref().clone(),
            })
    }
    /// The hash of the project's icon, to look up a cached image without
    /// copying its bytes.
    pub fn project_icon_hash(&self, project_id: String) -> Option<String> {
        self.project_icons
            .get(&project_id)
            .and_then(|entry| entry.icon.as_ref())
            .map(|icon| icon.hash.clone())
    }
    pub fn conversation_settings_loaded(&self) -> bool {
        self.conversation_settings.is_some()
    }
    /// The files of one draft, such as a question answer's.
    pub fn draft_attachments(&self, draft_key: String) -> Vec<DraftAttachment> {
        self.drafts
            .get(&draft_key)
            .map(|draft| draft.attachments.clone())
            .unwrap_or_default()
    }
    /// Captured viewport lines with the terminal's display metadata.
    pub fn terminal_output_context(
        &self,
        thread_id: String,
        terminal_id: String,
        output: String,
        start: u32,
        end: u32,
    ) -> crate::view::terminals::output_context::TerminalOutputContext {
        let lines = crate::view::terminals::output_context::visible_output_lines(&output);
        let selection =
            crate::view::terminals::output_context::visible_output_selection(&lines, start, end);
        crate::view::terminals::output_context::TerminalOutputContext {
            terminal_label: ThreadId::new(thread_id)
                .ok()
                .map(|thread| crate::view::terminals::tab_label(self, &thread, &terminal_id))
                .unwrap_or_default(),
            terminal_id,
            line_start: start.saturating_add(1),
            line_end: end.saturating_add(1),
            text: if end as usize >= lines.len() {
                String::new()
            } else {
                selection.text
            },
        }
    }
    /// The terminal's text size and the menu's "Text size" steps.
    pub fn terminal_text_size(&self) -> TerminalTextSize {
        terminal_text_size(self.preferences.terminal_font_size)
    }
    /// The mobile "Choose project" screen for the search text.
    pub fn project_picker(&self, query: String) -> ProjectPickerView {
        project_picker(self, &query)
    }
    /// The folders the add-project path field browses; list `directory_path`
    /// with `Intent::ListFiles` until `listed`.
    pub fn folder_browser(&self, query: String) -> FolderBrowserView {
        let listed = self
            .workspace
            .listed_directory
            .as_deref()
            .zip(self.workspace.directory.as_ref())
            .map(|(path, list)| (path, list.entries.as_slice()));
        folder_browser(&query, self.connected, listed)
    }
    /// What adding the typed folder does.
    pub fn add_project_target(&self, raw_path: String) -> AddProjectTarget {
        add_project_target(self.shell_projects(), &raw_path)
    }
    /// A model picked in the sheet starts with its last chosen options.
    pub fn stage_model(
        &self,
        current: Option<StagedModel>,
        mut pressed: StagedModel,
        pressed_is_applied: bool,
    ) -> Option<StagedModel> {
        pressed.options = staging::with_remembered_model_options(
            &self.preferences.model_options,
            &pressed.instance_id,
            &pressed.model,
            pressed.options,
        );
        staging::staged_model_after_press(current, pressed, pressed_is_applied)
    }
    /// The settings sheet's option rows while a model is staged.
    pub fn staged_model_traits(&self, staged: StagedModel) -> TraitsView {
        staging::staged_model_traits(&catalog(self), &staged)
    }
    /// The staged model after choosing a select option; remember its options
    /// with `Intent::RememberModelOptions`.
    pub fn select_staged_trait(
        &self,
        staged: StagedModel,
        descriptor_id: String,
        choice: String,
    ) -> StagedModel {
        staging::select_staged_trait(&catalog(self), staged, &descriptor_id, &choice)
    }
    /// The staged model after switching a toggle option.
    pub fn toggle_staged_trait(
        &self,
        staged: StagedModel,
        descriptor_id: String,
        on: bool,
    ) -> StagedModel {
        staging::toggle_staged_trait(&catalog(self), staged, &descriptor_id, on)
    }
    /// Save is possible while the staged model's provider still offers it.
    pub fn can_save_staged_model(&self, staged: StagedModel) -> bool {
        staging::can_save_staged_model(&catalog(self), &staged)
    }
    /// The mobile "Arrange threads" sheet.
    pub fn thread_arrangement_drop(
        &self,
        now_ms: i64,
        options: ArrangementOptions,
        thread_id: String,
        target_key: String,
        after: bool,
    ) -> Option<ArrangementDrop> {
        crate::view::thread_arrangement::thread_arrangement_drop(
            self,
            now_ms,
            options,
            &thread_id,
            &target_key,
            after,
        )
    }
}

/// The staged model's `CatalogSheetOptions.staged_key`.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn staged_model_key(staged: StagedModel) -> String {
    staged.key()
}

/// The add-project path field's first text: the configured folder, or home.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn add_project_initial_query(base_directory: Option<String>) -> String {
    crate::view::projects::add::add_project_initial_query(base_directory.as_deref())
}

/// How to bring a list showing `previous` rows to `next`.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn timeline_update(previous: Vec<TimelineRow>, next: Vec<TimelineRow>) -> TimelineUpdate {
    crate::view::timeline::rows::timeline_update(&previous, &next)
}

/// Whether the mobile feed keeps following its end after `event`.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn feed_live_follow(current: bool, event: LiveFollowEvent) -> bool {
    crate::view::timeline::mobile_follow::feed_live_follow(current, event)
}

/// The streaming haptic after the feed showed `streaming` for `thread_id`.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn stream_haptic(
    previous: Option<StreamHaptic>,
    thread_id: String,
    streaming: Option<StreamingMessageMark>,
    now_ms: i64,
) -> StreamHaptic {
    crate::view::timeline::mobile_follow::stream_haptic(previous, &thread_id, streaming, now_ms)
}

/// What tapping `href` in a thread whose workspace is `workspace_root` does.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn markdown_link_action(
    href: String,
    workspace_root: Option<String>,
) -> crate::presentation::markdown::links::MarkdownLinkAction {
    crate::presentation::markdown::links::markdown_link_action(&href, workspace_root.as_deref())
}

/// The file screen's subtitle for `path` in the project `project_name`.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn file_header_subtitle(project_name: String, path: String) -> String {
    crate::presentation::markdown::links::file_header_subtitle(&project_name, &path)
}

/// Where a conversation image's bytes load from.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn markdown_image_source(
    href: String,
    workspace_root: Option<String>,
) -> crate::view::work_log::media_source::MarkdownImageSource {
    crate::view::work_log::media_source::classify_markdown_image_source(
        Some(&href),
        workspace_root.as_deref(),
    )
}

/// How large a conversation image of the given pixel size is drawn.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn markdown_image_display_size(
    source_width: f64,
    source_height: f64,
    available_width: f64,
) -> Option<crate::presentation::markdown::image_size::ImageDisplaySize> {
    crate::presentation::markdown::image_size::markdown_image_display_size(
        source_width,
        source_height,
        available_width,
    )
}

/// A terminal's visible output as the attach sheet lists it.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn visible_output_lines(text: String) -> Vec<String> {
    crate::view::terminals::output_context::visible_output_lines(&text)
}

/// Lines `start` through `end` (0-based) of the attach sheet.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn visible_output_selection(
    lines: Vec<String>,
    start: u32,
    end: u32,
) -> crate::view::terminals::output_context::VisibleOutputSelection {
    crate::view::terminals::output_context::visible_output_selection(&lines, start, end)
}

/// Whether a feed message appearing now fades in.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn feed_entry_fades_in(created_at_ms: Option<i64>, now_ms: i64) -> bool {
    crate::view::timeline::mobile_follow::feed_entry_fades_in(created_at_ms, now_ms)
}

/// The draft key holding the files attached to one question's answer.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn answer_draft_key(request_id: String, question_id: String) -> String {
    crate::state::answer_draft_key(&request_id, &question_id)
}

/// Which picked files join a draft holding `existing`; accepted images marked
/// `needs_compression` are downscaled before `Intent::AttachFiles`.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn admit_attachments(
    existing: Vec<DraftAttachment>,
    candidates: Vec<AttachmentCandidate>,
) -> AttachmentAdmission {
    crate::view::attachments::admit_attachments(&existing, &candidates)
}

/// The snooze choices at `now_ms` in the device's time zone.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn snooze_presets(now_ms: i64, format: TimestampFormat) -> Vec<SnoozePreset> {
    Local
        .timestamp_millis_opt(now_ms)
        .single()
        .map(|now| resolve_snooze_presets(&now, format))
        .unwrap_or_default()
}

/// A custom snooze's wake time from `now_ms` in the device's time zone;
/// `None` for invalid, nonexistent or past times.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn custom_snooze_until(input: CustomSnoozeInput, now_ms: i64) -> Option<String> {
    let now = Local.timestamp_millis_opt(now_ms).single()?;
    resolve_custom_snooze(&input, &now)
}

/// The ticking time of the "Working for …" row.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn working_timer_label(started_at_ms: i64, now_ms: i64) -> String {
    crate::view::time::format_working_timer(started_at_ms, now_ms)
}

/// The ticking time of the working pill ("12m 04s").
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn working_duration_label(started_at_ms: i64, now_ms: i64) -> String {
    crate::view::working_status::format_working_duration(started_at_ms, now_ms)
}

/// Why the client could not prepare an image for upload.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn image_preparation_error(name: String, unreadable: bool) -> String {
    crate::view::attachments::image_preparation_error(&name, unreadable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::snooze::SnoozeDurationUnit;

    #[test]
    fn a_custom_snooze_wakes_after_a_positive_duration_only() {
        let now = 1_800_000_000_000;
        let until = custom_snooze_until(
            CustomSnoozeInput::Duration {
                amount: "2".into(),
                unit: SnoozeDurationUnit::Hours,
            },
            now,
        )
        .unwrap();
        assert_eq!(
            agent_domain::Timestamp::parse(&until).unwrap().millis(),
            now + 2 * 3_600_000
        );
        assert_eq!(
            custom_snooze_until(
                CustomSnoozeInput::Duration {
                    amount: "0".into(),
                    unit: SnoozeDurationUnit::Minutes,
                },
                now,
            ),
            None
        );
    }
}

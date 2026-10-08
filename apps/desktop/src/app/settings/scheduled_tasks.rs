//! Scheduled-task settings. The editor keeps only transient text controls;
//! schedule validation and Host conversion belong to agent-core.
use super::{Choice, Row, page_container, section, select, select_sized};
use crate::app::{
    Desktop,
    ui::{color, icon, tint},
};
use agent_core::{
    state::{Intent, ScheduledTaskDraft, ScheduledTaskScheduleDraft, ScheduledTaskWorkspaceDraft},
    view::{
        composer::controls::runtime_mode_choices,
        models::{ProviderStatus, traits::TraitControl},
    },
};
use agent_domain::RuntimeMode;
use gpui_kit::{
    component::{
        Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        input::{Input, InputEvent, InputState},
        switch::Switch,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};

pub(super) struct ScheduledTasksState {
    title: Entity<InputState>,
    prompt: Entity<InputState>,
    time: Entity<InputState>,
    interval: Entity<InputState>,
    branch: Entity<InputState>,
    worktree: Entity<InputState>,
    selected: Option<String>,
    draft: Option<ScheduledTaskDraft>,
    branches_project: Option<String>,
    seeded: bool,
    stale: bool,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

fn runtime_mode_id(mode: RuntimeMode) -> &'static str {
    match mode {
        RuntimeMode::ApprovalRequired => "approval-required",
        RuntimeMode::AutoAcceptEdits => "auto-accept-edits",
        RuntimeMode::Auto => "auto",
        RuntimeMode::FullAccess => "full-access",
    }
}

fn runtime_mode_from_id(id: &str) -> Option<RuntimeMode> {
    Some(match id {
        "approval-required" => RuntimeMode::ApprovalRequired,
        "auto-accept-edits" => RuntimeMode::AutoAcceptEdits,
        "auto" => RuntimeMode::Auto,
        "full-access" => RuntimeMode::FullAccess,
        _ => return None,
    })
}

impl ScheduledTasksState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Desktop>) -> Self {
        let title = cx.new(|cx| InputState::new(window, cx).placeholder("Task title"));
        let prompt = cx.new(|cx| InputState::new(window, cx).placeholder("Prompt"));
        let time = cx.new(|cx| InputState::new(window, cx).placeholder("09:00"));
        let interval = cx.new(|cx| InputState::new(window, cx).placeholder("15"));
        let branch = cx.new(|cx| InputState::new(window, cx).placeholder("Base branch"));
        let worktree = cx.new(|cx| InputState::new(window, cx).placeholder("Worktree path"));
        let subscriptions = vec![
            cx.subscribe_in(&title, window, |view, input, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change)
                    && let Some(draft) = view.settings.scheduled_tasks.draft.as_mut()
                {
                    draft.title = input.read(cx).value();
                }
            }),
            cx.subscribe_in(&prompt, window, |view, input, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change)
                    && let Some(draft) = view.settings.scheduled_tasks.draft.as_mut()
                {
                    draft.prompt = input.read(cx).value();
                }
            }),
            cx.subscribe_in(&time, window, |view, input, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change)
                    && let Some(draft) = view.settings.scheduled_tasks.draft.as_mut()
                    && let ScheduledTaskScheduleDraft::FixedTime { time_of_day, .. } =
                        &mut draft.schedule
                {
                    *time_of_day = input.read(cx).value();
                }
            }),
            cx.subscribe_in(
                &interval,
                window,
                |view, input, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change)
                        && let Some(draft) = view.settings.scheduled_tasks.draft.as_mut()
                        && let Ok(minutes) = input.read(cx).value().trim().parse::<u64>()
                    {
                        draft.schedule = ScheduledTaskScheduleDraft::Interval {
                            every_ms: minutes.saturating_mul(60_000),
                        };
                    }
                },
            ),
            cx.subscribe_in(&branch, window, |view, input, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change)
                    && let Some(draft) = view.settings.scheduled_tasks.draft.as_mut()
                {
                    let value = input.read(cx).value();
                    if let ScheduledTaskWorkspaceDraft::Worktree { base_ref, .. } =
                        &mut draft.workspace
                    {
                        *base_ref = value;
                    }
                }
            }),
            cx.subscribe_in(
                &worktree,
                window,
                |view, input, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change)
                        && let Some(draft) = view.settings.scheduled_tasks.draft.as_mut()
                    {
                        let value = input.read(cx).value();
                        if let ScheduledTaskWorkspaceDraft::ExistingWorktree {
                            worktree_path, ..
                        } = &mut draft.workspace
                        {
                            *worktree_path = value;
                        }
                    }
                },
            ),
        ];
        Self {
            title,
            prompt,
            time,
            interval,
            branch,
            worktree,
            selected: None,
            draft: None,
            branches_project: None,
            seeded: false,
            stale: false,
            error: None,
            _subscriptions: subscriptions,
        }
    }
}

impl Desktop {
    fn select_scheduled_task(&mut self, id: Option<String>) {
        self.settings.scheduled_tasks.selected = id;
        self.settings.scheduled_tasks.draft = None;
        self.settings.scheduled_tasks.branches_project = None;
        self.settings.scheduled_tasks.seeded = false;
        self.settings.scheduled_tasks.stale = false;
        self.settings.scheduled_tasks.error = None;
    }

    fn sync_scheduled_task_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let scheduled_ids = self
            .snapshot
            .scheduled_tasks()
            .tasks
            .into_iter()
            .map(|task| task.id)
            .collect::<std::collections::HashSet<_>>();
        let search_project = {
            let state = &mut self.settings.scheduled_tasks;
            if let Some(selected) = state.selected.as_deref()
                && state
                    .draft
                    .as_ref()
                    .is_some_and(|draft| draft.id.as_deref() == Some(selected))
                && !scheduled_ids.contains(selected)
            {
                state.stale = true;
            }
            if state.draft.is_none() {
                state.draft = Some(self.snapshot.scheduled_task_draft(state.selected.clone()));
                state.seeded = false;
            }
            if state.seeded {
                return;
            }
            let Some(draft) = state.draft.as_ref() else {
                return;
            };
            let branch_project = matches!(
                &draft.workspace,
                ScheduledTaskWorkspaceDraft::Worktree { .. }
            )
            .then(|| draft.project_id.clone());
            let search_project = (state.branches_project != branch_project)
                .then(|| branch_project.clone())
                .flatten();
            state.branches_project = branch_project;
            let (time, interval) = match &draft.schedule {
                ScheduledTaskScheduleDraft::FixedTime { time_of_day, .. } => {
                    (time_of_day.clone(), String::new())
                }
                ScheduledTaskScheduleDraft::Interval { every_ms } => {
                    (("09:00").into(), (every_ms / 60_000).to_string())
                }
            };
            let (branch, worktree) = match &draft.workspace {
                ScheduledTaskWorkspaceDraft::Root { branch } => {
                    (branch.clone().unwrap_or_default(), String::new())
                }
                ScheduledTaskWorkspaceDraft::Worktree {
                    base_ref, branch, ..
                } => (
                    branch.clone().unwrap_or_else(|| base_ref.clone()),
                    String::new(),
                ),
                ScheduledTaskWorkspaceDraft::ExistingWorktree {
                    worktree_path,
                    branch,
                } => (branch.clone().unwrap_or_default(), worktree_path.clone()),
            };
            state.title.update(cx, |input, cx| {
                input.set_value(draft.title.clone(), window, cx)
            });
            state.prompt.update(cx, |input, cx| {
                input.set_value(draft.prompt.clone(), window, cx)
            });
            state
                .time
                .update(cx, |input, cx| input.set_value(time, window, cx));
            state
                .interval
                .update(cx, |input, cx| input.set_value(interval, window, cx));
            state
                .branch
                .update(cx, |input, cx| input.set_value(branch, window, cx));
            state
                .worktree
                .update(cx, |input, cx| input.set_value(worktree, window, cx));
            state.seeded = true;
            search_project
        };
        if let Some(project_id) = search_project {
            self.perform(Intent::SearchScheduledTaskBranches {
                project_id,
                query: String::new(),
            });
        }
    }

    fn save_scheduled_task(&mut self, cx: &mut Context<Desktop>) {
        if self.settings.scheduled_tasks.stale {
            return;
        }
        let Some(draft) = self.settings.scheduled_tasks.draft.clone() else {
            return;
        };
        self.perform_then(
            Intent::SaveScheduledTask { draft },
            |view, result, _, cx| {
                if result.is_ok() {
                    view.settings.scheduled_tasks.draft = None;
                    view.settings.scheduled_tasks.selected = None;
                    view.settings.scheduled_tasks.branches_project = None;
                    view.settings.scheduled_tasks.seeded = false;
                    view.settings.scheduled_tasks.error = None;
                } else {
                    view.settings.scheduled_tasks.error =
                        result.err().map(|error| error.to_string());
                }
                cx.notify();
            },
        );
        cx.notify();
    }

    pub(super) fn render_scheduled_tasks(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Desktop>,
    ) -> AnyElement {
        self.sync_scheduled_task_inputs(window, cx);
        let rows = self
            .snapshot
            .scheduled_tasks()
            .tasks
            .into_iter()
            .enumerate()
            .map(|(index, task)| {
                let id = task.id.clone();
                let enabled_id = id.clone();
                let selected =
                    self.settings.scheduled_tasks.selected.as_deref() == Some(id.as_str());
                let enabled = task.enabled;
                h_flex()
                    .id(("scheduled-task", index))
                    .px_4()
                    .py_3()
                    .gap_3()
                    .cursor_pointer()
                    .when(selected, |row| row.bg(tint("accent", 0.08)))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(div().text_sm().font_medium().child(task.title.clone()))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(color("textMuted"))
                                    .child(task.schedule_label.clone()),
                            ),
                    )
                    .child(
                        Button::new(("scheduled-task-enabled", index))
                            .ghost()
                            .small()
                            .label(if enabled { "Enabled" } else { "Paused" })
                            .on_click(cx.listener(move |view, _, _, _| {
                                view.perform(Intent::SetScheduledTaskEnabled {
                                    id: enabled_id.clone(),
                                    enabled: !enabled,
                                });
                            })),
                    )
                    .child(
                        Button::new(("scheduled-task-run", index))
                            .ghost()
                            .small()
                            .label("Run now")
                            .on_click(cx.listener({
                                let id = id.clone();
                                move |view, _, _, _| {
                                    view.perform(Intent::RunScheduledTaskNow { id: id.clone() });
                                }
                            })),
                    )
                    .on_click(cx.listener(move |view, _, _, _| {
                        view.select_scheduled_task(Some(id.clone()));
                    }))
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        let new_button = Button::new("scheduled-task-new")
            .outline()
            .small()
            .icon(icon("plus"))
            .label("New task")
            .on_click(cx.listener(|view, _, _, _| view.select_scheduled_task(None)));
        let mut sections = vec![
            section(
                Some("Scheduled tasks".into()),
                Some(icon("calendar-clock").size_4().into_any_element()),
                Some(new_button.into_any_element()),
                if rows.is_empty() {
                    vec![
                        div()
                            .px_4()
                            .py_4()
                            .text_sm()
                            .text_color(color("textMuted"))
                            .child("No scheduled tasks yet")
                            .into_any_element(),
                    ]
                } else {
                    rows
                },
            )
            .into_any_element(),
        ];
        if self.settings.scheduled_tasks.draft.is_some() {
            sections.push(self.render_scheduled_task_editor(window, cx));
        }
        page_container(960., sections)
    }

    fn render_scheduled_task_editor(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Desktop>,
    ) -> AnyElement {
        let draft = self
            .settings
            .scheduled_tasks
            .draft
            .clone()
            .expect("editor draft");
        let schedule_kind = match &draft.schedule {
            ScheduledTaskScheduleDraft::Interval { .. } => "interval",
            ScheduledTaskScheduleDraft::FixedTime { .. } => "fixed",
        };
        let workspace_kind = match &draft.workspace {
            ScheduledTaskWorkspaceDraft::Root { .. } => "root",
            ScheduledTaskWorkspaceDraft::ExistingWorktree { .. } => "existing_worktree",
            ScheduledTaskWorkspaceDraft::Worktree { .. } => "worktree",
        };
        let schedule_choices = vec![
            Choice {
                id: "fixed".into(),
                label: "Fixed local time".into(),
                description: None,
                icon: Some("clock-3"),
                selected: schedule_kind == "fixed",
            },
            Choice {
                id: "interval".into(),
                label: "Interval".into(),
                description: None,
                icon: Some("timer"),
                selected: schedule_kind == "interval",
            },
        ];
        let workspace_choices = vec![
            Choice {
                id: "root".into(),
                label: "Project checkout".into(),
                description: None,
                icon: Some("folder"),
                selected: workspace_kind == "root",
            },
            Choice {
                id: "worktree".into(),
                label: "New worktree".into(),
                description: None,
                icon: Some("git-branch"),
                selected: workspace_kind == "worktree",
            },
            Choice {
                id: "existing_worktree".into(),
                label: "Existing worktree".into(),
                description: None,
                icon: Some("folder-tree"),
                selected: workspace_kind == "existing_worktree",
            },
        ];
        let projects = self.snapshot.shell_projects().to_vec();
        let project_choices = projects
            .iter()
            .map(|project| Choice {
                id: project.id.clone(),
                label: project.name.clone(),
                description: None,
                icon: None,
                selected: project.id == draft.project_id,
            })
            .collect();
        let models = self
            .snapshot
            .providers
            .as_ref()
            .into_iter()
            .flat_map(|providers| providers.iter())
            .filter(|provider| {
                provider.enabled
                    && provider.installed
                    && provider.available
                    && !matches!(
                        provider.status,
                        ProviderStatus::Error | ProviderStatus::Disabled
                    )
            })
            .flat_map(|provider| provider.models.iter().map(move |model| (provider, model)))
            .map(|(provider, model)| Choice {
                id: format!("{}\n{}", provider.instance, model.slug),
                label: format!("{} · {}", provider.display_name, model.name),
                description: None,
                icon: Some("bot"),
                selected: provider.instance == draft.instance_id && model.slug == draft.model,
            })
            .collect();
        let runtime_choices = self
            .snapshot
            .providers
            .as_ref()
            .into_iter()
            .flat_map(|providers| providers.iter())
            .find(|provider| provider.instance == draft.instance_id)
            .map_or_else(
                || runtime_mode_choices(&[]),
                |provider| runtime_mode_choices(&provider.supported_runtime_modes),
            )
            .into_iter()
            .map(|choice| Choice {
                id: runtime_mode_id(choice.mode).into(),
                label: choice.label,
                description: Some(choice.description),
                icon: Some("shield-check"),
                selected: choice.mode == draft.runtime_mode,
            })
            .collect::<Vec<_>>();
        let draft_id = self.settings.scheduled_tasks.selected.clone();
        let mut editor_rows = vec![
            Row::new("Title")
                .control(Input::new(&self.settings.scheduled_tasks.title).small())
                .render(),
            Row::new("Prompt")
                .control(Input::new(&self.settings.scheduled_tasks.prompt).small())
                .render(),
            Row::new("Enabled")
                .control(
                    Switch::new("scheduled-task-editor-enabled")
                        .checked(draft.enabled)
                        .accessibility_label("Enable scheduled task")
                        .on_click(cx.listener(|view, enabled: &bool, _, cx| {
                            if let Some(draft) = view.settings.scheduled_tasks.draft.as_mut() {
                                draft.enabled = *enabled;
                            }
                            cx.notify();
                        }))
                        .into_any_element(),
                )
                .render(),
            Row::new("Project")
                .control(
                    select(
                        "scheduled-project",
                        draft.project_id.clone(),
                        project_choices,
                        move |view, choice, _, cx| {
                            if let Some(draft) = view.settings.scheduled_tasks.draft.as_mut() {
                                draft.project_id = choice;
                            }
                            view.settings.scheduled_tasks.branches_project = None;
                            view.settings.scheduled_tasks.seeded = false;
                            cx.notify();
                        },
                        cx,
                    )
                    .into_any_element(),
                )
                .render(),
            Row::new("Model")
                .control(
                    select_sized(
                        "scheduled-model",
                        format!("{} · {}", draft.instance_id, draft.model),
                        models,
                        260.,
                        move |view, choice, _, cx| {
                            if let Some((instance, model)) = choice.split_once('\n')
                                && let Some(driver) = view
                                    .snapshot
                                    .providers
                                    .as_ref()
                                    .into_iter()
                                    .flat_map(|providers| providers.iter())
                                    .find(|provider| provider.instance == instance)
                                    .map(|provider| provider.driver)
                                && let Some(draft) = view.settings.scheduled_tasks.draft.as_mut()
                            {
                                let same_model =
                                    draft.instance_id == instance && draft.model == model;
                                draft.instance_id = instance.into();
                                draft.model = model.into();
                                draft.driver = driver;
                                if !same_model {
                                    draft.options.clear();
                                }
                            }
                            cx.notify();
                        },
                        cx,
                    )
                    .into_any_element(),
                )
                .render(),
            Row::new("Runtime")
                .control(
                    select(
                        "scheduled-runtime",
                        runtime_mode_id(draft.runtime_mode),
                        runtime_choices,
                        |view, choice, _, cx| {
                            if let Some(mode) = runtime_mode_from_id(&choice)
                                && let Some(draft) = view.settings.scheduled_tasks.draft.as_mut()
                            {
                                draft.runtime_mode = mode;
                            }
                            cx.notify();
                        },
                        cx,
                    )
                    .into_any_element(),
                )
                .render(),
            Row::new("Schedule")
                .control(
                    select(
                        "scheduled-schedule",
                        schedule_kind,
                        schedule_choices,
                        |view, choice, _, cx| {
                            if let Some(draft) = view.settings.scheduled_tasks.draft.as_mut() {
                                draft.schedule = if choice == "interval" {
                                    ScheduledTaskScheduleDraft::Interval {
                                        every_ms: 15 * 60_000,
                                    }
                                } else {
                                    ScheduledTaskScheduleDraft::FixedTime {
                                        time_of_day: "09:00".into(),
                                        weekdays: vec![1, 2, 3, 4, 5],
                                    }
                                };
                                view.settings.scheduled_tasks.seeded = false;
                            }
                            cx.notify();
                        },
                        cx,
                    )
                    .into_any_element(),
                )
                .render(),
            if schedule_kind == "interval" {
                Row::new("Interval (minutes)")
                    .control(Input::new(&self.settings.scheduled_tasks.interval).small())
                    .render()
            } else {
                let weekdays = match &draft.schedule {
                    ScheduledTaskScheduleDraft::FixedTime { weekdays, .. } => weekdays.clone(),
                    ScheduledTaskScheduleDraft::Interval { .. } => vec![],
                };
                let labels = [
                    ("S", 0u8),
                    ("M", 1),
                    ("T", 2),
                    ("W", 3),
                    ("T", 4),
                    ("F", 5),
                    ("S", 6),
                ];
                let day_buttons = labels
                    .into_iter()
                    .enumerate()
                    .map(|(index, (label, day))| {
                        let selected = weekdays.contains(&day);
                        Button::new(("scheduled-task-day", index))
                            .small()
                            .when(selected, |button| button.primary())
                            .ghost()
                            .label(label)
                            .on_click(cx.listener(move |view, _, _, cx| {
                                if let Some(draft) = view.settings.scheduled_tasks.draft.as_mut()
                                    && let ScheduledTaskScheduleDraft::FixedTime {
                                        weekdays, ..
                                    } = &mut draft.schedule
                                {
                                    if let Some(position) =
                                        weekdays.iter().position(|current| *current == day)
                                    {
                                        weekdays.remove(position);
                                    } else {
                                        weekdays.push(day);
                                        weekdays.sort_unstable();
                                    }
                                }
                                cx.notify();
                            }))
                            .into_any_element()
                    })
                    .collect::<Vec<_>>();
                Row::new("Local time")
                    .control(
                        v_flex()
                            .gap_1()
                            .child(Input::new(&self.settings.scheduled_tasks.time).small())
                            .child(h_flex().gap_1().children(day_buttons))
                            .into_any_element(),
                    )
                    .render()
            },
            Row::new("Workspace")
                .control(
                    select(
                        "scheduled-workspace",
                        workspace_kind,
                        workspace_choices,
                        |view, choice, _, cx| {
                            if let Some(draft) = view.settings.scheduled_tasks.draft.as_mut() {
                                draft.workspace = match choice.as_str() {
                                    "worktree" => ScheduledTaskWorkspaceDraft::Worktree {
                                        base_ref: "main".into(),
                                        branch: None,
                                        start_from_origin: true,
                                    },
                                    "existing_worktree" => {
                                        ScheduledTaskWorkspaceDraft::ExistingWorktree {
                                            worktree_path: String::new(),
                                            branch: None,
                                        }
                                    }
                                    _ => ScheduledTaskWorkspaceDraft::Root { branch: None },
                                };
                                view.settings.scheduled_tasks.branches_project = None;
                                view.settings.scheduled_tasks.seeded = false;
                            }
                            cx.notify();
                        },
                        cx,
                    )
                    .into_any_element(),
                )
                .render(),
        ];
        let scheduled_traits = self.snapshot.scheduled_task_traits(draft.clone());
        for control in scheduled_traits.controls {
            match control {
                TraitControl::Select {
                    id,
                    label,
                    choices,
                    selected,
                    disabled,
                    ..
                } => {
                    let descriptor_id = id.clone();
                    let options = choices
                        .into_iter()
                        .map(|choice| {
                            let is_selected = choice.id == selected;
                            Choice {
                                id: choice.id,
                                label: choice.label,
                                description: choice.description,
                                icon: Some("sliders-horizontal"),
                                selected: is_selected,
                            }
                        })
                        .collect();
                    editor_rows.push(
                        Row::new(label)
                            .control(
                                select(
                                    format!("scheduled-option-{descriptor_id}"),
                                    selected,
                                    options,
                                    move |view, choice, _, cx| {
                                        if !disabled {
                                            let current =
                                                view.settings.scheduled_tasks.draft.clone();
                                            if let Some(current) = current {
                                                let next =
                                                    view.snapshot.select_scheduled_task_trait(
                                                        current,
                                                        descriptor_id.clone(),
                                                        choice,
                                                    );
                                                view.settings.scheduled_tasks.draft = Some(next);
                                            }
                                        }
                                        cx.notify();
                                    },
                                    cx,
                                )
                                .into_any_element(),
                            )
                            .render(),
                    );
                }
                TraitControl::Toggle { id, label, on } => {
                    let descriptor_id = id.clone();
                    editor_rows.push(
                        Row::new(label)
                            .control(
                                Switch::new(format!("scheduled-option-{descriptor_id}"))
                                    .checked(on)
                                    .on_click(cx.listener(move |view, value: &bool, _, cx| {
                                        let current = view.settings.scheduled_tasks.draft.clone();
                                        if let Some(current) = current {
                                            let next = view.snapshot.toggle_scheduled_task_trait(
                                                current,
                                                descriptor_id.clone(),
                                                *value,
                                            );
                                            view.settings.scheduled_tasks.draft = Some(next);
                                        }
                                        cx.notify();
                                    }))
                                    .into_any_element(),
                            )
                            .render(),
                    );
                }
            }
        }
        let branch_row = match workspace_kind {
            "worktree" => {
                let branch_view = self.snapshot.scheduled_task_branches(
                    draft.project_id.clone(),
                    match &draft.workspace {
                        ScheduledTaskWorkspaceDraft::Worktree { base_ref, .. } => base_ref.clone(),
                        _ => String::new(),
                    },
                );
                let choices = branch_view
                    .branches
                    .iter()
                    .map(|branch| Choice {
                        id: branch.name.clone(),
                        label: branch.name.clone(),
                        description: branch.badge.clone(),
                        icon: Some("git-branch"),
                        selected: branch.selected,
                    })
                    .collect::<Vec<_>>();
                if choices.is_empty() {
                    Some(
                        Row::new("Base branch")
                            .control(Input::new(&self.settings.scheduled_tasks.branch).small())
                            .render(),
                    )
                } else {
                    Some(
                        Row::new("Base branch")
                            .control(
                                select_sized(
                                    "scheduled-base-branch",
                                    match &draft.workspace {
                                        ScheduledTaskWorkspaceDraft::Worktree {
                                            base_ref, ..
                                        } => base_ref.clone(),
                                        _ => String::new(),
                                    },
                                    choices,
                                    260.,
                                    |view, choice, _, cx| {
                                        if let Some(ScheduledTaskWorkspaceDraft::Worktree {
                                            base_ref,
                                            ..
                                        }) = view
                                            .settings
                                            .scheduled_tasks
                                            .draft
                                            .as_mut()
                                            .map(|draft| &mut draft.workspace)
                                        {
                                            *base_ref = choice;
                                        }
                                        cx.notify();
                                    },
                                    cx,
                                )
                                .into_any_element(),
                            )
                            .render(),
                    )
                }
            }
            "existing_worktree" => Some(
                Row::new("Worktree path")
                    .control(Input::new(&self.settings.scheduled_tasks.worktree).small())
                    .render(),
            ),
            _ => None,
        };
        let selected = draft_id;
        let save = Button::new("scheduled-task-save")
            .primary()
            .small()
            .disabled(self.settings.scheduled_tasks.stale)
            .label("Save")
            .on_click(cx.listener(|view, _, _, cx| view.save_scheduled_task(cx)));
        let delete = selected.as_ref().map(|id| {
            let id = id.clone();
            Button::new("scheduled-task-delete")
                .ghost()
                .small()
                .text_color(color("error"))
                .label("Delete")
                .on_click(cx.listener(move |view, _, _, _| {
                    view.perform(Intent::DeleteScheduledTask { id: id.clone() });
                    view.select_scheduled_task(None);
                }))
        });
        let mut rows = editor_rows;
        if let Some(branch) = branch_row {
            rows.push(branch);
        }
        if self.settings.scheduled_tasks.stale {
            rows.push(
                Row::new("Status")
                    .control(
                        div()
                            .text_color(color("error"))
                            .child("This task was deleted elsewhere. Close this editor and start again.")
                            .into_any_element(),
                    )
                    .render(),
            );
        }
        if let Some(error) = self.settings.scheduled_tasks.error.clone() {
            rows.push(
                Row::new("Error")
                    .control(
                        div()
                            .text_color(color("error"))
                            .child(error)
                            .into_any_element(),
                    )
                    .render(),
            );
        }
        section(
            Some(
                if selected.is_some() {
                    "Edit scheduled task"
                } else {
                    "New scheduled task"
                }
                .into(),
            ),
            None,
            Some(
                h_flex()
                    .gap_2()
                    .children(delete.into_iter().map(|button| button.into_any_element()))
                    .child(save.into_any_element())
                    .into_any_element(),
            ),
            rows,
        )
        .into_any_element()
    }
}

//! The shared setup card: clients draw the same observed stages and actions.
use super::*;
use agent_protocol::workspace::{SetupPhase, SetupStageId, SetupStageStatus, WorkspaceSetup};

pub(super) fn project(
    language: UiLanguage,
    thread: &models::Thread,
    setup: &WorkspaceSetup,
) -> PreparationPresentation {
    let startup = thread.preparations.first();
    let script_failed = setup.stages.iter().any(|stage| {
        stage.id == SetupStageId::SetupScript && stage.status == SetupStageStatus::Failed
    });
    let text = match setup.phase {
        SetupPhase::Running => "ワークツリーを準備中…",
        SetupPhase::Done if script_failed => "ワークツリーの準備完了（設定スクリプトは失敗）",
        SetupPhase::Done => "ワークツリーの準備完了",
        SetupPhase::Failed => "ワークツリーの準備に失敗しました",
        SetupPhase::Cancelled => "ワークツリーの準備を停止しました",
    };
    let retry = startup
        .filter(|startup| {
            matches!(
                startup.status,
                agent_protocol::session::PreparationStatus::Failed { .. }
                    | agent_protocol::session::PreparationStatus::Interrupted
            )
        })
        .map(|startup| agent_protocol::operations::RetryPreparation {
            thread_id: startup.submission.thread_id.clone(),
            input_id: startup.submission.client_user_message_id.clone(),
            work_locally: false,
        });
    let cancel = startup
        .filter(|startup| {
            startup.status == agent_protocol::session::PreparationStatus::Preparing
                && setup.can_cancel
                && setup.phase == SetupPhase::Running
        })
        .map(|startup| agent_protocol::operations::Interrupt {
            thread_id: startup.submission.thread_id.clone(),
            turn_id: startup.submission.client_user_message_id.to_string().into(),
        });
    let work_locally = startup
        .filter(|startup| {
            startup.status != agent_protocol::session::PreparationStatus::Unknown
                && matches!(
                    startup.stage,
                    agent_protocol::session::PreparationStage::Workspace
                        | agent_protocol::session::PreparationStage::Worktree
                        | agent_protocol::session::PreparationStage::SetupScript
                )
                && (setup.can_cancel || retry.is_some())
        })
        .map(|startup| agent_protocol::operations::RetryPreparation {
            thread_id: startup.submission.thread_id.clone(),
            input_id: startup.submission.client_user_message_id.clone(),
            work_locally: true,
        });
    let steps = setup
        .stages
        .iter()
        .map(|stage| {
            let (id, label) = match stage.id {
                SetupStageId::Fetch => ("fetch", "ベースブランチを取得"),
                SetupStageId::Checkout => ("checkout", "ファイルをチェックアウト"),
                SetupStageId::Submodules => ("submodules", "サブモジュールを初期化"),
                SetupStageId::SetupScript => ("setup-script", "設定スクリプトを実行"),
                SetupStageId::Agent => ("agent", "エージェントを開始"),
            };
            let state = match stage.status {
                SetupStageStatus::Pending => "pending",
                SetupStageStatus::Running => "running",
                SetupStageStatus::Done => "done",
                SetupStageStatus::Skipped => "skipped",
                SetupStageStatus::Warning => "warning",
                SetupStageStatus::Failed => "failed",
            };
            PreparationStep {
                id: id.into(),
                label: if stage.id == SetupStageId::SetupScript {
                    setup.script.as_ref().map(|script| script.name.clone())
                } else {
                    None
                }
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| language.text(label).into()),
                state: state.into(),
                detail: stage
                    .exit_code
                    .map(|code| {
                        localized_ui_format(
                            language,
                            "終了コード: {0}".into(),
                            vec![code.to_string()],
                        )
                    })
                    .or_else(|| stage.detail.clone())
                    .or_else(|| {
                        if stage.status == SetupStageStatus::Skipped {
                            Some(language.text("スキップ済み").into())
                        } else if stage.id == SetupStageId::Checkout
                            && stage.status == SetupStageStatus::Running
                        {
                            stage.percent.map(|percent| format!("{percent}%"))
                        } else {
                            None
                        }
                    }),
                started_at_ms: stage.started_at_ms,
                ended_at_ms: stage.ended_at_ms.or(setup.ended_at_ms),
                tail: if matches!(
                    stage.status,
                    SetupStageStatus::Running | SetupStageStatus::Failed
                ) {
                    stage.tail.clone()
                } else {
                    Vec::new()
                },
            }
        })
        .collect();
    let mut details = Vec::new();
    for (label, value) in [
        ("ブランチ", setup.branch.as_ref()),
        ("ベース", setup.base_ref.as_ref()),
        ("パス", setup.worktree_path.as_ref()),
        (
            "セットアップ",
            setup.script.as_ref().map(|script| &script.command),
        ),
    ] {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            details.push(PreparationDetail {
                label: language.text(label).into(),
                value: value.clone(),
            });
        }
    }
    PreparationPresentation {
        text: language.text(text).into(),
        in_progress: setup.phase == SetupPhase::Running,
        shimmer: setup.phase == SetupPhase::Running,
        retry,
        steps,
        show_steps: setup.phase != SetupPhase::Done,
        details,
        started_at_ms: Some(setup.started_at_ms),
        ended_at_ms: setup.ended_at_ms,
        cancel,
        terminal: setup
            .terminal_handle
            .as_ref()
            .zip(setup.worktree_path.as_ref())
            .map(|(handle, cwd)| SetupTerminal {
                handle: handle.clone(),
                cwd: cwd.clone(),
            }),
        work_locally,
        error: setup.error.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::{
        operations::Submission,
        session::{
            PreparationStage, PreparationStatus, ProviderKind, SessionPreparation, SessionRef,
        },
    };

    fn thread() -> models::Thread {
        let id = SessionRef::new(ProviderKind::Codex, "admitted".into()).unwrap();
        models::Thread {
            id: Some(id.clone()),
            preparations: vec![SessionPreparation {
                submission: Submission {
                    thread_id: id,
                    client_user_message_id: "initial".into(),
                    input: Vec::new(),
                    model: None,
                    effort: None,
                    service_tier: None,
                    follow_up: Default::default(),
                },
                status: PreparationStatus::Preparing,
                stage: PreparationStage::Worktree,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn card_exposes_observed_steps_details_and_only_actions_that_can_run() {
        let mut source = thread();
        let mut setup = WorkspaceSetup::new(Some("origin/main".into()), 1000);
        setup.worktree_path = Some("/workspace".into());
        setup.branch = Some("feature".into());
        setup.terminal_handle = Some("setup-terminal".into());
        setup.stage(SetupStageId::Fetch, SetupStageStatus::Skipped, 1100);
        setup.stage(SetupStageId::Checkout, SetupStageStatus::Running, 1200);
        setup.stages[1].percent = Some(37);
        let card = project(UiLanguage::English, &source, &setup);
        assert_eq!(card.text, "Setting up worktree…");
        assert_eq!(
            card.steps
                .iter()
                .map(|step| step.label.as_str())
                .collect::<Vec<_>>(),
            [
                "Fetch base branch",
                "Check out files",
                "Run setup script",
                "Start agent"
            ]
        );
        assert_eq!(card.steps[0].detail.as_deref(), Some("skipped"));
        assert_eq!(card.steps[1].detail.as_deref(), Some("37%"));
        assert_eq!(card.cancel.unwrap().turn_id.as_str(), "initial");
        assert!(card.work_locally.unwrap().work_locally);
        assert!(card.retry.is_none());
        assert_eq!(card.terminal.unwrap().cwd, "/workspace");
        assert_eq!(
            card.details
                .iter()
                .map(|detail| detail.label.as_str())
                .collect::<Vec<_>>(),
            ["Branch", "Base", "Path"]
        );
        source.preparations.first_mut().unwrap().status = PreparationStatus::Unknown;
        let uncertain = project(UiLanguage::English, &source, &setup);
        assert!(uncertain.cancel.is_none());
        assert!(uncertain.retry.is_none());
        assert!(uncertain.work_locally.is_none());
        for status in [
            PreparationStatus::Interrupted,
            PreparationStatus::Failed {
                error: Default::default(),
            },
        ] {
            source.preparations.first_mut().unwrap().status = status;
            let stopped = project(UiLanguage::English, &source, &setup);
            assert!(stopped.retry.is_some());
            assert!(stopped.work_locally.unwrap().work_locally);
            assert!(stopped.cancel.is_none());
        }
    }

    #[test]
    fn script_failure_after_agent_start_remains_visible_as_a_completed_warning_and_elapsed_time_freezes()
     {
        let mut source = thread();
        source.preparations.clear();
        source.turns = Some(vec![Arc::new(models::Turn {
            status: models::TurnStatus::Running,
            ..Default::default()
        })]);
        let mut setup = WorkspaceSetup::new(None, 1000);
        setup.stage(SetupStageId::SetupScript, SetupStageStatus::Failed, 2000);
        setup.stages[2].exit_code = Some(9);
        setup.append_output(SetupStageId::SetupScript, "setup output");
        setup.finish(SetupPhase::Done, None, 4000);
        source.workspace_setup = Some(setup.clone());
        let graph = project_conversation(
            UiLanguage::English,
            &Default::default(),
            &Default::default(),
            Arc::new(source.clone()),
            &None,
        );
        let ConversationRowContent::Preparation { preparation } = &graph.session_rows[0].content
        else {
            panic!("setup must remain alongside the running turn")
        };
        assert_eq!(preparation.text, "Worktree ready, setup script failed");
        assert!(!preparation.show_steps);
        assert!(!preparation.in_progress);
        assert!(preparation.retry.is_none());
        assert_eq!(
            preparation.steps[2].detail.as_deref(),
            Some("exited with 9")
        );
        assert_eq!(preparation.steps[2].tail, ["setup output"]);
        assert_eq!(
            preparation_elapsed(UiLanguage::English, 1000, Some(4000), 9000),
            "3.0s"
        );
        assert_eq!(
            preparation_elapsed(UiLanguage::English, 1000, None, 1999),
            ""
        );
    }
}

use super::*;

fn later(seconds: i64) -> Timestamp {
    Timestamp::from_millis(at().millis() + seconds * 1000).unwrap()
}
fn command_at(s: &mut State, id: &CommandId, at: Timestamp, command: Command) -> Step {
    let result = ThreadMachine::step(
        s,
        &InputEnvelope {
            at,
            key: id.to_string(),
            input: Input::Command {
                id: id.clone(),
                command: Box::new(command),
                receipt: None,
            },
        },
    );
    *s = fold(s, &result.facts).unwrap();
    result
}
/// Commands are replayed by id, like the actor's receipts.
struct Thread {
    state: State,
    receipts: BTreeSet<CommandId>,
}
impl Thread {
    fn dispatch(&mut self, at: Timestamp, id: &CommandId, command: Command) -> Reply {
        if !self.receipts.insert(id.clone()) {
            return Reply::Ignored;
        }
        command_at(&mut self.state, id, at, command).reply
    }
    fn update(&mut self, key: &str, at: Timestamp, recovery: LimitRecoveryUpdate) -> Reply {
        self.dispatch(
            at,
            &CommandId::new(key).unwrap(),
            Command::UpdateMetadata {
                title: None,
                regenerate_title: None,
                branch: None,
                worktree_path: None,
                expected_worktree_path: None,
                expected_empty: false,
                limit_recovery: Some(Some(recovery)),
                linked_pull_request: None,
                project_root: None,
            },
        )
    }
    fn shell(&self) -> ThreadShell {
        shell(&self.state).unwrap()
    }
}
fn choice(run: &RunId, auto_resume: Option<bool>, snooze: Option<bool>) -> LimitRecoveryUpdate {
    LimitRecoveryUpdate {
        run: run.clone(),
        reset_at: later(60),
        auto_resume,
        snooze,
    }
}
/// A Codex run stopped by a usage limit whose reset is a minute away.
fn limited(queued: bool) -> (Thread, RunId) {
    let mut s = state();
    let (run, attempt) = running(&mut s, "work");
    if queued {
        command(
            &mut s,
            "queued",
            send_message("queued", DispatchMode::QueueAfterActive),
        );
    }
    provider(
        &mut s,
        "limits",
        &attempt,
        ProviderEvent::RateLimits {
            resets_at: Some(later(60).millis() / 1000),
        },
    );
    provider(
        &mut s,
        "limit",
        &attempt,
        ProviderEvent::ItemFinished {
            key: "terminal-failure".into(),
            kind: ProviderItem::Error {
                message: "Plan limit reached.".into(),
                retry: None,
                code: Some("usageLimitExceeded".into()),
                class: Some("usage_limit".into()),
                retryable: None,
            },
            text: None,
            status: ItemStatus::Failed,
        },
    );
    provider(
        &mut s,
        "failed",
        &attempt,
        ProviderEvent::TurnFinished {
            status: RunStatus::Failed,
            native_head: None,
        },
    );
    (
        Thread {
            state: s,
            receipts: BTreeSet::new(),
        },
        run,
    )
}

// Guards a scheduled usage-limit continuation against each scenario this
// domain can express.
#[test]
fn a_scheduled_usage_limit_continuation_is_guarded() {
    let scenarios = [
        "resume",
        "queued-resume",
        "cancel",
        "rearm",
        "snooze-race",
        "new-message",
        "archive",
        "settle",
        "manual-snooze",
        "manual-snooze-after-recovery",
        "snooze-only",
        "snooze-resume",
        "cancel-resume-keep-snooze",
        "wake-preserve-resume",
        "independent-patches",
        "expired-snooze",
        "wake",
    ];
    for scenario in scenarios {
        let (mut t, run) = limited(scenario == "queued-resume");
        let now = at().millis();
        let shell = t.shell();
        assert_eq!(shell.usage_limit_reset_at, Some(later(60)), "{scenario}");
        if scenario == "queued-resume" {
            assert_eq!(
                t.dispatch(
                    at(),
                    &CommandId::new("resume-held").unwrap(),
                    Command::ResumeQueue
                ),
                Reply::Rejected {
                    reason: "usage-limited".into()
                }
            );
        }
        assert!(limit_recovery_command(&shell, false, false, now).is_none());
        let snooze = [
            "snooze-only",
            "manual-snooze-after-recovery",
            "snooze-resume",
            "wake",
            "cancel-resume-keep-snooze",
            "wake-preserve-resume",
        ]
        .contains(&scenario);
        let auto_resume = scenario != "snooze-only" && scenario != "wake";
        let (arm_id, arm) = limit_recovery_command(&shell, auto_resume, snooze, now).unwrap();
        assert_eq!(
            t.dispatch(at(), &arm_id, arm),
            Reply::Accepted,
            "{scenario}"
        );
        let mut armed = t.shell();
        assert_eq!(
            armed.limit_recovery,
            Some(LimitRecovery {
                request: Some(arm_id.clone()),
                run: run.clone(),
                reset_at: later(60),
                auto_resume,
                snooze,
            }),
            "{scenario}"
        );
        if snooze {
            assert_eq!(armed.snoozed_until, Some(later(60)));
        }
        if scenario == "cancel-resume-keep-snooze" || scenario == "wake-preserve-resume" {
            let keep = scenario == "cancel-resume-keep-snooze";
            t.update(
                "independent-choice",
                later(10),
                choice(&run, Some(!keep), Some(keep)),
            );
            armed = t.shell();
            if keep {
                assert_eq!(armed.snoozed_until, Some(later(60)));
                assert_eq!(armed.snoozed_at, Some(armed.updated_at.clone()));
                assert!(!armed.limit_recovery.as_ref().unwrap().auto_resume);
            } else {
                assert_eq!(
                    (armed.snoozed_until.clone(), armed.snoozed_at.clone()),
                    (None, None)
                );
                assert!(armed.limit_recovery.as_ref().unwrap().auto_resume);
            }
        }
        if scenario == "independent-patches" {
            t.update("patch-snooze", later(10), choice(&run, None, Some(true)));
            let recovery = t.shell().limit_recovery.unwrap();
            assert!(recovery.auto_resume && recovery.snooze);
            t.update("patch-cancel", later(20), choice(&run, Some(false), None));
            let current = t.shell();
            let recovery = current.limit_recovery.clone().unwrap();
            assert!(!recovery.auto_resume && recovery.snooze);
            assert_eq!(current.snoozed_until, Some(later(60)));
            assert_eq!(current.snoozed_at, Some(current.updated_at.clone()));
            t.update("patch-resume", later(20), choice(&run, Some(true), None));
            armed = t.shell();
            let recovery = armed.limit_recovery.clone().unwrap();
            assert!(recovery.auto_resume && recovery.snooze);
        }
        if scenario == "manual-snooze" || scenario == "manual-snooze-after-recovery" {
            t.dispatch(
                at(),
                &CommandId::new("manual-snooze").unwrap(),
                Command::Snooze {
                    until: Some(later(60)),
                },
            );
            t.update(
                "manual-cancel",
                at(),
                choice(&run, Some(false), Some(false)),
            );
            assert_eq!(
                t.state.thread.as_ref().unwrap().snoozed_until,
                Some(later(60))
            );
            t.dispatch(
                at(),
                &CommandId::new("manual-wake").unwrap(),
                Command::Snooze { until: None },
            );
            assert_eq!(t.state.thread.as_ref().unwrap().snoozed_until, None);
        }
        if scenario == "wake" {
            t.update("wake", at(), choice(&run, Some(false), Some(false)));
            assert_eq!(t.state.thread.as_ref().unwrap().snoozed_until, None);
        }
        assert!(limit_recovery_command(&armed, true, false, now).is_none());
        let early = Command::Send(SendMessage {
            context: None,
            continuation: Some(Continuation::UsageLimit {
                run: run.clone(),
                recovery: None,
            }),
            ..match send_message("early", DispatchMode::StartImmediately) {
                Command::Send(message) => message,
                _ => unreachable!(),
            }
        });
        t.dispatch(at(), &CommandId::new("early").unwrap(), early);
        let runs = if scenario == "queued-resume" { 2 } else { 1 };
        assert_eq!(t.state.runs.len(), runs, "{scenario}");

        let reset = later(60);
        let resume = limit_recovery_command(&armed, true, false, reset.millis());
        assert_eq!(
            resume.is_some(),
            auto_resume && scenario != "cancel-resume-keep-snooze",
            "{scenario}"
        );
        if scenario == "snooze-race" {
            let (id, command) = resume.clone().unwrap();
            t.dispatch(
                reset.clone(),
                &CommandId::new("raced-snooze").unwrap(),
                Command::Snooze {
                    until: Some(later(120)),
                },
            );
            t.dispatch(reset.clone(), &id, command);
            assert_eq!(t.state.runs.len(), 1);
            let (fresh_id, fresh) =
                limit_recovery_command(&t.shell(), true, false, later(120).millis()).unwrap();
            assert_ne!(fresh_id, id);
            t.dispatch(later(120), &fresh_id, fresh.clone());
            t.dispatch(later(120), &fresh_id, fresh);
            assert_eq!(t.state.runs.len(), 2);
        }
        if scenario == "expired-snooze" {
            assert_eq!(
                t.update(
                    "expired-snooze",
                    reset.clone(),
                    choice(&run, None, Some(true))
                ),
                Reply::Rejected {
                    reason: "limit-reset-passed".into()
                }
            );
            let recovery = t.shell().limit_recovery.unwrap();
            assert!(recovery.auto_resume && !recovery.snooze);
        }
        if scenario == "cancel" || scenario == "rearm" {
            t.update("cancel", reset.clone(), choice(&run, Some(false), None));
        }
        if scenario == "archive" {
            t.dispatch(
                reset.clone(),
                &CommandId::new("archive").unwrap(),
                Command::Archive { archived: true },
            );
        }
        if scenario == "new-message" {
            t.dispatch(
                reset.clone(),
                &CommandId::new("new-message").unwrap(),
                send_message("new-message", DispatchMode::DeferStart),
            );
        }
        if scenario == "settle" {
            t.dispatch(
                reset.clone(),
                &CommandId::new("settle").unwrap(),
                Command::Settle {
                    settled: true,
                    at: None,
                },
            );
        }
        if scenario == "rearm" {
            let (id, command) = resume.clone().unwrap();
            t.dispatch(reset.clone(), &id, command);
            t.update("rearm", reset.clone(), choice(&run, Some(true), None));
            let (id, command) = resume.clone().unwrap();
            t.dispatch(reset.clone(), &id, command);
            assert_eq!(t.state.runs.len(), 1);
            let (fresh_id, fresh) =
                limit_recovery_command(&t.shell(), true, false, reset.millis()).unwrap();
            assert_ne!(fresh_id, id);
            t.dispatch(reset.clone(), &fresh_id, fresh.clone());
            t.dispatch(reset.clone(), &fresh_id, fresh);
            assert_eq!(t.state.runs.len(), 2);
        }
        let before = t.state.runs.len();
        if let Some((id, command)) = resume {
            t.dispatch(reset.clone(), &id, command.clone());
            t.dispatch(reset.clone(), &id, command);
        }
        let resumed = [
            "resume",
            "queued-resume",
            "snooze-resume",
            "wake-preserve-resume",
            "independent-patches",
            "expired-snooze",
        ]
        .contains(&scenario);
        assert_eq!(
            t.state.runs.len(),
            before + usize::from(resumed),
            "{scenario}"
        );
        if scenario == "queued-resume" {
            assert_eq!(t.state.runs[1].status, RunStatus::Queued);
            assert_eq!(t.state.runs[2].status, RunStatus::Starting);
        }
    }
}

// Resumes a stopped run manually once for an interrupted and a usage-limited
// run.
#[test]
fn a_stopped_run_is_resumed_manually_once() {
    for reason in ["interrupted", "usage_limit"] {
        let (mut t, source) = if reason == "usage_limit" {
            limited(true)
        } else {
            let mut s = state();
            let (run, attempt) = running(&mut s, "work");
            command(
                &mut s,
                "queued",
                send_message("queued", DispatchMode::QueueAfterActive),
            );
            command(
                &mut s,
                "stop",
                Command::Interrupt {
                    run: run.clone(),
                    hold_queue: true,
                    reason: None,
                },
            );
            provider(
                &mut s,
                "stopped",
                &attempt,
                ProviderEvent::TurnFinished {
                    status: RunStatus::Interrupted,
                    native_head: None,
                },
            );
            (
                Thread {
                    state: s,
                    receipts: BTreeSet::new(),
                },
                run,
            )
        };
        let mut scheduled = None;
        if reason == "usage_limit" {
            let (id, arm) = limit_recovery_command(&t.shell(), true, false, at().millis()).unwrap();
            t.dispatch(at(), &id, arm);
            scheduled = limit_recovery_command(&t.shell(), true, false, later(60).millis());
            assert!(scheduled.is_some());
        }
        let resume = |key: &str| {
            let Command::Send(mut message) = send_message(key, DispatchMode::StartImmediately)
            else {
                unreachable!()
            };
            message.continuation = Some(Continuation::Manual {
                run: source.clone(),
            });
            message.text = "Continue where you left off.".into();
            Command::Send(message)
        };
        t.dispatch(at(), &CommandId::new("first").unwrap(), resume("first"));
        assert_eq!(t.state.runs.len(), 3, "{reason}");
        assert_eq!(t.state.runs[1].status, RunStatus::Queued);
        assert_eq!(t.state.runs[2].status, RunStatus::Starting);
        assert_eq!(
            t.dispatch(at(), &CommandId::new("second").unwrap(), resume("second")),
            Reply::Rejected {
                reason: "continuation-unavailable".into()
            }
        );
        if let Some((id, command)) = scheduled {
            t.dispatch(later(60), &id, command);
        }
        assert_eq!(t.state.runs.len(), 3);
    }
}

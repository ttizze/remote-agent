use super::*;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum Mode {
    Saved,
    Local,
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
enum Action {
    Lost,
    Edit,
    Choose,
    Restart,
    WrongScope,
    WrongEpoch,
    WrongReceipt,
    Rejected,
    Reply,
}

#[derive(Deserialize)]
struct Step {
    action: Action,
    expected: Value,
}

#[derive(Deserialize)]
struct Trace {
    mode: Mode,
    restore: bool,
    accepted: bool,
    steps: Vec<Step>,
}

struct Device {
    snapshot: Snapshot,
    session: SessionRef,
    input: ClientInputId,
    receipt: uuid::Uuid,
}

fn draft(text: &str, files: &[&str], fresh_context: bool) -> Draft {
    Draft {
        text: text.into(),
        attachments: files
            .iter()
            .map(|&path| crate::state::Attachment {
                path: path.into(),
                name: path.into(),
                is_image: false,
            })
            .collect(),
        fresh_context,
        ..Default::default()
    }
}

impl Device {
    fn new(mode: &Mode) -> Self {
        let mut snapshot = Snapshot::default();
        let session = SessionRef::new(None, uuid::Uuid::new_v4().to_string()).unwrap();
        let input: ClientInputId = uuid::Uuid::new_v4().to_string().into();
        let receipt = uuid::Uuid::new_v4();
        let key = DraftKey::from(&session);
        let mut original = draft("sent", &["shared", "original"], true);
        original.model = Some(crate::models::ModelRef {
            provider: crate::session::ProviderKind::Codex,
            id: "model".into(),
        });
        let original = Arc::new(original);
        match mode {
            Mode::Local => {
                Arc::make_mut(&mut snapshot.input.drafts).insert(key.clone(), original.clone());
                let (next, effects) = crate::state::submission(
                    &snapshot,
                    Destination {
                        thread_id: None,
                        cwd: None,
                        draft_key: key,
                    },
                    original,
                    input.clone(),
                    None,
                    agent_protocol::operations::FollowUp::Queue,
                );
                assert_eq!(effects.len(), 1);
                snapshot = next;
            }
            Mode::Saved => {
                Arc::make_mut(&mut snapshot.input.input_recoveries).insert(
                    input.clone(),
                    Arc::new(InputRecovery {
                        session: session.clone(),
                        operation: receipt,
                        restore: true,
                        draft: original,
                    }),
                );
            }
        }
        Self {
            snapshot,
            session,
            input,
            receipt,
        }
    }

    fn observe(&self) -> Value {
        let input = &self.snapshot.input;
        let phase = if let Some(saved) = input.input_recoveries.get(&self.input) {
            assert_eq!(saved.operation, self.receipt);
            if saved.restore { "restore" } else { "discard" }
        } else if let Some(pending) = self.snapshot.sessions.pending_submissions.get(&self.input) {
            match (pending.accepted, pending.delivery_unknown, pending.cursor) {
                (false, false, None) => "sending",
                (false, true, None) => "uncertain",
                (true, false, Some(10)) => "accepted",
                other => panic!("inconsistent submission: {other:?}"),
            }
        } else {
            "settled"
        };
        let composer = input
            .drafts
            .get(&DraftKey::from(&self.session))
            .cloned()
            .unwrap_or_default();
        let files: Vec<_> = composer.attachments.iter().map(|file| &file.path).collect();
        json!({"phase":phase, "composer":{"text":composer.text, "files":files, "fresh":composer.fresh_context}})
    }

    fn apply(&mut self, trace: &Trace, action: &Action) {
        match action {
            Action::Lost if matches!(trace.mode, Mode::Local) => {
                self.snapshot = crate::state::reduce_event(
                    &self.snapshot,
                    Event::SubmissionUnknown(self.input.clone()),
                )
                .0;
            }
            Action::Lost => assert_eq!(reconnect_input_recovery(&self.snapshot).len(), 1),
            Action::Edit => {
                Arc::make_mut(&mut self.snapshot.input.drafts).insert(
                    DraftKey::from(&self.session),
                    Arc::new(draft("new typing", &["new", "shared"], false)),
                );
            }
            Action::Choose => assert!(
                resolve_saved_input(&mut self.snapshot, &self.input, trace.restore).is_some()
            ),
            Action::Restart => {
                self.snapshot = crate::persistence::decode(
                    &crate::persistence::encode(&self.snapshot).unwrap(),
                )
                .unwrap();
            }
            _ => self.reply(trace, action),
        }
    }

    fn reply(&mut self, trace: &Trace, action: &Action) {
        let mut scope = self.snapshot.connection.storage_scope.clone();
        if *action == Action::WrongScope {
            scope.push_str("-other");
        }
        let ack = records::Acknowledged {
            sequence: 10,
            accepted_at: String::new(),
            error: (!trace.accepted || *action == Action::Rejected).then(|| "not sent".into()),
        };
        match trace.mode {
            Mode::Saved => {
                let operation = ResolveSavedInput {
                    session: self.session.clone(),
                    input: self.input.clone(),
                    restore: true,
                    operation: if *action == Action::WrongReceipt {
                        uuid::Uuid::new_v4()
                    } else {
                        self.receipt
                    },
                };
                let epoch =
                    self.snapshot.connection.epoch + u64::from(*action == Action::WrongEpoch);
                let result =
                    operation.complete(&mut self.snapshot, "", ((scope, epoch), ack), true);
                if *action == Action::Rejected {
                    assert!(result.is_err());
                } else {
                    assert!(result.unwrap().is_empty());
                }
            }
            Mode::Local => {
                let operation = ResolveLocalInput {
                    input: self.input.clone(),
                    restore: trace.restore,
                };
                let (_, session) = operation.capture(&self.snapshot, "").unwrap();
                let effects = operation.apply(&mut self.snapshot, "", ((scope, session), ack));
                assert_eq!(
                    effects.len(),
                    usize::from(*action == Action::Reply && trace.accepted)
                );
            }
        }
    }
}

#[test]
#[ignore = "requires generated Lean recovery cases"]
fn lean_core_recovery_conformance() {
    let dir = std::env::var_os("AGENT_CORE_CASES").expect("Lean case directory required");
    let cases =
        std::fs::read_to_string(std::path::PathBuf::from(dir).join("recovery.jsonl")).unwrap();
    assert!(!cases.is_empty());
    for (index, line) in cases.lines().enumerate() {
        let trace: Trace = serde_json::from_str(line).unwrap();
        assert!(!trace.steps.is_empty());
        let mut device = Device::new(&trace.mode);
        for step in &trace.steps {
            device.apply(&trace, &step.action);
            assert_eq!(
                device.observe(),
                step.expected,
                "trace {index}: {:?}",
                step.action
            );
        }
    }
}

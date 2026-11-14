use super::*;
pub use agent_protocol::triage::Prepare as PrepareTriage;

impl Operation for PrepareTriage {
    rpc_operation!();
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Triage)
    }
    fn prepare(&mut self, _: &mut Snapshot, _: &str) -> Result<(), String> {
        self.validate().map_err(str::to_owned)
    }
    fn apply(self, snapshot: &mut Snapshot, pane: &str, output: Self::Output) -> Vec<Effect> {
        let key = snapshot
            .input
            .draft_slot(format!("triage:{}", output.context.id));
        let cwd = output.context.directory.clone();
        Arc::make_mut(&mut snapshot.input.drafts).insert(
            key.clone(),
            Arc::new(Draft {
                text: output.prompt,
                triage: Some(Box::new(output.context)),
                ..Default::default()
            }),
        );
        snapshot.invalidate_pane(pane);
        navigate(
            snapshot,
            pane,
            Navigation {
                cwd,
                draft_key: key,
                ..Default::default()
            },
        );
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preparing_diagnostics_keeps_the_original_draft_and_never_submits_a_turn() {
        let mut snapshot = Snapshot::default();
        let original = DraftKey::from("original");
        Arc::make_mut(&mut snapshot.input.drafts).insert(
            original.clone(),
            Arc::new(Draft {
                text: "作業中の入力".into(),
                ..Default::default()
            }),
        );
        let context = agent_protocol::triage::Context {
            machine_name: "Fixture PC".into(),
            cli_command: "host-daemon triage --output /triage".into(),
            id: "report".into(),
            directory: "/triage/report".into(),
            facts_path: "/triage/report/facts.json".into(),
            playbook_path: "/triage/report/PLAYBOOK.md".into(),
            source: None,
            version: "test".into(),
            revision: None,
        };
        let request = PrepareTriage {
            source: None,
            description: "症状".into(),
        };
        let effects = request.apply(
            &mut snapshot,
            "",
            agent_protocol::triage::Prepared {
                context: context.clone(),
                prompt: "調査用入力".into(),
            },
        );
        assert!(effects.is_empty());
        assert_eq!(snapshot.input.drafts[&original].text, "作業中の入力");
        let draft = &snapshot.input.drafts[&snapshot.panes.get("").unwrap().navigation.draft_key];
        assert_eq!(draft.triage.as_deref(), Some(&context));
        assert_eq!(draft.text, "調査用入力");
    }
}

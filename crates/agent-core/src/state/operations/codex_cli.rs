use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ManageCodexCli {
    pub update: bool,
}

impl Operation for ManageCodexCli {
    no_input!();
    type Output = rpc::CodexCliStatus;
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::CodexCli)
    }

    async fn run(&self, _: (), context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        let mut status = context
            .call(&rpc::CodexCliRequest {
                update: self.update,
            })
            .await?;
        while status.running {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            status = context
                .call(&rpc::CodexCliRequest { update: false })
                .await?;
        }
        Ok(status)
    }

    fn apply(self, snapshot: &mut Snapshot, status: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.account).codex_cli = Some(Arc::new(status));
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[allow(dead_code)]
    mod host_fixture {
        include!("../../../tests/support/host.rs");
    }

    #[tokio::test]
    async fn update_polls_without_restarting_installation_and_publishes_failure() {
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            let initial = Snapshot::default();
            let (peer, mut reader, writer) = host_fixture::connect(&initial).await;
            let store = crate::store::Store::new(peer, initial);
            for _ in 0..4 {
                let request = reader.read_request().await.unwrap().unwrap();
                let result = match request["method"].as_str().unwrap() {
                    "host/taskActivity/read" => json!({"revision":0,"statuses":[],"display":agent_protocol::live_activity::TaskActivitySummary::default().display()}),
                    "host/session/list" => json!({"data":[],"projects":[],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5}),
                    "host/model/list" => json!({"data":[],"nextCursor":null}),
                    "host/account/list" => json!({"accounts":[],"selected":{}}),
                    method => panic!("unexpected bootstrap: {method}"),
                };
                writer.reply(&request, json!({"result": result})).await.unwrap();
            }
            let receipt = store.dispatch(Intent::ManageCodexCli(ManageCodexCli { update: true }));
            let request = reader.read_request().await.unwrap().unwrap();
            assert_eq!(request["method"], "host/codex/cli");
            assert_eq!(request["params"]["update"], true);
            writer.reply(&request, json!({"result":{"running":true,"version":null,"restartRequired":false,"error":null}})).await.unwrap();
            let poll = reader.read_request().await.unwrap().unwrap();
            assert_eq!(poll["method"], "host/codex/cli");
            assert_eq!(poll["params"]["update"], false);
            writer.reply(&poll, json!({"result":{"running":false,"version":null,"restartRequired":false,"error":"update denied"}})).await.unwrap();
            receipt.await.unwrap();
            let snapshot = store.snapshot();
            let status = snapshot.account.codex_cli.as_ref().unwrap();
            assert_eq!(status.error.as_deref(), Some("update denied"));
            assert!(!status.restart_required);
            assert!(!snapshot.operation_running(OperationKey::CodexCli));
            store.close().await.unwrap();
        }).await.expect("CLI update lifecycle deadline");
    }
}

use super::*;
pub use agent_protocol::live_activity::{
    ReadTaskActivity, RegisterLiveActivity, UnregisterLiveActivity,
};

impl Operation for ReadTaskActivity {
    rpc_operation!();
    const BACKGROUND: bool = true;
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::TaskActivity)
    }
    fn scheduling(&self) -> Scheduling {
        Scheduling::LatestTaskActivity
    }
    fn apply(self, snapshot: &mut Snapshot, state: Self::Output) -> Vec<Effect> {
        snapshot.accept_task_activity(state);
        Vec::new()
    }
}

#[cfg(test)]
mod task_activity_tests {
    use super::*;
    use serde_json::json;

    #[allow(dead_code)]
    mod host_fixture {
        include!("../../../tests/support/host.rs");
    }

    #[tokio::test]
    async fn activity_read_survives_navigation_and_failures_stay_out_of_the_recent_list() {
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let initial = Snapshot::default();
            let (peer, mut reader, writer) = host_fixture::connect(&initial).await;
            let store = crate::store::Store::new(peer, initial);
            let mut activity = None;
            for _ in 0..4 {
                let request = reader.read_request().await.unwrap().unwrap();
                let result = match request["method"].as_str().unwrap() {
                    "host/taskActivity/read" => { activity = Some(request); continue; }
                    "host/session/list" => json!({"data":[],"projects":[],"hasMore":false}),
                    "host/model/list" => json!({"data":[],"nextCursor":null}),
                    "host/account/list" => json!({"accounts":[],"selected":{}}),
                    method => panic!("unexpected bootstrap method {method}"),
                };
                writer.reply(&request, json!({"result":result})).await.unwrap();
            }
            let mut updates = store.subscribe();
            while store.snapshot().threads.is_none() || store.snapshot().account.accounts.is_none() {
                updates.changed().await.unwrap();
            }
            store.dispatch(Intent::NewChat { cwd: String::new() }).await.unwrap();
            let display = agent_protocol::live_activity::TaskActivitySummary { running: 1, ..Default::default() }.display();
            writer.reply(&activity.unwrap(), json!({"result":{"revision":1,"display":display}})).await.unwrap();
            while store.snapshot().task_activity.is_none() { updates.changed().await.unwrap(); }
            assert_eq!(store.snapshot().task_activity.as_ref().unwrap().display.current.total, 1);
            tokio::time::timeout(std::time::Duration::from_secs(5), store.dispatch(Intent::ReadTaskActivity(ReadTaskActivity {})))
                .await.expect("activity receipt waits for the provider").unwrap();
            let request = reader.read_request().await.unwrap().unwrap();
            assert_eq!(request["method"], "host/taskActivity/read");
            writer.reply(&request, json!({"error":{"code":"provider_failed","message":"fixture unavailable","delivery":"notSent"}})).await.unwrap();
            while store.snapshot().operation_running(OperationKey::TaskActivity) { updates.changed().await.unwrap(); }
            assert!(store.snapshot().error.is_none());
            assert_eq!(store.snapshot().task_activity.as_ref().unwrap().display.current.total, 1);
            assert!(store.snapshot().threads.is_some());
            store.close().await.unwrap();
        }).await.expect("activity lifecycle deadline");
    }
}

impl Operation for RegisterLiveActivity {
    rpc_operation!();
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::LiveActivity {
            activity_id: self.activity_id.clone(),
        })
    }
    fn prepare(&mut self, _: &mut Snapshot) -> Result<(), String> {
        self.validate().map_err(str::to_owned)
    }
    fn outcome(output: &mut Self::Output) -> Outcome {
        Outcome::LiveActivityRegistered {
            enabled: output.enabled,
        }
    }
}
impl Operation for UnregisterLiveActivity {
    rpc_operation!();
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::LiveActivity {
            activity_id: self.activity_id.clone(),
        })
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadHostName {}
rpc::rpc_method!(LoadHostName, HostName, |self| crate::models::Empty {});

impl Operation for LoadHostName {
    rpc_operation!();
    fn apply(self, snapshot: &mut Snapshot, name: Self::Output) -> Vec<Effect> {
        snapshot.host_name = Some(name);
        Vec::new()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateInvitation {}
rpc::rpc_method!(CreateInvitation, Invite, |self| crate::models::Empty {});

impl Operation for CreateInvitation {
    rpc_operation!(management.invitation);
}

pub use agent_protocol::operations::RemoveRemoteHost;

impl Operation for RemoveRemoteHost {
    rpc_operation!();
    fn apply(self, snapshot: &mut Snapshot, _output: Self::Output) -> Vec<Effect> {
        let Self { id } = self;
        Arc::make_mut(&mut snapshot.management)
            .remotes
            .retain(|host| host.id != id);
        Vec::new()
    }
}

pub use agent_protocol::operations::RevokeDevice;

impl Operation for RevokeDevice {
    rpc_operation!();
    fn apply(self, snapshot: &mut Snapshot, _output: Self::Output) -> Vec<Effect> {
        let Self { id } = self;
        if let Some(status) = Arc::make_mut(&mut snapshot.management).status.as_mut() {
            Arc::make_mut(status).devices.retain(|device| device != &id);
        }
        Vec::new()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadHostManagement {}
impl Operation for LoadHostManagement {
    no_input!();
    type Output = (HostStatus, Vec<RemoteHost>);

    async fn run(
        &self,
        _: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        let (status, remotes) = tokio::try_join!(
            context.client.call(&rpc::ReadHostStatus {}),
            context.client.call(&rpc::ListRemoteHosts {})
        )?;
        Ok((status, remotes))
    }
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        let management = Arc::make_mut(&mut snapshot.management);
        management.status = Some(Arc::new(output.0));
        management.remotes = output.1;
        Vec::new()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairRemoteHost {
    pub invitation: Invitation,
    pub name: String,
}

impl Operation for PairRemoteHost {
    no_input!();
    type Output = RemoteHost;
    const STALE_POLICY: StalePolicy = StalePolicy::Apply;
    async fn run(
        &self,
        _: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if now >= self.invitation.expires_at {
            return Err(PeerError::InvalidMessage("invitation expired".into()));
        }
        let local = context
            .session
            .ok_or_else(|| PeerError::InvalidMessage("pairing requires an iroh session".into()))?;
        let ticket = self.invitation.endpoint.parse().map_err(
            |error: crate::transport::TransportError| PeerError::InvalidMessage(error.to_string()),
        )?;
        {
            let remote = scopeguard::guard(
                local
                    .connect(&ticket)
                    .await
                    .map_err(|error| PeerError::ConnectionClosed(error.to_string()))?,
                |session| session.close(),
            );
            let (peer, _events) = remote
                .open_peer(std::time::Duration::from_secs(20), 8)
                .await
                .map_err(|error| PeerError::ConnectionClosed(error.to_string()))?;
            peer.call(&rpc::Pair {
                invitation: self.invitation.invitation,
            })
            .await?;
            peer.close().await;
        }
        context
            .call(&rpc::RegisterRemoteHost {
                ticket: self.invitation.endpoint.clone(),
                name: self.name.clone(),
            })
            .await
    }
    fn apply(self, snapshot: &mut Snapshot, host: Self::Output) -> Vec<Effect> {
        let management = Arc::make_mut(&mut snapshot.management);
        if let Some(current) = management
            .remotes
            .iter_mut()
            .find(|current| current.id == host.id)
        {
            *current = host;
        } else {
            management.remotes.push(host);
        }
        Vec::new()
    }
    fn outcome(output: &mut Self::Output) -> Outcome {
        Outcome::RemoteHostPaired {
            id: output.id.clone(),
        }
    }
}

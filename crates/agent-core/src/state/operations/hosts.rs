use super::*;
pub use agent_protocol::live_activity::{
    ReadTaskActivity, RegisterLiveActivity, UnregisterLiveActivity,
};

impl Operation for ReadTaskActivity {
    rpc_operation!();
    fn apply(self, snapshot: &mut Snapshot, state: Self::Output) -> Vec<Effect> {
        snapshot.accept_task_activity(state);
        Vec::new()
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

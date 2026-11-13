use super::*;
pub use agent_protocol::operations::ReadHostSoftware;

impl Operation for ReadHostSoftware {
    rpc_operation!();
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::HostSoftware)
    }
    fn scheduling(&self) -> Scheduling {
        Scheduling::LatestHostState(OperationKey::HostSoftware)
    }
    fn apply(self, snapshot: &mut Snapshot, _pane_id: &str, output: Self::Output) -> Vec<Effect> {
        snapshot.catalog.host_software = Some(Arc::new(output));
        Vec::new()
    }
}
pub use agent_protocol::live_activity::{
    MarkSessionRead, RegisterLiveActivity, UnregisterLiveActivity,
};

impl Operation for RegisterLiveActivity {
    rpc_operation!();
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::LiveActivity {
            activity_id: self.activity_id.clone(),
        })
    }
    fn prepare(&mut self, _: &mut Snapshot, _pane_id: &str) -> Result<(), String> {
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
            activity_id: Some(self.activity_id.clone()),
        })
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadHostName {}
rpc::rpc_method!(LoadHostName, HostName, |self| crate::models::Empty {});

impl Operation for LoadHostName {
    rpc_operation!();
    const STALE_POLICY: StalePolicy = StalePolicy::Apply;
    fn apply(self, snapshot: &mut Snapshot, _pane_id: &str, name: Self::Output) -> Vec<Effect> {
        snapshot.connection.host_name = Some(name);
        Vec::new()
    }
}

pub use agent_protocol::operations::CreateInvitation;

impl Operation for CreateInvitation {
    rpc_operation!(connection.management.invitation);
}

pub use agent_protocol::operations::RemoveRemoteHost;

impl Operation for RemoveRemoteHost {
    rpc_operation!();
    fn apply(self, snapshot: &mut Snapshot, _pane_id: &str, _output: Self::Output) -> Vec<Effect> {
        let Self { id } = self;
        Arc::make_mut(&mut snapshot.connection.management)
            .remotes
            .retain(|host| host.id != id);
        Vec::new()
    }
}

pub use agent_protocol::operations::RevokeDevice;

impl Operation for RevokeDevice {
    rpc_operation!();
    fn apply(self, snapshot: &mut Snapshot, _pane_id: &str, _output: Self::Output) -> Vec<Effect> {
        let Self { id } = self;
        if let Some(status) = Arc::make_mut(&mut snapshot.connection.management)
            .status
            .as_mut()
        {
            Arc::make_mut(status)
                .devices
                .retain(|device| device.id != id);
        }
        Vec::new()
    }
}

#[cfg(test)]
mod device_tests {
    use super::*;

    #[test]
    fn revocation_removes_only_the_selected_id_even_when_names_match() {
        let devices = ["phone", "tablet"].map(|id| agent_protocol::models::HostDevice {
            id: id.into(),
            name: Some("My device".into()),
        });
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut snapshot.connection.management).status = Some(Arc::new(HostStatus {
            node_id: "host".into(),
            name: "My PC".into(),
            devices: devices.to_vec(),
            provider_errors: None,
        }));
        RevokeDevice { id: "phone".into() }.apply(&mut snapshot, "", crate::models::Empty {});
        assert_eq!(
            snapshot
                .connection
                .management
                .status
                .as_ref()
                .unwrap()
                .devices,
            [devices[1].clone()]
        );
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
    fn apply(self, snapshot: &mut Snapshot, _pane_id: &str, output: Self::Output) -> Vec<Effect> {
        let management = Arc::make_mut(&mut snapshot.connection.management);
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
            return Err(PeerError::InvalidMessage(
                "招待の有効期限が切れました。".into(),
            ));
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
    fn apply(self, snapshot: &mut Snapshot, _pane_id: &str, host: Self::Output) -> Vec<Effect> {
        let management = Arc::make_mut(&mut snapshot.connection.management);
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

impl Operation for MarkSessionRead {
    rpc_operation!();
    const BACKGROUND: bool = true;
}

use super::*;

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateInvitation {}
impl rpc::RpcMethod for CreateInvitation {
    type Output = Invitation;
    const METHOD: &'static str = "host/invite";
}

impl Operation for CreateInvitation {
    rpc_operation!();
    fn apply(self, snapshot: &mut Snapshot, invitation: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.management).invitation = Some(Arc::new(invitation));
        Vec::new()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoveRemoteHost {
    pub id: String,
}
impl rpc::RpcMethod for RemoveRemoteHost {
    type Output = Map<String, Value>;
    const METHOD: &'static str = "host/removeRemote";
}

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

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevokeDevice {
    #[serde(rename = "node_id")]
    pub id: String,
}
impl rpc::RpcMethod for RevokeDevice {
    type Output = Map<String, Value>;
    const METHOD: &'static str = "host/revoke";
}

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
    type Output = (HostStatus, Vec<RemoteHost>);

    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        let (status, remotes) = tokio::try_join!(
            context.client.call(&rpc::ReadHostStatus {}),
            context.client.call(&rpc::ListRemoteHosts {})
        )?;
        Ok((status.value, remotes.value))
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

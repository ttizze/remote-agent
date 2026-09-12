use super::*;

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateInvitation {}
rpc::rpc_method!(CreateInvitation, Invitation, "host/invite");

impl Operation for CreateInvitation {
    rpc_operation!(management.invitation);
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoveRemoteHost {
    pub id: String,
}
rpc::rpc_method!(RemoveRemoteHost, Map<String, Value>, "host/removeRemote");

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
    #[serde(rename = "nodeId")]
    pub id: String,
}
rpc::rpc_method!(RevokeDevice, Map<String, Value>, "host/revoke");

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

impl Operation for PairRemoteHost {
    type Output = RemoteHost;
    const APPLY_WHEN_STALE: bool = true;
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
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
            let peer = remote
                .open_peer(std::time::Duration::from_secs(20), 8)
                .await
                .map_err(|error| PeerError::ConnectionClosed(error.to_string()))?;
            peer.request::<_, <rpc::Pair as rpc::RpcMethod>::Output>(
                <rpc::Pair as rpc::RpcMethod>::METHOD,
                &rpc::Pair {
                    invitation: self.invitation.invitation,
                },
            )
            .await?;
            peer.close().await?;
        }
        context
            .call(&rpc::RegisterRemoteHost {
                ticket: &self.invitation.endpoint,
                name: &self.name,
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

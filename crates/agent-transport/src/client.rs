mod connection;
use crate::peer::PeerError;
use agent_protocol::operations::RpcMethod;
pub(crate) use connection::{BLOB, CALL, CLOSE, EVENTS};
pub use connection::{Client, HostPeer, HostRequest, Updates};
impl Client {
    pub async fn call<O: RpcMethod>(&self, operation: &O) -> Result<O::Output, PeerError> {
        self.request(&operation.request()?).await
    }
}

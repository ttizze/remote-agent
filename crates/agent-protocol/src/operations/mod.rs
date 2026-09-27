//! Shared request records and their typed RPC contracts.
use crate::error::PeerError;
use serde::{Serialize, de::DeserializeOwned};

/// The method, parameters and result are one contract.
pub trait RpcMethod: Serialize {
    type Contract: crate::protocol::contracts::Contract<Output = Self::Output>;
    type Output: DeserializeOwned + Serialize;
    fn method(&self) -> &'static str {
        <Self::Contract as crate::protocol::contracts::Contract>::METHOD
    }
    fn params(
        &self,
    ) -> Result<<Self::Contract as crate::protocol::contracts::Contract>::Params, PeerError>;
    fn request(&self) -> Result<crate::protocol::Call, PeerError> {
        self.params()
            .map(<Self::Contract as crate::protocol::contracts::Contract>::call)
    }
    fn subscription(_output: &mut Self::Output, _id: uuid::Uuid) {}
    fn validate(&self, _output: &Self::Output) -> Result<(), &'static str> {
        Ok(())
    }
}
#[macro_export]
macro_rules! rpc_contract {
    ($variant:ident) => {
        type Contract = $crate::protocol::contracts::$variant;
        type Output = <$crate::protocol::contracts::$variant as $crate::protocol::contracts::Contract>::Output;
    };
}
pub use crate::rpc_contract;
#[macro_export]
macro_rules! rpc_method {
    ($name:ty, $variant:ident, |$this:ident| $params:expr) => {
        impl $crate::operations::RpcMethod for $name {
            $crate::operations::rpc_contract!($variant);
            fn params(&$this) -> Result<<Self::Contract as $crate::protocol::contracts::Contract>::Params, $crate::error::PeerError> {
                Ok($params)
            }
        }
    };
}
pub use crate::rpc_method;

mod accounts;
mod host;
mod requests;
mod terminal;
mod turns;
mod workspace;

pub use accounts::*;
pub use host::*;
pub use requests::*;
pub use terminal::*;
pub use turns::*;
pub use workspace::*;

pub fn validate_output<O: RpcMethod>(operation: &O, output: &O::Output) -> Result<(), PeerError> {
    operation
        .validate(output)
        .map_err(|reason| PeerError::InvalidResponse {
            method: operation.method().into(),
            reason: reason.into(),
            sequence: None,
            raw: serde_json::to_string(output).expect("wire output serializes"),
        })
}

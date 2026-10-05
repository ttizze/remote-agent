use super::service::Failure;
use agent_protocol::{models::Empty, operations as op};
/// Only account workflow values cross the provider boundary.
pub(crate) enum AccountCommand {
    Select { id: String },
    Logout { id: String },
    StartLogin,
    SubmitLogin { id: String, code: String },
    ReadLogin { id: String },
    CancelLogin { id: String },
}
pub(crate) enum AccountReply {
    Selection(op::AccountSelection),
    Login(op::AccountLogin),
    Status(op::AccountLoginStatus),
    Complete,
}
impl From<op::AccountSelection> for AccountReply {
    fn from(value: op::AccountSelection) -> Self {
        Self::Selection(value)
    }
}
impl From<op::AccountLogin> for AccountReply {
    fn from(value: op::AccountLogin) -> Self {
        Self::Login(value)
    }
}
impl From<op::AccountLoginStatus> for AccountReply {
    fn from(value: op::AccountLoginStatus) -> Self {
        Self::Status(value)
    }
}
impl From<Empty> for AccountReply {
    fn from(_: Empty) -> Self {
        Self::Complete
    }
}

#[async_trait::async_trait]
pub(crate) trait Identity: Send + Sync {
    async fn list(&self) -> Result<op::Accounts, Failure>;
    async fn account(&self, command: AccountCommand) -> Result<AccountReply, Failure>;
    async fn usage(&self, id: &str) -> Result<op::AccountUsage, Failure>;
}

use super::*;

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListAccounts {}
rpc::rpc_method!(ListAccounts, rpc::Accounts, "host/account/list");

impl Operation for ListAccounts {
    rpc_operation!(account.accounts);
}

pub use crate::client::SelectAccount;
rpc::rpc_method!(SelectAccount, rpc::AccountSelection, "host/account/select");

impl Operation for SelectAccount {
    rpc_operation!();
    const INVALIDATES: bool = true;
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        let rpc::AccountSelection {
            selected_id,
            provider,
            persistence_error,
            ..
        } = output;
        if let Some(accounts) = &mut Arc::make_mut(&mut snapshot.account).accounts {
            let accounts = Arc::make_mut(accounts);
            match provider {
                crate::session::ProviderKind::Codex => accounts.selected_id = Some(selected_id),
                crate::session::ProviderKind::Claude => {
                    accounts.selected_claude_id = Some(selected_id)
                }
            }
        }
        snapshot.error = persistence_error;
        vec![
            Effect::execute(ListAccounts {}),
            Effect::execute(LoadModels {}),
        ]
    }
}

pub use crate::client::{StartAccountLogin, SubmitAccountLogin};
rpc::rpc_method!(
    StartAccountLogin,
    rpc::AccountLogin,
    "host/account/login/start"
);

impl Operation for StartAccountLogin {
    rpc_operation!();
    const INVALIDATES: bool = true;
    fn apply(self, snapshot: &mut Snapshot, login: Self::Output) -> Vec<Effect> {
        let account = Arc::make_mut(&mut snapshot.account);
        account.login = Some(Arc::new(login));
        Vec::new()
    }
}

pub use crate::client::ReadAccountLogin;
rpc::rpc_method!(
    ReadAccountLogin,
    rpc::AccountLoginStatus,
    "host/account/login/status"
);

impl Operation for ReadAccountLogin {
    rpc_operation!();
    fn apply(self, snapshot: &mut Snapshot, status: Self::Output) -> Vec<Effect> {
        if status.completed {
            Arc::make_mut(&mut snapshot.account).login = None;
            if let Some(id) = status.account_id {
                let (updated, effects) = reduce(
                    snapshot,
                    Event::Intent(Intent::SelectAccount(SelectAccount { id })),
                );
                *snapshot = updated;
                return effects;
            }
            return vec![
                Effect::execute(ListAccounts {}),
                Effect::execute(LoadModels {}),
            ];
        }
        Vec::new()
    }
}

pub use crate::client::CancelAccountLogin;
rpc::rpc_method!(CancelAccountLogin, Map<String, Value>, "host/account/login/cancel");

impl Operation for CancelAccountLogin {
    rpc_operation!();
    const INVALIDATES: bool = true;
    fn apply(self, snapshot: &mut Snapshot, _output: Self::Output) -> Vec<Effect> {
        let account = Arc::make_mut(&mut snapshot.account);
        account.login = None;
        Vec::new()
    }
}

pub use crate::client::LogoutAccount;
rpc::rpc_method!(LogoutAccount, Map<String, Value>, "host/account/logout");

impl Operation for LogoutAccount {
    rpc_operation!();
    const INVALIDATES: bool = true;
    fn apply(self, snapshot: &mut Snapshot, _output: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.account).accounts = None;
        vec![
            Effect::execute(ListAccounts {}),
            Effect::execute(LoadModels {}),
        ]
    }
}

rpc::rpc_method!(SubmitAccountLogin, Map<String, Value>, "host/account/login/submit");
impl Operation for SubmitAccountLogin {
    rpc_operation!();
}

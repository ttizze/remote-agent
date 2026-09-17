use super::*;

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListAccounts {}
rpc::rpc_method!(
    ListAccounts,
    rpc::Accounts,
    "host/account/list",
    ListAccounts,
    |self| crate::models::Empty {}
);

impl Operation for ListAccounts {
    rpc_operation!(account.accounts);
}

pub use crate::client::SelectAccount;
rpc::rpc_method!(
    SelectAccount,
    rpc::AccountSelection,
    "host/account/select",
    SelectAccount,
    |self| self.clone()
);

impl Operation for SelectAccount {
    rpc_operation!();
    const INVALIDATES: bool = true;
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        let rpc::AccountSelection {
            selected_id,
            persistence_error,
            ..
        } = output;
        if let Some(accounts) = &mut Arc::make_mut(&mut snapshot.account).accounts {
            Arc::make_mut(accounts).selected_id = Some(selected_id);
        }
        snapshot.error = persistence_error;
        vec![
            Effect::execute(ListAccounts {}),
            Effect::execute(LoadModels {}),
        ]
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartAccountLogin {}
rpc::rpc_method!(
    StartAccountLogin,
    rpc::AccountLogin,
    "host/account/login/start",
    StartAccountLogin,
    |self| crate::models::Empty {}
);

impl Operation for StartAccountLogin {
    rpc_operation!();
    const INVALIDATES: bool = true;
    fn apply(self, snapshot: &mut Snapshot, login: Self::Output) -> Vec<Effect> {
        let account = Arc::make_mut(&mut snapshot.account);
        account.login = Some(Arc::new(login));
        account.login_status = None;
        Vec::new()
    }
}

pub use crate::client::ReadAccountLogin;
rpc::rpc_method!(
    ReadAccountLogin,
    rpc::AccountLoginStatus,
    "host/account/login/status",
    ReadAccountLogin,
    |self| self.clone()
);

impl Operation for ReadAccountLogin {
    rpc_operation!();
    fn apply(self, snapshot: &mut Snapshot, status: Self::Output) -> Vec<Effect> {
        let completed = status.completed;
        let account = Arc::make_mut(&mut snapshot.account);
        account.login_status = Some(Arc::new(status));
        if completed {
            account.login = None;
            let (updated, mut effects) = reduce(
                snapshot,
                Event::Intent(Intent::ListAccounts(ListAccounts {})),
            );
            *snapshot = updated;
            effects.push(Effect::execute(LoadModels {}));
            return effects;
        }
        Vec::new()
    }
}

pub use crate::client::CancelAccountLogin;
rpc::rpc_method!(
    CancelAccountLogin,
    crate::models::Empty,
    "host/account/login/cancel",
    CancelAccountLogin,
    |self| self.clone()
);

impl Operation for CancelAccountLogin {
    rpc_operation!();
    const INVALIDATES: bool = true;
    fn apply(self, snapshot: &mut Snapshot, _output: Self::Output) -> Vec<Effect> {
        let account = Arc::make_mut(&mut snapshot.account);
        account.login = None;
        account.login_status = None;
        Vec::new()
    }
}

pub use crate::client::LogoutAccount;
rpc::rpc_method!(
    LogoutAccount,
    crate::models::Empty,
    "host/account/logout",
    LogoutAccount,
    |self| self.clone()
);

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

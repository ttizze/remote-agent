use super::*;

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListAccounts {}
impl rpc::RpcMethod for ListAccounts {
    type Output = rpc::Accounts;
    const METHOD: &'static str = "host/account/list";
}

impl Operation for ListAccounts {
    rpc_operation!();
    fn apply(self, snapshot: &mut Snapshot, accounts: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.account).accounts = Some(Arc::new(accounts));
        Vec::new()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectAccount {
    #[serde(rename = "accountId")]
    pub id: String,
}
impl rpc::RpcMethod for SelectAccount {
    type Output = rpc::AccountSelection;
    const METHOD: &'static str = "host/account/select";
}

impl Operation for SelectAccount {
    rpc_operation!();
    fn invalidates(&self, _snapshot: &Snapshot) -> bool {
        true
    }
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
        vec![Effect::execute(LoadModels {})]
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartAccountLogin {}
impl rpc::RpcMethod for StartAccountLogin {
    type Output = rpc::AccountLogin;
    const METHOD: &'static str = "host/account/login/start";
}

impl Operation for StartAccountLogin {
    rpc_operation!();
    fn invalidates(&self, _snapshot: &Snapshot) -> bool {
        true
    }
    fn apply(self, snapshot: &mut Snapshot, login: Self::Output) -> Vec<Effect> {
        let account = Arc::make_mut(&mut snapshot.account);
        account.login = Some(Arc::new(login));
        account.login_status = None;
        Vec::new()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadAccountLogin {
    #[serde(rename = "loginId")]
    pub id: String,
}
impl rpc::RpcMethod for ReadAccountLogin {
    type Output = rpc::AccountLoginStatus;
    const METHOD: &'static str = "host/account/login/status";
}

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

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelAccountLogin {
    #[serde(rename = "loginId")]
    pub id: String,
}
impl rpc::RpcMethod for CancelAccountLogin {
    type Output = Map<String, Value>;
    const METHOD: &'static str = "host/account/login/cancel";
}

impl Operation for CancelAccountLogin {
    rpc_operation!();
    fn invalidates(&self, _snapshot: &Snapshot) -> bool {
        true
    }
    fn apply(self, snapshot: &mut Snapshot, _output: Self::Output) -> Vec<Effect> {
        let account = Arc::make_mut(&mut snapshot.account);
        account.login = None;
        account.login_status = None;
        Vec::new()
    }
}

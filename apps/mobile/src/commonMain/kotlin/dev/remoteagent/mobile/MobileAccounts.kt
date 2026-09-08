package dev.remoteagent.mobile

import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement

private val accountJson = Json { ignoreUnknownKeys = true }
private const val ACCOUNT_POLL_INTERVAL_MS = 2000L

/** Mobile lifecycle boundary; a connection change retires its native projection and polling job. */
internal fun MobileController.syncAccountHost() {
    val selected = state.selectedProfile?.id
    val generation = selected?.let(sessions::currentGeneration)
    if (accountHost == selected && accountGeneration == generation) return
    accountHost = selected
    accountGeneration = generation
    pauseAccountPolling()
    publishAccounts(AccountSettingsState())
}

internal fun MobileController.observeAccounts(observer: (AccountSettingsState) -> Unit): HostEventSubscription {
    syncAccountHost()
    accountObservers += observer
    observer(accountState)
    return HostEventSubscription { accountObservers -= observer }
}

private fun MobileController.publishAccounts(value: AccountSettingsState) {
    accountState = value
    accountObservers.toList().forEach { it(value) }
}

private suspend inline fun <reified T> MobileController.requestAccount(command: AgentCommand): GatewayResult<T> {
    val profile = state.selectedProfile ?: return GatewayResult.Failure("接続先が選択されていません")
    return requestAgent(profile.id, command).mapGateway { accountJson.decodeFromString<T>(it) }
}

internal suspend fun MobileController.refreshAccounts() {
    syncAccountHost()
    val selected = accountHost
    val token = accountGeneration
    val result = requestAccount<HostAccountList>(AgentCommand.Accounts)
    if (accountHost != selected || accountGeneration != token) return
    when (accountAction(accountState, AccountEvent.ListReply, failed = result is GatewayResult.Failure)) {
        AccountAction.ApplyList -> {
            val value = (result as GatewayResult.Success).value
            publishAccounts(
                accountState.copy(accounts = value.accounts, selectedId = value.selectedId, error = value.error)
            )
        }
        AccountAction.ShowError -> publishAccounts(accountState.copy(error = (result as GatewayResult.Failure).message))
        else -> error("Invalid account list transition")
    }
    resumeAccountPolling()
}

internal suspend fun MobileController.selectAccount(id: String) {
    syncAccountHost()
    if (
        accountAction(accountState, AccountEvent.Select, sameSelection = accountState.selectedId == id) ==
            AccountAction.Ignore
    )
        return
    val selected = accountHost
    val token = accountGeneration
    publishAccounts(accountState.copy(selecting = true, error = null))
    val result = requestAccount<HostAccountSelection>(AgentCommand.SelectAccount(id))
    if (accountHost != selected || accountGeneration != token) return
    when (accountAction(accountState, AccountEvent.SelectionReply, failed = result is GatewayResult.Failure)) {
        AccountAction.ApplySelection -> {
            val value = (result as GatewayResult.Success).value
            publishAccounts(
                accountState.copy(selecting = false, selectedId = value.selectedId, error = value.persistenceError)
            )
        }
        AccountAction.SelectionFailed ->
            publishAccounts(accountState.copy(selecting = false, error = (result as GatewayResult.Failure).message))
        else -> error("Invalid account selection transition")
    }
}

internal suspend fun MobileController.startAccountLogin() {
    syncAccountHost()
    if (accountAction(accountState, AccountEvent.StartLogin) == AccountAction.Ignore) return
    val selected = accountHost
    val token = accountGeneration
    publishAccounts(accountState.copy(startingLogin = true, error = null))
    val result = requestAccount<HostAccountLogin>(AgentCommand.StartAccountLogin)
    if (accountHost != selected || accountGeneration != token) return
    when (accountAction(accountState, AccountEvent.LoginReply, failed = result is GatewayResult.Failure)) {
        AccountAction.ApplyLogin -> {
            publishAccounts(accountState.copy(startingLogin = false, login = (result as GatewayResult.Success).value))
            resumeAccountPolling()
        }
        AccountAction.LoginFailed ->
            publishAccounts(accountState.copy(startingLogin = false, error = (result as GatewayResult.Failure).message))
        else -> error("Invalid account login transition")
    }
}

internal fun MobileController.pauseAccountPolling() {
    accountPolling?.cancel()
    accountPolling = null
}

internal fun MobileController.resumeAccountPolling() {
    syncAccountHost()
    val login = accountState.login ?: return
    if (accountPolling != null) return
    val selected = accountHost
    val token = accountGeneration
    publishAccounts(accountState.copy(error = null))
    accountPolling = persistenceScope.launch {
        while (true) {
            delay(ACCOUNT_POLL_INTERVAL_MS)
            val result = requestAccount<HostAccountLoginStatus>(AgentCommand.AccountLoginStatus(login.loginId))
            if (accountHost != selected || accountGeneration != token || accountState.login != login) return@launch
            val completed = (result as? GatewayResult.Success)?.value?.completed == true
            when (
                accountAction(
                    accountState,
                    AccountEvent.StatusReply,
                    failed = result is GatewayResult.Failure,
                    completed = completed,
                )
            ) {
                AccountAction.ShowError -> {
                    accountPolling = null
                    publishAccounts(accountState.copy(error = (result as GatewayResult.Failure).message))
                    return@launch
                }
                AccountAction.CompleteLogin -> {
                    accountPolling = null
                    publishAccounts(accountState.copy(login = null))
                    refreshAccounts()
                    (result as GatewayResult.Success).value.accountId?.let { selectAccount(it) }
                    return@launch
                }
                AccountAction.ContinuePolling -> Unit
                else -> error("Invalid account polling transition")
            }
        }
    }
}

internal suspend fun MobileController.cancelAccountLogin() {
    syncAccountHost()
    val login = accountState.login ?: return
    val selected = accountHost
    val token = accountGeneration
    pauseAccountPolling()
    val result = requestAccount<JsonElement>(AgentCommand.CancelAccountLogin(login.loginId))
    if (accountHost != selected || accountGeneration != token) return
    when (accountAction(accountState, AccountEvent.CancelReply, failed = result is GatewayResult.Failure)) {
        AccountAction.ClearLogin -> {
            publishAccounts(accountState.copy(login = null, error = null))
            refreshAccounts()
        }
        AccountAction.ShowError -> publishAccounts(accountState.copy(error = (result as GatewayResult.Failure).message))
        else -> error("Invalid account cancellation transition")
    }
}

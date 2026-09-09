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

private suspend inline fun <reified T> MobileController.requestAccount(command: AgentCommand): GatewayResult<T>? {
    val profile = state.selectedProfile ?: return GatewayResult.Failure("接続先が選択されていません")
    val selected = accountHost
    val token = accountGeneration
    val result = requestAgent(profile.id, command).mapGateway { accountJson.decodeFromString<T>(it) }
    return result.takeIf { accountHost == selected && accountGeneration == token }
}

internal suspend fun MobileController.refreshAccounts() {
    syncAccountHost()
    when (val result = requestAccount<HostAccountList>(AgentCommand.Accounts) ?: return) {
        is GatewayResult.Success ->
            publishAccounts(
                accountState.copy(
                    accounts = result.value.accounts,
                    selectedId = result.value.selectedId,
                    error = result.value.error,
                )
            )
        is GatewayResult.Failure -> publishAccounts(accountState.copy(error = result.message))
    }
    resumeAccountPolling()
}

internal suspend fun MobileController.selectAccount(id: String) {
    syncAccountHost()
    if (accountState.selecting || accountState.selectedId == id) return
    publishAccounts(accountState.copy(selecting = true, error = null))
    when (val result = requestAccount<HostAccountSelection>(AgentCommand.SelectAccount(id)) ?: return) {
        is GatewayResult.Success ->
            publishAccounts(
                accountState.copy(
                    selecting = false,
                    selectedId = result.value.selectedId,
                    error = result.value.persistenceError,
                )
            )
        is GatewayResult.Failure -> publishAccounts(accountState.copy(selecting = false, error = result.message))
    }
}

internal suspend fun MobileController.startAccountLogin() {
    syncAccountHost()
    if (accountState.login != null || accountState.startingLogin) return
    publishAccounts(accountState.copy(startingLogin = true, error = null))
    when (val result = requestAccount<HostAccountLogin>(AgentCommand.StartAccountLogin) ?: return) {
        is GatewayResult.Success -> {
            publishAccounts(accountState.copy(startingLogin = false, login = result.value))
            resumeAccountPolling()
        }
        is GatewayResult.Failure -> publishAccounts(accountState.copy(startingLogin = false, error = result.message))
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
            val result =
                requestAccount<HostAccountLoginStatus>(AgentCommand.AccountLoginStatus(login.loginId)) ?: return@launch
            if (accountHost != selected || accountGeneration != token || accountState.login != login) return@launch
            when (result) {
                is GatewayResult.Failure -> {
                    accountPolling = null
                    publishAccounts(accountState.copy(error = result.message))
                    return@launch
                }
                is GatewayResult.Success ->
                    if (result.value.completed) {
                        accountPolling = null
                        publishAccounts(accountState.copy(login = null))
                        refreshAccounts()
                        result.value.accountId?.let { selectAccount(it) }
                        return@launch
                    }
            }
        }
    }
}

internal suspend fun MobileController.cancelAccountLogin() {
    syncAccountHost()
    val login = accountState.login ?: return
    pauseAccountPolling()
    when (val result = requestAccount<JsonElement>(AgentCommand.CancelAccountLogin(login.loginId)) ?: return) {
        is GatewayResult.Success -> {
            publishAccounts(accountState.copy(login = null, error = null))
            refreshAccounts()
        }
        is GatewayResult.Failure -> publishAccounts(accountState.copy(error = result.message))
    }
}

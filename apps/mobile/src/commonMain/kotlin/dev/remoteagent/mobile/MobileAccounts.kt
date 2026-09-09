package dev.remoteagent.mobile

import kotlinx.atomicfu.AtomicRef
import kotlinx.atomicfu.getAndUpdate
import kotlinx.atomicfu.update
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement

private val accountJson = Json { ignoreUnknownKeys = true }
private const val ACCOUNT_POLL_INTERVAL_MS = 2000L

/** Mobile lifecycle boundary; a connection change retires its native projection and polling job. */
internal fun AtomicRef<MobileApp>.syncSettingsHost() {
    val selected = state.selectedProfile?.id
    val context =
        SettingsContext(
            selected,
            selected?.let(value.effects.sessions::currentGeneration),
            state.selectedView.connection == ConnectionPhase.Connected,
        )
    if (value.settingsContext == context) return
    val previous = getAndUpdate {
        it.copy(
            settingsContext = context,
            accountPolling = null,
            settingsState =
                AgentSettingsState(
                    options = it.state.turnChoices[selected] ?: CodexTurnOptions(),
                    modelRevision = it.settingsState.modelRevision + 1,
                ),
        )
    }
    previous.accountPolling?.cancel()
    value.settingsObservers.forEach { it(settingsState) }
    if (context.connected && value.settingsObservers.isNotEmpty()) value.effects.scope.launch { loadModels() }
}

internal fun AtomicRef<MobileApp>.observeSettings(observer: (AgentSettingsState) -> Unit): HostEventSubscription {
    syncSettingsHost()
    update { it.copy(settingsObservers = it.settingsObservers + observer) }
    observer(settingsState)
    if (value.settingsContext?.connected == true && settingsState.models.isEmpty())
        value.effects.scope.launch { loadModels() }
    return HostEventSubscription { update { it.copy(settingsObservers = it.settingsObservers - observer) } }
}

internal fun AtomicRef<MobileApp>.publishSettings(settings: AgentSettingsState) {
    val changed = settingsState.selectedId != settings.selectedId
    val updated =
        if (changed)
            settings.copy(models = emptyList(), loadingModels = false, modelRevision = settings.modelRevision + 1)
        else settings
    update { it.copy(settingsState = updated) }
    value.settingsObservers.forEach { it(settingsState) }
    if (changed && value.settingsObservers.isNotEmpty()) value.effects.scope.launch { loadModels() }
}

private suspend inline fun <reified T> AtomicRef<MobileApp>.requestAccount(command: AgentCommand): GatewayResult<T>? {
    val profile = state.selectedProfile ?: return GatewayResult.Failure("接続先が選択されていません")
    val context = value.settingsContext
    val result = requestAgent(profile.id, command).mapGateway { accountJson.decodeFromString<T>(it) }
    return result.takeIf { value.settingsContext == context }
}

internal suspend fun AtomicRef<MobileApp>.refreshAccounts() {
    syncSettingsHost()
    when (val result = requestAccount<HostAccountList>(AgentCommand.Accounts) ?: return) {
        is GatewayResult.Success ->
            publishSettings(
                settingsState.copy(
                    accounts = result.value.accounts,
                    selectedId = result.value.selectedId,
                    error = result.value.error,
                )
            )
        is GatewayResult.Failure -> publishSettings(settingsState.copy(error = result.message))
    }
    resumeAccountPolling()
}

internal suspend fun AtomicRef<MobileApp>.selectAccount(id: String) {
    syncSettingsHost()
    if (settingsState.selecting || settingsState.selectedId == id) return
    publishSettings(settingsState.copy(selecting = true, error = null))
    when (val result = requestAccount<HostAccountSelection>(AgentCommand.SelectAccount(id)) ?: return) {
        is GatewayResult.Success ->
            publishSettings(
                settingsState.copy(
                    selecting = false,
                    selectedId = result.value.selectedId,
                    error = result.value.persistenceError,
                )
            )
        is GatewayResult.Failure -> publishSettings(settingsState.copy(selecting = false, error = result.message))
    }
}

internal suspend fun AtomicRef<MobileApp>.startAccountLogin() {
    syncSettingsHost()
    if (settingsState.login != null || settingsState.startingLogin) return
    publishSettings(settingsState.copy(startingLogin = true, error = null))
    when (val result = requestAccount<HostAccountLogin>(AgentCommand.StartAccountLogin) ?: return) {
        is GatewayResult.Success -> {
            publishSettings(settingsState.copy(startingLogin = false, login = result.value))
            resumeAccountPolling()
        }
        is GatewayResult.Failure -> publishSettings(settingsState.copy(startingLogin = false, error = result.message))
    }
}

internal fun AtomicRef<MobileApp>.pauseAccountPolling() {
    getAndUpdate { it.copy(accountPolling = null) }.accountPolling?.cancel()
}

internal fun AtomicRef<MobileApp>.resumeAccountPolling() {
    syncSettingsHost()
    val login = settingsState.login ?: return
    if (value.accountPolling != null) return
    val context = value.settingsContext
    publishSettings(settingsState.copy(error = null))
    val job =
        value.effects.scope.launch(start = CoroutineStart.LAZY) {
            while (true) {
                delay(ACCOUNT_POLL_INTERVAL_MS)
                val result =
                    requestAccount<HostAccountLoginStatus>(AgentCommand.AccountLoginStatus(login.loginId))
                        ?: return@launch
                if (value.settingsContext != context || settingsState.login != login) return@launch
                when (result) {
                    is GatewayResult.Failure -> {
                        update { it.copy(accountPolling = null) }
                        publishSettings(settingsState.copy(error = result.message))
                        return@launch
                    }
                    is GatewayResult.Success ->
                        if (result.value.completed) {
                            update { it.copy(accountPolling = null) }
                            publishSettings(settingsState.copy(login = null))
                            refreshAccounts()
                            result.value.accountId?.let { selectAccount(it) }
                            return@launch
                        }
                }
            }
        }
    update { it.copy(accountPolling = job) }
    job.start()
}

internal suspend fun AtomicRef<MobileApp>.cancelAccountLogin() {
    syncSettingsHost()
    val login = settingsState.login ?: return
    pauseAccountPolling()
    when (val result = requestAccount<JsonElement>(AgentCommand.CancelAccountLogin(login.loginId)) ?: return) {
        is GatewayResult.Success -> {
            publishSettings(settingsState.copy(login = null, error = null))
            refreshAccounts()
        }
        is GatewayResult.Failure -> publishSettings(settingsState.copy(error = result.message))
    }
}

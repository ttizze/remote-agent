package dev.remoteagent.mobile

import kotlinx.atomicfu.AtomicRef
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch

/** Swift callback adapter; Rust owns decisions; common Kotlin coordinates the mobile lifecycle. */
class IosSettingsActions
internal constructor(private val controller: AtomicRef<MobileApp>, private val scope: CoroutineScope) {
    fun currentState(): AgentSettingsState = controller.settingsState

    fun observeSettings(observer: (AgentSettingsState) -> Unit): HostEventSubscription =
        controller.observeSettings(observer)

    fun loadModels() {
        scope.launch { controller.loadModels() }
    }

    fun chooseModel(value: String) {
        controller.chooseModel(value)
    }

    fun chooseEffort(value: String) {
        controller.chooseEffort(value)
    }

    fun refreshAccounts() {
        scope.launch { controller.refreshAccounts() }
    }

    fun selectAccount(id: String) {
        scope.launch { controller.selectAccount(id) }
    }

    fun startAccountLogin() {
        scope.launch { controller.startAccountLogin() }
    }

    fun cancelAccountLogin() {
        scope.launch { controller.cancelAccountLogin() }
    }

    fun setAccountLoginPolling(enabled: Boolean) {
        if (enabled) controller.resumeAccountPolling() else controller.pauseAccountPolling()
    }
}

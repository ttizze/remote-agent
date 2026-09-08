package dev.remoteagent.mobile

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch

/** Swift callback adapter; Rust owns decisions and MobileController owns lifecycle. */
class IosAccountActions
internal constructor(private val controller: MobileController, private val scope: CoroutineScope) {
    fun observeAccounts(observer: (AccountSettingsState) -> Unit): HostEventSubscription =
        controller.observeAccounts(observer)

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

    fun resumeAccountLogin() {
        controller.resumeAccountPolling()
    }

    fun pauseAccountLogin() {
        controller.pauseAccountPolling()
    }
}

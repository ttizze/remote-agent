package dev.remoteagent.mobile

import kotlinx.atomicfu.AtomicRef
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch

class IosHostActions
internal constructor(private val controller: AtomicRef<MobileApp>, private val scope: CoroutineScope) {
    fun openPairing() = controller.dispatch(AppAction.PairingOpened)

    fun dismissPairing() = controller.dispatch(AppAction.PairingDismissed)

    fun showProfiles() = controller.dispatch(AppAction.ProfileSelectionOpened)

    fun selectProfile(hostIdentity: String) = controller.dispatch(AppAction.ProfileSelected(hostIdentity))

    fun pair(contents: String, nowMs: Long) {
        scope.launch { controller.pair(contents, nowMs) }
    }

    fun connect() = controller.restoreConnection(scope)
}

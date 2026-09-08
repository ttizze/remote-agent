package dev.remoteagent.mobile

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/** Keep restoring the selected Host while the application scope is alive. */
internal fun MobileController.maintainConnection(scope: CoroutineScope): HostEventSubscription {
    val observation = observe {
        ensureSelectedConnection(scope)
        ensureVisibleThreadWatch(scope)
    }
    return HostEventSubscription {
        observation.cancel()
        foregroundRefreshJob?.cancel()
        foregroundRefreshJob = null
        reconnectJob?.cancel()
        reconnectJob = null
        threadWatchTarget = null
        threadWatchJob?.cancel()
        threadWatchJob = null
        threadWatchChanges?.close()
        threadWatchChanges = null
    }
}

internal fun MobileController.restoreConnection(scope: CoroutineScope) {
    when (state.selectedView.connection) {
        ConnectionPhase.Connected -> refreshAfterForeground(scope)
        ConnectionPhase.Connecting -> Unit
        else -> {
            reconnectJob?.cancel()
            reconnectJob = null
            ensureSelectedConnection(scope)
        }
    }
}

private fun MobileController.refreshAfterForeground(scope: CoroutineScope) {
    if (foregroundRefreshJob?.isActive == true || reconnectJob?.isActive == true) return
    val profile = state.selectedProfile
    val generation = profile?.let { sessions.currentGeneration(it.id) }
    if (profile != null && generation != null) {
        val job = scope.launch(start = CoroutineStart.LAZY) { refreshVisibleState(profile, generation) }
        foregroundRefreshJob = job
        job.start()
    }
}

internal fun MobileController.openApp(scope: CoroutineScope) {
    state.selectedProfileId?.let {
        dispatch(AppAction.ThreadListSearchChanged(it, ""))
        dispatch(AppAction.ThreadListOpened(it))
    }
    restoreConnection(scope)
}

internal fun MobileController.ensureSelectedConnection(scope: CoroutineScope) {
    val profile = state.selectedProfile
    if (reconnectHostId != profile?.id) {
        val previousJob = reconnectJob
        val previousRefresh = foregroundRefreshJob
        reconnectJob = null
        foregroundRefreshJob = null
        reconnectHostId = profile?.id
        previousRefresh?.cancel()
        previousJob?.cancel()
    }
    if (profile == null) return
    if (
        state.showingPairing ||
            state.selectedView.connection == ConnectionPhase.Connected ||
            reconnectJob?.isActive == true
    )
        return
    val job =
        scope.launch(start = CoroutineStart.LAZY) {
            var retryDelay = INITIAL_RECONNECT_DELAY_MS
            var current = state.selectedProfile
            while (current != null && current.id == profile.id) {
                connect(current, scope)
                if (state.selectedView.connection == ConnectionPhase.Connected) break
                delay(retryDelay)
                retryDelay = (retryDelay * 2).coerceAtMost(MAX_RECONNECT_DELAY_MS)
                current = state.selectedProfile
            }
        }
    reconnectJob = job
    job.start()
}

/** Snapshot one live generation before starting an asynchronous intent. */
internal fun MobileController.connectedGeneration(hostIdentity: String): Long? =
    sessions.currentGeneration(hostIdentity)?.takeIf { isConnected(hostIdentity, it) }

private const val INITIAL_RECONNECT_DELAY_MS = 1_000L
private const val MAX_RECONNECT_DELAY_MS = 30_000L

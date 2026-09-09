package dev.remoteagent.mobile

import kotlinx.atomicfu.AtomicRef
import kotlinx.atomicfu.getAndUpdate
import kotlinx.atomicfu.update
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/** Keep restoring the selected Host while the application scope is alive. */
internal fun AtomicRef<MobileApp>.maintainConnection(scope: CoroutineScope): HostEventSubscription {
    val observation = observe {
        ensureSelectedConnection(scope)
        ensureVisibleThreadWatch(scope)
    }
    return HostEventSubscription {
        observation.cancel()
        val previous = getAndUpdate { it.copy(foregroundRefreshJob = null, reconnectJob = null, watch = null) }
        previous.foregroundRefreshJob?.cancel()
        previous.reconnectJob?.cancel()
        previous.watch?.job?.cancel()
        previous.watch?.changes?.close()
    }
}

internal fun AtomicRef<MobileApp>.restoreConnection(scope: CoroutineScope) {
    when (state.selectedView.connection) {
        ConnectionPhase.Connected -> refreshAfterForeground(scope)
        ConnectionPhase.Connecting -> Unit
        else -> {
            getAndUpdate { it.copy(reconnectJob = null) }.reconnectJob?.cancel()
            ensureSelectedConnection(scope)
        }
    }
}

private fun AtomicRef<MobileApp>.refreshAfterForeground(scope: CoroutineScope) {
    if (value.foregroundRefreshJob?.isActive == true || value.reconnectJob?.isActive == true) return
    val profile = state.selectedProfile
    val generation = profile?.let { value.effects.sessions.currentGeneration(it.id) }
    if (profile != null && generation != null) {
        val job = scope.launch(start = CoroutineStart.LAZY) { refreshVisibleState(profile, generation) }
        update { it.copy(foregroundRefreshJob = job) }
        job.start()
    }
}

internal fun AtomicRef<MobileApp>.openApp(scope: CoroutineScope) {
    state.selectedProfileId?.let {
        dispatch(AppAction.ThreadListSearchChanged(it, ""))
        dispatch(AppAction.ThreadListOpened(it))
    }
    restoreConnection(scope)
}

internal fun AtomicRef<MobileApp>.ensureSelectedConnection(scope: CoroutineScope) {
    val profile = state.selectedProfile
    if (value.reconnectHostId != profile?.id) {
        val previous = getAndUpdate {
            it.copy(reconnectJob = null, foregroundRefreshJob = null, reconnectHostId = profile?.id)
        }
        previous.foregroundRefreshJob?.cancel()
        previous.reconnectJob?.cancel()
    }
    if (profile == null) return
    if (
        state.showingPairing ||
            state.selectedView.connection == ConnectionPhase.Connected ||
            value.reconnectJob?.isActive == true
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
    update { it.copy(reconnectJob = job) }
    job.start()
}

/** Snapshot one live generation before starting an asynchronous intent. */
internal fun AtomicRef<MobileApp>.connectedGeneration(hostIdentity: String): Long? =
    value.effects.sessions.currentGeneration(hostIdentity)?.takeIf { isConnected(hostIdentity, it) }

internal fun AtomicRef<MobileApp>.isConnected(hostIdentity: String, generation: Long): Boolean =
    value.effects.sessions.isCurrent(hostIdentity, generation) &&
        state.profileViews[hostIdentity]?.connection == ConnectionPhase.Connected

internal inline fun AtomicRef<MobileApp>.ifCurrent(hostIdentity: String, generation: Long, block: () -> Unit) {
    if (value.effects.sessions.isCurrent(hostIdentity, generation)) block()
}

internal inline fun AtomicRef<MobileApp>.dispatchIfCurrent(
    hostIdentity: String,
    generation: Long,
    action: () -> AppAction,
) {
    ifCurrent(hostIdentity, generation) { dispatch(action()) }
}

private const val INITIAL_RECONNECT_DELAY_MS = 1_000L
private const val MAX_RECONNECT_DELAY_MS = 30_000L

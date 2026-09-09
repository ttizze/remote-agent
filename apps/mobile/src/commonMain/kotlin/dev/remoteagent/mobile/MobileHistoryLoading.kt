package dev.remoteagent.mobile

import kotlinx.atomicfu.AtomicRef
import kotlinx.coroutines.sync.withLock

internal suspend fun AtomicRef<MobileApp>.readThread(profile: HostProfile, threadId: String) {
    val generation = value.effects.sessions.currentGeneration(profile.id) ?: return
    readThread(profile, threadId, generation)
}

internal suspend fun AtomicRef<MobileApp>.readThread(
    profile: HostProfile,
    threadId: String,
    generation: Long,
    background: Boolean = false,
) {
    if (
        !isConnected(profile.id, generation) ||
            (background && state.profileViews[profile.id]?.selectedThreadId != threadId)
    )
        return
    if (!background) {
        dispatchIfCurrent(profile.id, generation) { AppAction.ThreadSelected(profile.id, threadId) }
        dispatchIfCurrent(profile.id, generation) { AppAction.ThreadReadLoading(profile.id, threadId) }
    }
    val token = value.effects.sessions.beginRead(profile.id, threadId, generation) ?: return
    try {
        val result =
            value.effects.gateway
                .command(profile, AgentCommand.ReadThread(threadId, value.effects.deferHistoryItemDetails))
                .mapGateway(::codexThreadFromResponse)
        // Event callbacks and the read completion share one mutex. Events
        // delivered while the request was in flight are therefore drained
        // after the replacement Snapshot and never race it.
        value.effects.eventMutex.withLock {
            val completion = value.effects.sessions.finishRead(token)
            if (
                completion != null &&
                    value.effects.sessions.isCurrent(profile.id, generation) &&
                    state.profileViews[profile.id]?.selectedThreadId == threadId
            ) {
                if (completion.overflowed) {
                    dispatch(AppAction.ThreadReadFailed(profile.id, "Thread更新が多すぎるため同期できません。もう一度読み込んでください。"))
                } else {
                    result.fold(
                        success = { snapshot ->
                            dispatch(
                                AppAction.SnapshotReceived(profile.id, snapshot, bufferedEvents = completion.events)
                            )
                        },
                        failure = { dispatch(AppAction.ThreadReadFailed(profile.id, it)) },
                    )
                }
            }
        }
    } finally {
        // Navigation can cancel a background read while a notification is
        // buffered. Do not leave that thread behind an abandoned barrier.
        value.effects.sessions.finishRead(token)
    }
}

internal suspend fun AtomicRef<MobileApp>.loadOlderHistory(profile: HostProfile, turnId: String? = null) {
    val generation = connectedGeneration(profile.id) ?: return
    val snapshot = selectedHistoryPage(profile.id, turnId) ?: return
    val threadId = snapshot.summary.id
    val cursor = snapshot.historyCursor(turnId)
    val navigation = value.historyNavigation
    dispatch(AppAction.HistoryLoading(profile.id, true))
    try {
        val result =
            value.effects.gateway
                .command(
                    profile,
                    AgentCommand.ReadOlder(threadId, cursor, turnId, value.effects.deferHistoryItemDetails),
                )
                .mapGateway(::codexThreadFromResponse)
        value.effects.eventMutex.withLock {
            if (!isConnected(profile.id, generation) || value.historyNavigation != navigation) return@withLock
            val current = state.cache.snapshot(profile.id, threadId) ?: return@withLock
            when (result) {
                is GatewayResult.Success -> {
                    try {
                        dispatch(
                            AppAction.HistoryReceived(
                                profile.id,
                                mergeOlderHistory(current, result.value, turnId, cursor),
                            )
                        )
                    } catch (error: IllegalStateException) {
                        dispatch(AppAction.HistoryLoading(profile.id, false, error.message))
                    }
                }
                is GatewayResult.Failure -> dispatch(AppAction.HistoryLoading(profile.id, false, result.message))
            }
        }
    } finally {
        if (
            isConnected(profile.id, generation) &&
                value.historyNavigation == navigation &&
                state.profileViews[profile.id]?.loadingHistory == true
        )
            dispatch(AppAction.HistoryLoading(profile.id, false))
    }
}

private fun AtomicRef<MobileApp>.selectedHistoryPage(hostIdentity: String, turnId: String?): ThreadSnapshot? {
    val view = state.profileViews[hostIdentity] ?: return null
    val snapshot = view.selectedThreadId?.let { state.cache.snapshot(hostIdentity, it) }
    return when {
        view.loadingHistory || snapshot == null -> null
        turnId == null -> snapshot.takeIf { it.olderTurnsCursor != null }
        else -> snapshot.takeIf { it.turns.firstOrNull { turn -> turn.id == turnId }?.hasOlderItems == true }
    }
}

private fun ThreadSnapshot.historyCursor(turnId: String?): String? =
    if (turnId == null) olderTurnsCursor else turns.firstOrNull { it.id == turnId }?.olderItemsCursor

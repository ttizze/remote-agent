package dev.remoteagent.mobile

import kotlinx.coroutines.sync.withLock

internal suspend fun MobileController.readThread(profile: HostProfile, threadId: String) {
    val generation = sessions.currentGeneration(profile.id) ?: return
    readThread(profile, threadId, generation)
}

internal suspend fun MobileController.readThread(
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
    val token = sessions.beginRead(profile.id, threadId, generation) ?: return
    try {
        val result = gateway.codex.readThread(profile, threadId)
        // Event callbacks and the read completion share one mutex. Events
        // delivered while the request was in flight are therefore drained
        // after the replacement Snapshot and never race it.
        eventMutex.withLock {
            val completion = sessions.finishRead(token)
            if (
                completion != null &&
                    sessions.isCurrent(profile.id, generation) &&
                    state.profileViews[profile.id]?.selectedThreadId == threadId
            ) {
                if (completion.overflowed) {
                    dispatch(AppAction.ThreadReadFailed(profile.id, "Thread更新が多すぎるため同期できません。もう一度読み込んでください。"))
                } else {
                    result.fold(
                        success = { snapshot ->
                            val buffered =
                                (snapshot.bufferedEvents + completion.events).filter {
                                    it.threadId == snapshot.thread.summary.id
                                }
                            dispatch(AppAction.SnapshotReceived(profile.id, snapshot.copy(bufferedEvents = buffered)))
                        },
                        failure = { dispatch(AppAction.ThreadReadFailed(profile.id, it)) },
                    )
                }
            }
        }
    } finally {
        // Navigation can cancel a background read while a notification is
        // buffered. Do not leave that thread behind an abandoned barrier.
        sessions.finishRead(token)
    }
}

internal suspend fun MobileController.loadOlderHistory(profile: HostProfile, turnId: String? = null) {
    val generation = connectedGeneration(profile.id) ?: return
    val snapshot = selectedHistoryPage(profile.id, turnId) ?: return
    val threadId = snapshot.summary.id
    val cursor = snapshot.historyCursor(turnId)
    val navigation = historyNavigation
    dispatch(AppAction.HistoryLoading(profile.id, true))
    try {
        val result = historyClient.readOlderHistory(profile, threadId, cursor, turnId)
        eventMutex.withLock {
            if (!isConnected(profile.id, generation) || historyNavigation != navigation) return@withLock
            val current = state.cache.snapshot(profile.id, threadId) ?: return@withLock
            if (!current.matchesHistoryPage(turnId, cursor)) return@withLock
            when (result) {
                is GatewayResult.Success ->
                    dispatch(AppAction.HistoryReceived(profile.id, mergeOlderHistory(current, result.value, turnId)))
                is GatewayResult.Failure -> dispatch(AppAction.HistoryLoading(profile.id, false, result.message))
            }
        }
    } finally {
        if (
            isConnected(profile.id, generation) &&
                historyNavigation == navigation &&
                state.profileViews[profile.id]?.loadingHistory == true
        )
            dispatch(AppAction.HistoryLoading(profile.id, false))
    }
}

private fun MobileController.selectedHistoryPage(hostIdentity: String, turnId: String?): ThreadSnapshot? {
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

private fun ThreadSnapshot.matchesHistoryPage(turnId: String?, cursor: String?): Boolean =
    historyCursor(turnId) == cursor && (turnId == null || turns.firstOrNull { it.id == turnId }?.hasOlderItems == true)

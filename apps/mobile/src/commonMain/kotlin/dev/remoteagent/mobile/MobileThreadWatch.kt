package dev.remoteagent.mobile

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

internal fun MobileController.ensureVisibleThreadWatch(scope: CoroutineScope) {
    val target = visibleThreadWatchTarget()
    if (target == threadWatchTarget) return
    threadWatchTarget = target
    threadWatchJob?.cancel()
    threadWatchChanges?.close()
    threadWatchJob = null
    threadWatchChanges = null
    if (target == null) return

    val revision = ++threadWatchRevision
    val changes = Channel<Unit>(Channel.CONFLATED)
    threadWatchChanges = changes
    val job = scope.launch(start = CoroutineStart.LAZY) { watchThread(target, changes, revision) }
    threadWatchJob = job
    job.start()
}

internal fun MobileController.receiveThreadWatchNotification(
    hostIdentity: String,
    message: RawCodexMessage.Notification,
) {
    val target = threadWatchTarget ?: return
    val params = message.params.asObjectOrNull() ?: return
    if (
        target.profile.id == hostIdentity &&
            params.long("watchId") == threadWatchRevision &&
            params.string("threadId") == target.threadId
    ) {
        if (message.method == "host/thread/watchFailed") {
            dispatch(AppAction.ThreadReadFailed(hostIdentity, "会話の自動更新が停止しました。再読み込みしてください。"))
        } else threadWatchChanges?.trySend(Unit)
    }
}

internal data class ThreadWatchTarget(
    val profile: HostProfile,
    val threadId: String,
    val path: String,
    val generation: Long,
)

private fun MobileController.visibleThreadWatchTarget(): ThreadWatchTarget? {
    val profile = state.selectedProfile ?: return null
    val view = state.selectedView
    val snapshot = view.selectedThreadId?.let { state.cache.snapshot(profile.id, it) }
    val path = snapshot?.raw?.string("path")?.takeIf(String::isNotBlank)
    val generation = connectedGeneration(profile.id)
    return if (snapshot == null || path == null || generation == null) null
    else if (
        snapshot.summary.status != ThreadStatus.NotLoaded ||
            (view.threadDetail !is LoadPhase.Ready && view.threadDetail !is LoadPhase.Failed)
    )
        null
    else ThreadWatchTarget(profile, snapshot.summary.id, path, generation)
}

private suspend fun MobileController.watchThread(target: ThreadWatchTarget, changes: Channel<Unit>, revision: Long) {
    try {
        val registered =
            gateway.rawRequest(
                target.profile,
                "host/thread/watch",
                buildJsonObject {
                    put("watchId", revision)
                    put("threadId", target.threadId)
                    put("path", target.path)
                },
            )
        if (registered is GatewayResult.Failure) {
            if (threadWatchTarget == target && threadWatchRevision == revision) {
                dispatch(AppAction.ThreadReadFailed(target.profile.id, "会話の自動更新を開始できません: ${registered.message}"))
            }
            return
        }
        // Close the gap between the initial history read and installing
        // the OS watch, without replacing the screen with a loader.
        changes.trySend(Unit)
        while (changes.receiveCatching().isSuccess) {
            delay(THREAD_WATCH_COALESCE_MS)
            var pending: Boolean
            do {
                pending = changes.tryReceive().isSuccess
            } while (pending)
            if (threadWatchTarget != target || threadWatchRevision != revision) return
            readThread(target.profile, target.threadId, target.generation, background = true)
        }
    } finally {
        withContext(NonCancellable) {
            if (isConnected(target.profile.id, target.generation)) {
                gateway.rawRequest(target.profile, "host/thread/unwatch", buildJsonObject { put("watchId", revision) })
            }
        }
    }
}

private const val THREAD_WATCH_COALESCE_MS = 100L

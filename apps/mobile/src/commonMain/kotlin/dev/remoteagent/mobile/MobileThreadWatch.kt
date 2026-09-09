package dev.remoteagent.mobile

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.jsonPrimitive
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
    val params = message.params.asObjectOrNull()?.takeIf { it.long("watchKey") == VISIBLE_THREAD_WATCH_KEY } ?: return
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
    val path = snapshot?.let { thread ->
        Json.parseToJsonElement(
                nativeConversationPresentation(
                    buildJsonObject {
                            put("operation", "watchPath")
                            put(
                                "thread",
                                buildJsonObject {
                                    put("path", thread.raw?.string("path"))
                                    put(
                                        "status",
                                        Json.encodeToJsonElement(ThreadStatus.serializer(), thread.summary.status),
                                    )
                                },
                            )
                        }
                        .toString()
                )
            )
            .jsonPrimitive
            .contentOrNull
    }
    val generation = connectedGeneration(profile.id)
    return if (snapshot == null || path == null || generation == null) null
    else if (view.threadDetail !is LoadPhase.Ready && view.threadDetail !is LoadPhase.Failed) null
    else ThreadWatchTarget(profile, snapshot.summary.id, path, generation)
}

private suspend fun MobileController.watchThread(target: ThreadWatchTarget, changes: Channel<Unit>, revision: Long) {
    try {
        val registered =
            gateway.agentCommand(
                target.profile,
                AgentCommand.WatchThread(target.threadId, VISIBLE_THREAD_WATCH_KEY, revision, target.path),
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
                gateway.agentCommand(target.profile, AgentCommand.UnwatchThread(VISIBLE_THREAD_WATCH_KEY, revision))
            }
        }
    }
}

private const val VISIBLE_THREAD_WATCH_KEY = 1L
private const val THREAD_WATCH_COALESCE_MS = 100L

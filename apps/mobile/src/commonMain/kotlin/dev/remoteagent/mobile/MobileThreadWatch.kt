package dev.remoteagent.mobile

import kotlinx.atomicfu.AtomicRef
import kotlinx.atomicfu.getAndUpdate
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Job
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

internal data class MobileThreadWatch(
    val target: ThreadWatchTarget,
    val revision: Long,
    val changes: Channel<Unit>,
    val job: Job,
)

internal fun AtomicRef<MobileApp>.ensureVisibleThreadWatch(scope: CoroutineScope) {
    val target = visibleThreadWatchTarget()
    if (target == value.watch?.target) return
    val revision = value.nextWatchRevision + if (target == null) 0 else 1
    val next = target?.let {
        val changes = Channel<Unit>(Channel.CONFLATED)
        MobileThreadWatch(
            it,
            revision,
            changes,
            scope.launch(start = CoroutineStart.LAZY) { watchThread(it, changes, revision) },
        )
    }
    val previous = getAndUpdate { it.copy(watch = next, nextWatchRevision = revision) }.watch
    previous?.job?.cancel()
    previous?.changes?.close()
    next?.job?.start()
}

internal fun AtomicRef<MobileApp>.receiveThreadWatchNotification(
    hostIdentity: String,
    message: RawCodexMessage.Notification,
) {
    val watch = value.watch ?: return
    val target = watch.target
    val params = message.params.asObjectOrNull()?.takeIf { it.long("watchKey") == VISIBLE_THREAD_WATCH_KEY } ?: return
    if (
        target.profile.id == hostIdentity &&
            params.long("watchId") == watch.revision &&
            params.string("threadId") == target.threadId
    ) {
        if (message.method == "host/thread/watchFailed") {
            dispatch(AppAction.ThreadReadFailed(hostIdentity, "会話の自動更新が停止しました。再読み込みしてください。"))
        } else watch.changes.trySend(Unit)
    }
}

internal data class ThreadWatchTarget(
    val profile: HostProfile,
    val threadId: String,
    val path: String,
    val generation: Long,
)

private fun AtomicRef<MobileApp>.visibleThreadWatchTarget(): ThreadWatchTarget? {
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

private suspend fun AtomicRef<MobileApp>.watchThread(
    target: ThreadWatchTarget,
    changes: Channel<Unit>,
    revision: Long,
) {
    try {
        val registered =
            value.effects.gateway.agentCommand(
                target.profile,
                AgentCommand.WatchThread(target.threadId, VISIBLE_THREAD_WATCH_KEY, revision, target.path),
            )
        if (registered is GatewayResult.Failure) {
            if (value.watch?.target == target && value.watch?.revision == revision) {
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
            if (value.watch?.target != target || value.watch?.revision != revision) return
            readThread(target.profile, target.threadId, target.generation, background = true)
        }
    } finally {
        withContext(NonCancellable) {
            if (isConnected(target.profile.id, target.generation)) {
                value.effects.gateway.agentCommand(
                    target.profile,
                    AgentCommand.UnwatchThread(VISIBLE_THREAD_WATCH_KEY, revision),
                )
            }
        }
    }
}

private const val VISIBLE_THREAD_WATCH_KEY = 1L
private const val THREAD_WATCH_COALESCE_MS = 100L

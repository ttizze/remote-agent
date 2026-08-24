package dev.remoteagent.mobile

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.atomicfu.locks.SynchronizedObject
import kotlinx.atomicfu.locks.synchronized

/**
 * Shared application module used by the Android Compose and iOS SwiftUI
 * presentation adapters. Platform UI code observes state and invokes these
 * intent-level operations; transport and reconciliation remain hidden here.
 */
internal class MobileController(
    private val gateway: HostGateway,
    private val repository: MobileRepository,
    private val cacheLimits: MobileCacheLimits = MobileCacheLimits(),
) {
    var state = repository.load().restoreDisconnected()
        private set

    private val eventMutex = Mutex()
    private val coordinationLock = SynchronizedObject()
    private val eventSubscriptions = mutableMapOf<String, HostEventSubscription>()
    private val subscriptionGenerations = mutableMapOf<String, Long>()
    private val readBuffers = mutableMapOf<String, ReadBuffer>()
    private var nextReadToken = 0L
    private val observers = mutableSetOf<(AppState) -> Unit>()

    fun observe(observer: (AppState) -> Unit): HostEventSubscription {
        observers += observer
        observer(state)
        return HostEventSubscription { observers -= observer }
    }

    fun dispatch(action: AppAction) {
        state = reduce(state, action, cacheLimits)
        repository.save(state)
        observers.toList().forEach { it(state) }
    }

    suspend fun pair(contents: String, nowMs: Long) {
        when (val parsed = parsePairingQr(contents, nowMs)) {
            is PairingQrResult.Invalid -> dispatch(AppAction.PairingFailed(parsed.reason.name))
            is PairingQrResult.Valid -> gateway.pair(parsed.payload).fold(
                success = { dispatch(AppAction.ProfilePaired(it)) },
                failure = { dispatch(AppAction.PairingFailed(it)) },
            )
        }
    }

    suspend fun discover(profile: HostProfile) {
        gateway.discover(profile).fold(
            success = { dispatch(AppAction.AddressesDiscovered(profile.hostIdentity, it)) },
            failure = { dispatch(AppAction.ConnectFailed(profile.hostIdentity, it)) },
        )
    }

    /** Connect, subscribe, and reconcile the visible Host state before returning. */
    suspend fun connect(profile: HostProfile, scope: CoroutineScope) {
        dispatch(AppAction.ConnectStarted(profile.hostIdentity))
        // Retire the old stream before opening a new one. A generation check
        // below also rejects callbacks which were already in flight.
        val generation = replaceSubscription(profile.hostIdentity, null)
        when (val result = gateway.connect(profile)) {
            is GatewayResult.Failure -> dispatch(AppAction.ConnectFailed(profile.hostIdentity, result.message))
            is GatewayResult.Success -> {
                dispatch(AppAction.ConnectSucceeded(profile.hostIdentity))
                val subscription = runCatching {
                    gateway.subscribeRaw(profile) { message ->
                        val event = (message as? RawCodexMessage.Notification)?.let {
                            codexThreadEvent(it.method, it.params, it.extensions)
                        }
                        // Capture the read barrier synchronously at callback
                        // time. The transport may deliver the event on a
                        // native thread immediately before the read returns.
                        val current = isCurrentSubscription(profile.hostIdentity, generation)
                        val buffered = if (current) {
                            event?.let { bufferReadEvent(profile.hostIdentity, it) } == true
                        } else {
                            false
                        }
                        scope.launch(start = CoroutineStart.UNDISPATCHED) {
                            eventMutex.withLock {
                                if (current && isCurrentSubscription(profile.hostIdentity, generation)) {
                                    // Keep raw and typed projections in one
                                    // serialized transition. This preserves
                                    // wire arrival order for live output.
                                    dispatch(AppAction.RawMessageReceived(profile.hostIdentity, message))
                                    if (event != null && !buffered) {
                                        dispatch(AppAction.LiveEventReceived(profile.hostIdentity, event))
                                    }
                                }
                            }
                        }
                    }
                }.getOrElse {
                    dispatch(AppAction.Disconnected(profile.hostIdentity))
                    return
                }
                replaceSubscription(profile.hostIdentity, subscription, generation)

                // A reconnect must never leave the user looking at an old
                // list. Loading the list first also restores the list screen
                // for a profile that had no selected thread.
                val selectedThreadId = state.profileViews[profile.hostIdentity]?.selectedThreadId
                listThreads(profile)
                if (
                    selectedThreadId != null &&
                    state.profileViews[profile.hostIdentity]?.threadList == LoadPhase.Ready
                ) {
                    readThread(profile, selectedThreadId)
                }
            }
        }
    }

    suspend fun listThreads(profile: HostProfile) {
        if (!isConnected(profile.hostIdentity)) return
        val cwd = state.profileViews[profile.hostIdentity]?.workingDirectoryPath.orEmpty()
        dispatch(AppAction.ThreadListLoading(profile.hostIdentity))
        gateway.listThreads(profile, cwd).fold(
            success = { dispatch(AppAction.ThreadListLoaded(profile.hostIdentity, it)) },
            failure = { dispatch(AppAction.ThreadListFailed(profile.hostIdentity, it)) },
        )
    }

    suspend fun readThread(profile: HostProfile, threadId: String) {
        if (!isConnected(profile.hostIdentity)) return
        dispatch(AppAction.ThreadSelected(profile.hostIdentity, threadId))
        dispatch(AppAction.ThreadReadLoading(profile.hostIdentity, threadId))
        val token = beginRead(profile.hostIdentity, threadId)
        val result = gateway.readThread(profile, threadId)
        // Event callbacks and the read completion share one mutex. Events
        // delivered while the request was in flight are therefore drained
        // after the replacement Snapshot and never race it.
        eventMutex.withLock {
            val completion = finishRead(profile.hostIdentity, token)
            if (completion != null && state.profileViews[profile.hostIdentity]?.selectedThreadId == threadId) {
                if (completion.overflowed) {
                    dispatch(
                        AppAction.ThreadReadFailed(
                            profile.hostIdentity,
                            "Thread更新が多すぎるため同期できません。もう一度読み込んでください。",
                        ),
                    )
                } else {
                    result.fold(
                        success = { snapshot ->
                            val buffered = (snapshot.bufferedEvents + completion.events)
                                .filter { it.threadId == snapshot.thread.summary.id }
                            dispatch(
                                AppAction.SnapshotReceived(
                                    profile.hostIdentity,
                                    snapshot.copy(bufferedEvents = buffered),
                                ),
                            )
                        },
                        failure = { dispatch(AppAction.ThreadReadFailed(profile.hostIdentity, it)) },
                    )
                }
            }
        }
    }

    suspend fun startThread(profile: HostProfile) {
        if (!isConnected(profile.hostIdentity)) return
        val cwd = state.profileViews[profile.hostIdentity]?.workingDirectoryPath.orEmpty()
        if (cwd.isBlank()) {
            dispatch(AppAction.ThreadStartFailed(profile.hostIdentity, "作業ディレクトリを指定してください。"))
            return
        }
        gateway.startThread(profile, cwd).fold(
            success = { dispatch(AppAction.SnapshotReceived(profile.hostIdentity, ThreadReadResult(it, emptyList()))) },
            failure = { dispatch(AppAction.ThreadStartFailed(profile.hostIdentity, it)) },
        )
    }

    suspend fun startTurn(profile: HostProfile, threadId: String, text: String) {
        if (!isConnected(profile.hostIdentity)) return
        val cwd = cachedWorkingDirectory(profile.hostIdentity, threadId)
        if (cwd == null) {
            dispatch(AppAction.TurnFailed(profile.hostIdentity, MissingThreadWorkingDirectoryMessage))
            return
        }
        gateway.startTurn(profile, threadId, cwd, text).fold(
            success = { dispatch(AppAction.TurnStartAcknowledged(profile.hostIdentity, threadId, it)) },
            failure = { dispatch(AppAction.TurnFailed(profile.hostIdentity, it)) },
        )
    }

    suspend fun interrupt(profile: HostProfile, threadId: String, turnId: String) {
        if (!isConnected(profile.hostIdentity)) return
        dispatch(AppAction.InterruptStarted(profile.hostIdentity, turnId))
        gateway.interrupt(profile, threadId, turnId).fold(
            success = { dispatch(AppAction.InterruptFinished(profile.hostIdentity)) },
            failure = {
                dispatch(AppAction.InterruptFinished(profile.hostIdentity))
                dispatch(AppAction.TurnFailed(profile.hostIdentity, it))
            },
        )
    }

    private fun isConnected(hostIdentity: String): Boolean =
        state.profileViews[hostIdentity]?.connection == ConnectionPhase.Connected

    private fun cachedWorkingDirectory(hostIdentity: String, threadId: String): String? {
        val cache = state.cache.profile(hostIdentity)
        val snapshotCwd = cache.snapshots[threadId]?.summary?.workingDirectory?.path
        if (!snapshotCwd.isNullOrBlank()) return snapshotCwd
        return cache.threadList.firstOrNull { it.id == threadId }
            ?.workingDirectory
            ?.path
            ?.takeIf(String::isNotBlank)
    }

    private fun replaceSubscription(hostIdentity: String, subscription: HostEventSubscription?): Long =
        synchronized(coordinationLock) {
            eventSubscriptions.remove(hostIdentity)?.cancel()
            val generation = (subscriptionGenerations[hostIdentity] ?: 0L) + 1L
            subscriptionGenerations[hostIdentity] = generation
            if (subscription != null) eventSubscriptions[hostIdentity] = subscription
            generation
        }

    private fun replaceSubscription(
        hostIdentity: String,
        subscription: HostEventSubscription,
        generation: Long,
    ) {
        synchronized(coordinationLock) {
            if (subscriptionGenerations[hostIdentity] == generation) {
                eventSubscriptions[hostIdentity] = subscription
            } else {
                subscription.cancel()
            }
        }
    }

    private fun isCurrentSubscription(hostIdentity: String, generation: Long): Boolean =
        synchronized(coordinationLock) { subscriptionGenerations[hostIdentity] == generation } &&
            isConnected(hostIdentity)

    private fun beginRead(hostIdentity: String, threadId: String): Long = synchronized(coordinationLock) {
        nextReadToken += 1L
        readBuffers[hostIdentity] = ReadBuffer(nextReadToken, threadId)
        nextReadToken
    }

    /** Returns true when the event belongs to an active read and was held/dropped. */
    private fun bufferReadEvent(hostIdentity: String, event: ThreadEvent): Boolean = synchronized(coordinationLock) {
        val read = readBuffers[hostIdentity] ?: return@synchronized false
        if (read.threadId != event.threadId) return@synchronized false
        if (read.overflowed) return@synchronized true
        val eventBytes = event.approximateBytes()
        if (
            read.events.size >= MaxReadBufferedEvents ||
            read.approximateBytes + eventBytes > cacheLimits.maxApproximateBytes.coerceAtMost(MaxReadBufferedBytes)
        ) {
            read.overflowed = true
            return@synchronized true
        }
        read.events += event
        read.approximateBytes += eventBytes
        true
    }

    private fun finishRead(hostIdentity: String, token: Long): ReadCompletion? = synchronized(coordinationLock) {
        val read = readBuffers[hostIdentity]
        if (read == null || read.token != token) return@synchronized null
        readBuffers.remove(hostIdentity)
        ReadCompletion(read.events.toList(), read.overflowed)
    }

    private data class ReadBuffer(
        val token: Long,
        val threadId: String,
        val events: MutableList<ThreadEvent> = mutableListOf(),
        var approximateBytes: Int = 0,
        var overflowed: Boolean = false,
    )

    private data class ReadCompletion(
        val events: List<ThreadEvent>,
        val overflowed: Boolean,
    )

    private companion object {
        const val MissingThreadWorkingDirectoryMessage = "タスクの作業ディレクトリが不明です。タスク一覧を更新してください"
        const val MaxReadBufferedEvents = 256
        const val MaxReadBufferedBytes = 256 * 1024
    }
}

private suspend fun <T> GatewayResult<T>.fold(
    success: suspend (T) -> Unit,
    failure: suspend (String) -> Unit,
) = when (this) {
    is GatewayResult.Success -> success(value)
    is GatewayResult.Failure -> failure(message)
}

private fun ThreadEvent.approximateBytes(): Int = toString().length * 2 + 8

private fun AppState.restoreDisconnected(): AppState = copy(
    profileViews = profileViews.mapValues { (_, view) ->
        view.copy(
            connection = ConnectionPhase.Disconnected,
            threadList = LoadPhase.Idle,
            threadDetail = LoadPhase.Idle,
            interruptingTurnId = null,
        )
    },
)

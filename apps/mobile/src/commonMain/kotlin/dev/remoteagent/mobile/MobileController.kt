package dev.remoteagent.mobile

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext

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
    /** Sanitized diagnostic only; persistence failures never become UI notices. */
    var lastPersistenceFailureType: String? = null
        private set

    private val eventMutex = Mutex()
    private val sessions = HostSessionCoordinator(cacheLimits)
    private val observers = mutableSetOf<(AppState) -> Unit>()

    fun observe(observer: (AppState) -> Unit): HostEventSubscription {
        observers += observer
        observer(state)
        return HostEventSubscription { observers -= observer }
    }

    fun dispatch(action: AppAction) {
        if (action is AppAction.Disconnected) sessions.retireHost(action.hostIdentity)
        publish(action)
    }

    private fun publish(action: AppAction) {
        state = reduce(state, action, cacheLimits)
        lastPersistenceFailureType = try {
            repository.save(state)
            null
        } catch (failure: Exception) {
            failure::class.simpleName ?: "PersistenceFailure"
        }
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

    /** Retires the active transport generation before reducing Disconnected. */
    internal fun disconnect(hostIdentity: String) {
        sessions.currentGeneration(hostIdentity)?.let { generation ->
            disconnect(hostIdentity, generation)
        }
    }

    suspend fun disconnect(profile: HostProfile) {
        withContext(NonCancellable) {
            val hostIdentity = profile.hostIdentity
            sessions.currentGeneration(hostIdentity) ?: return@withContext
            // Invalidate callbacks immediately, then serialize the native
            // close behind any connection attempt already in flight.
            sessions.retireHost(hostIdentity)
            sessions.withHostConnection(hostIdentity) {
                // A reconnect requested after this disconnect is the newer
                // user intent and owns the newly installed handle.
                if (sessions.currentGeneration(hostIdentity) != null) return@withHostConnection
                gateway.disconnect(profile)
                eventMutex.withLock { publish(AppAction.Disconnected(hostIdentity)) }
            }
        }
    }

    /** Connect, subscribe, and reconcile the visible Host state before returning. */
    suspend fun connect(profile: HostProfile, scope: CoroutineScope) {
        sessions.withHostConnection(profile.hostIdentity) {
            // Start the generation before touching the transport. A previous
            // subscription is retired by the coordinator before connect can
            // deliver callbacks for the new generation.
            val generation = eventMutex.withLock {
                sessions.beginConnection(profile.hostIdentity).also {
                    publish(AppAction.ConnectStarted(profile.hostIdentity))
                }
            }
            try {
                val connectionProfile = when (val discovered = gateway.discover(profile)) {
                    is GatewayResult.Success -> discovered.value
                        .map(String::trim)
                        .filter(String::isNotEmpty)
                        .distinct()
                        .takeIf(List<String>::isNotEmpty)
                        ?.let { addresses ->
                            dispatch(AppAction.AddressesDiscovered(profile.hostIdentity, addresses))
                            profile.copy(addresses = addresses)
                        }
                        ?: profile
                    is GatewayResult.Failure -> profile
                }
                if (!sessions.isCurrent(profile.hostIdentity, generation)) return@withHostConnection

                when (val result = gateway.connect(connectionProfile)) {
                    is GatewayResult.Failure -> ifCurrent(profile.hostIdentity, generation) {
                        sessions.retireHost(profile.hostIdentity)
                        gateway.disconnect(connectionProfile)
                        dispatch(AppAction.ConnectFailed(profile.hostIdentity, result.message))
                    }

                    is GatewayResult.Success -> {
                        if (!sessions.isCurrent(profile.hostIdentity, generation)) {
                            gateway.disconnect(connectionProfile)
                            return@withHostConnection
                        }
                        dispatch(AppAction.ConnectSucceeded(profile.hostIdentity))
                        val subscription = try {
                            gateway.subscribeRaw(
                                profile = connectionProfile,
                                onMessage = { message ->
                                    val notification = message as? RawCodexMessage.Notification
                                    val refreshProjects = notification?.method == "project/changed"
                                    val refreshThreads = notification?.method in ThreadListInvalidatingMethods
                                    val event = message
                                        .takeUnless { refreshProjects || refreshThreads }
                                        ?.let(::codexThreadEvent)
                                    // Capture the generation and read barrier before
                                    // yielding. Native transports may invoke this
                                    // callback synchronously while a read is in flight.
                                    val current = sessions.isCurrent(profile.hostIdentity, generation)
                                    val buffered = if (current) {
                                        event?.let {
                                            sessions.bufferEvent(profile.hostIdentity, it).isHeld
                                        } == true
                                    } else {
                                        false
                                    }
                                    // Platform controllers provide their serialized
                                    // application/UI scope. Never reduce state
                                    // inline on a native poller thread.
                                    scope.launch {
                                        eventMutex.withLock {
                                            if (sessions.isCurrent(profile.hostIdentity, generation)) {
                                                // Keep raw and typed projections in one
                                                // serialized transition. This preserves
                                                // wire arrival order for live output.
                                                dispatch(AppAction.RawMessageReceived(profile.hostIdentity, message))
                                                if (event != null && !buffered) {
                                                    dispatch(AppAction.LiveEventReceived(profile.hostIdentity, event))
                                                }
                                            }
                                        }
                                        if (sessions.isCurrent(profile.hostIdentity, generation)) {
                                            if (refreshProjects) listProjects(connectionProfile, generation)
                                            if (refreshThreads) listThreads(connectionProfile, generation)
                                        }
                                    }
                                },
                                onClosed = {
                                    scope.launch { disconnect(connectionProfile, generation) }
                                },
                            )
                        } catch (cancelled: CancellationException) {
                            throw cancelled
                        } catch (_: Throwable) {
                            disconnect(connectionProfile, generation)
                            return@withHostConnection
                        }
                        if (!sessions.installSubscription(profile.hostIdentity, generation, subscription)) {
                            gateway.disconnect(connectionProfile)
                            return@withHostConnection
                        }

                        // A reconnect always opens the task list. ConnectSucceeded
                        // clears any durable detail selection before these fresh
                        // list reads; a thread is read only after an explicit tap.
                        listProjects(connectionProfile, generation)
                        listThreads(connectionProfile, generation)
                    }
                }
            } catch (cancelled: CancellationException) {
                // A blocking platform connect may install its handle just as
                // the caller is cancelled. Close it before the per-Host
                // connection mutex admits a reconnect.
                withContext(NonCancellable) {
                    sessions.retireHost(profile.hostIdentity)
                    gateway.disconnect(profile)
                    eventMutex.withLock { publish(AppAction.Disconnected(profile.hostIdentity)) }
                }
                throw cancelled
            }
        }
    }

    suspend fun listThreads(profile: HostProfile) {
        val generation = sessions.currentGeneration(profile.hostIdentity) ?: return
        listThreads(profile, generation)
    }

    suspend fun listProjects(profile: HostProfile) {
        val generation = sessions.currentGeneration(profile.hostIdentity) ?: return
        listProjects(profile, generation)
    }

    suspend fun readThread(profile: HostProfile, threadId: String) {
        val generation = sessions.currentGeneration(profile.hostIdentity) ?: return
        readThread(profile, threadId, generation)
    }

    suspend fun startThread(profile: HostProfile) {
        val generation = sessions.currentGeneration(profile.hostIdentity) ?: return
        if (!isConnected(profile.hostIdentity, generation)) return
        val cwd = state.profileViews[profile.hostIdentity]?.workingDirectoryPath.orEmpty()
        if (cwd.isBlank()) {
            dispatchIfCurrent(profile.hostIdentity, generation) {
                AppAction.ThreadStartFailed(profile.hostIdentity, "作業ディレクトリを指定してください。")
            }
            return
        }
        gateway.startThread(profile, cwd).fold(
            success = { snapshot ->
                dispatchIfCurrent(profile.hostIdentity, generation) {
                    AppAction.SnapshotReceived(profile.hostIdentity, ThreadReadResult(snapshot, emptyList()))
                }
            },
            failure = { message ->
                dispatchIfCurrent(profile.hostIdentity, generation) {
                    AppAction.ThreadStartFailed(profile.hostIdentity, message)
                }
            },
        )
    }

    suspend fun startThread(
        profile: HostProfile,
        cwd: String,
        firstPrompt: String,
    ) {
        val generation = sessions.currentGeneration(profile.hostIdentity) ?: return
        if (!isConnected(profile.hostIdentity, generation)) return
        val normalizedCwd = cwd.trim()
        val normalizedPrompt = firstPrompt.trim()
        if (normalizedCwd.isBlank()) {
            dispatchIfCurrent(profile.hostIdentity, generation) {
                AppAction.ThreadStartFailed(profile.hostIdentity, "作業ディレクトリを指定してください。")
            }
            return
        }
        if (normalizedPrompt.isBlank()) {
            dispatchIfCurrent(profile.hostIdentity, generation) {
                AppAction.ThreadStartFailed(profile.hostIdentity, "最初のメッセージを入力してください。")
            }
            return
        }
        gateway.startThread(profile, normalizedCwd, normalizedPrompt).fold(
            success = { result ->
                dispatchIfCurrent(profile.hostIdentity, generation) {
                    AppAction.SnapshotReceived(
                        profile.hostIdentity,
                        ThreadReadResult(result.thread, emptyList()),
                    )
                }
                dispatchIfCurrent(profile.hostIdentity, generation) {
                    AppAction.TurnStartAcknowledged(profile.hostIdentity, result.thread.summary.id, result.turnId)
                }
            },
            failure = { message ->
                dispatchIfCurrent(profile.hostIdentity, generation) {
                    AppAction.ThreadStartFailed(profile.hostIdentity, message)
                }
            },
        )
    }

    suspend fun startTurn(profile: HostProfile, threadId: String, text: String) {
        val generation = sessions.currentGeneration(profile.hostIdentity) ?: return
        if (!isConnected(profile.hostIdentity, generation)) return

        val cachedSnapshot = state.cache
            .snapshot(profile.hostIdentity, threadId)
        val activeTurnId = cachedSnapshot
            ?.turns
            ?.lastOrNull { it.status == TurnStatus.InProgress && it.id.isNotBlank() }
            ?.id
        if (activeTurnId != null) {
            gateway.steerTurn(profile, threadId, activeTurnId, text).fold(
                success = {
                    dispatchIfCurrent(profile.hostIdentity, generation) {
                        AppAction.TurnStartAcknowledged(profile.hostIdentity, threadId, activeTurnId)
                    }
                    // A successful steer must be reconciled just like a new
                    // turn so the user's input and any streamed output become
                    // visible even when notifications arrive late.
                    if (state.profileViews[profile.hostIdentity]?.selectedThreadId == threadId) {
                        readThread(profile, threadId, generation)
                    }
                },
                failure = { message ->
                    dispatchIfCurrent(profile.hostIdentity, generation) {
                        AppAction.TurnFailed(profile.hostIdentity, message)
                    }
                },
            )
            return
        }

        val listedStatus = state.cache
            .profile(profile.hostIdentity)
            .threadList
            .lastOrNull { it.id == threadId }
            ?.status
        if (cachedSnapshot?.summary?.status is ThreadStatus.Active || listedStatus is ThreadStatus.Active) {
            gateway.queueTurn(profile, threadId, text).fold(
                success = { queueId ->
                    dispatchIfCurrent(profile.hostIdentity, generation) {
                        AppAction.TurnQueued(profile.hostIdentity, threadId, queueId)
                    }
                },
                failure = { message ->
                    dispatchIfCurrent(profile.hostIdentity, generation) {
                        AppAction.TurnFailed(profile.hostIdentity, message)
                    }
                },
            )
            return
        }

        val cwd = cachedWorkingDirectory(profile.hostIdentity, threadId)
        if (cwd == null) {
            dispatchIfCurrent(profile.hostIdentity, generation) {
                AppAction.TurnFailed(profile.hostIdentity, MissingThreadWorkingDirectoryMessage)
            }
            return
        }
        gateway.startTurn(profile, threadId, cwd, text).fold(
            success = { turnId ->
                dispatchIfCurrent(profile.hostIdentity, generation) {
                    AppAction.TurnStartAcknowledged(profile.hostIdentity, threadId, turnId)
                }
                // A successful turn/start must become visible even when the
                // corresponding live notifications are delayed or use a
                // shape this Mobile Client does not yet project. Reconcile a
                // fresh Snapshot through the existing read barrier so events
                // arriving during the read are still applied in wire order.
                if (state.profileViews[profile.hostIdentity]?.selectedThreadId == threadId) {
                    readThread(profile, threadId, generation)
                }
            },
            failure = { message ->
                dispatchIfCurrent(profile.hostIdentity, generation) {
                    AppAction.TurnFailed(profile.hostIdentity, message)
                }
            },
        )
    }

    suspend fun interrupt(profile: HostProfile, threadId: String, turnId: String) {
        val generation = sessions.currentGeneration(profile.hostIdentity) ?: return
        if (!isConnected(profile.hostIdentity, generation)) return
        dispatchIfCurrent(profile.hostIdentity, generation) {
            AppAction.InterruptStarted(profile.hostIdentity, turnId)
        }
        gateway.interrupt(profile, threadId, turnId).fold(
            success = {
                dispatchIfCurrent(profile.hostIdentity, generation) {
                    AppAction.InterruptFinished(profile.hostIdentity)
                }
            },
            failure = { message ->
                dispatchIfCurrent(profile.hostIdentity, generation) {
                    AppAction.InterruptFinished(profile.hostIdentity)
                }
                dispatchIfCurrent(profile.hostIdentity, generation) {
                    AppAction.TurnFailed(profile.hostIdentity, message)
                }
            },
        )
    }

    private suspend fun listThreads(profile: HostProfile, generation: Long) {
        if (!isConnected(profile.hostIdentity, generation)) return
        dispatchIfCurrent(profile.hostIdentity, generation) {
            AppAction.ThreadListLoading(profile.hostIdentity)
        }
        // The project list is a global Codex Desktop view. A cwd filter would
        // hide threads belonging to the other displayed projects.
        gateway.listThreads(profile, "").fold(
            success = { threads ->
                dispatchIfCurrent(profile.hostIdentity, generation) {
                    AppAction.ThreadListLoaded(profile.hostIdentity, threads)
                }
            },
            failure = { message ->
                dispatchIfCurrent(profile.hostIdentity, generation) {
                    AppAction.ThreadListFailed(profile.hostIdentity, message)
                }
            },
        )
    }

    private suspend fun listProjects(profile: HostProfile, generation: Long) {
        if (!isConnected(profile.hostIdentity, generation)) return
        dispatchIfCurrent(profile.hostIdentity, generation) {
            AppAction.ProjectListLoading(profile.hostIdentity)
        }
        gateway.listProjects(profile).fold(
            success = { projects ->
                dispatchIfCurrent(profile.hostIdentity, generation) {
                    AppAction.ProjectListLoaded(profile.hostIdentity, projects)
                }
            },
            failure = { message ->
                dispatchIfCurrent(profile.hostIdentity, generation) {
                    AppAction.ProjectListFailed(profile.hostIdentity, message)
                }
            },
        )
    }

    private suspend fun readThread(profile: HostProfile, threadId: String, generation: Long) {
        if (!isConnected(profile.hostIdentity, generation)) return
        dispatchIfCurrent(profile.hostIdentity, generation) {
            AppAction.ThreadSelected(profile.hostIdentity, threadId)
        }
        dispatchIfCurrent(profile.hostIdentity, generation) {
            AppAction.ThreadReadLoading(profile.hostIdentity, threadId)
        }
        val token = sessions.beginRead(profile.hostIdentity, threadId, generation) ?: return
        val result = gateway.readThread(profile, threadId)
        // Event callbacks and the read completion share one mutex. Events
        // delivered while the request was in flight are therefore drained
        // after the replacement Snapshot and never race it.
        eventMutex.withLock {
            val completion = sessions.finishRead(token)
            if (
                completion != null &&
                    sessions.isCurrent(profile.hostIdentity, generation) &&
                    state.profileViews[profile.hostIdentity]?.selectedThreadId == threadId
            ) {
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

    private fun isConnected(hostIdentity: String, generation: Long): Boolean =
        sessions.isCurrent(hostIdentity, generation) &&
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

    private companion object {
        const val MissingThreadWorkingDirectoryMessage = "タスクの作業ディレクトリが不明です。タスク一覧を更新してください"
        val ThreadListInvalidatingMethods = setOf(
            "thread/started",
            "thread/name/updated",
            "thread/project/updated",
            "thread/archived",
            "thread/unarchived",
        )
    }

    private inline fun ifCurrent(hostIdentity: String, generation: Long, block: () -> Unit) {
        if (sessions.isCurrent(hostIdentity, generation)) block()
    }

    private inline fun dispatchIfCurrent(
        hostIdentity: String,
        generation: Long,
        action: () -> AppAction,
    ) {
        ifCurrent(hostIdentity, generation) { dispatch(action()) }
    }

    private fun disconnect(hostIdentity: String, generation: Long) {
        if (!sessions.isCurrent(hostIdentity, generation)) return
        // Retire first: retireHost cancels the subscription before the
        // Disconnected action can reduce UI state or notify observers.
        dispatch(AppAction.Disconnected(hostIdentity))
    }

    private suspend fun disconnect(profile: HostProfile, generation: Long) {
        if (!sessions.isCurrent(profile.hostIdentity, generation)) return
        sessions.retireHost(profile.hostIdentity)
        gateway.disconnect(profile)
        eventMutex.withLock { publish(AppAction.Disconnected(profile.hostIdentity)) }
    }
}

private suspend fun <T> GatewayResult<T>.fold(
    success: suspend (T) -> Unit,
    failure: suspend (String) -> Unit,
) = when (this) {
    is GatewayResult.Success -> success(value)
    is GatewayResult.Failure -> failure(message)
}

private fun AppState.restoreDisconnected(): AppState = copy(
    profileViews = profileViews.mapValues { (_, view) ->
        view.copy(
            connection = ConnectionPhase.Disconnected,
            projectList = LoadPhase.Idle,
            threadList = LoadPhase.Idle,
            threadDetail = LoadPhase.Idle,
            interruptingTurnId = null,
        )
    },
)

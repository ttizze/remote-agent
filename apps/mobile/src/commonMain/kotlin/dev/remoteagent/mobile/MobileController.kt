package dev.remoteagent.mobile

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
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
        val hostIdentity = profile.hostIdentity
        sessions.currentGeneration(hostIdentity) ?: return
        // Invalidate callbacks immediately, then serialize the native close
        // behind any connection attempt already in flight for this Host.
        sessions.retireHost(hostIdentity)
        sessions.withHostConnection(hostIdentity) {
            // A reconnect requested after this disconnect is the newer user
            // intent. Its active generation owns the newly installed handle.
            if (sessions.currentGeneration(hostIdentity) != null) return@withHostConnection
            gateway.disconnect(profile)
            publish(AppAction.Disconnected(hostIdentity))
        }
    }

    /** Connect, subscribe, and reconcile the visible Host state before returning. */
    suspend fun connect(profile: HostProfile, scope: CoroutineScope) {
        sessions.withHostConnection(profile.hostIdentity) {
            // Start the generation before touching the transport. A previous
            // subscription is retired by the coordinator before connect can
            // deliver callbacks for the new generation.
            val generation = sessions.beginConnection(profile.hostIdentity)
            dispatch(AppAction.ConnectStarted(profile.hostIdentity))
            try {
                when (val result = gateway.connect(profile)) {
                    is GatewayResult.Failure -> ifCurrent(profile.hostIdentity, generation) {
                        sessions.retireHost(profile.hostIdentity)
                        gateway.disconnect(profile)
                        dispatch(AppAction.ConnectFailed(profile.hostIdentity, result.message))
                    }

                    is GatewayResult.Success -> {
                        if (!sessions.isCurrent(profile.hostIdentity, generation)) {
                            gateway.disconnect(profile)
                            return@withHostConnection
                        }
                        dispatch(AppAction.ConnectSucceeded(profile.hostIdentity))
                        val subscription = try {
                            gateway.subscribeRaw(profile) { message ->
                                val event = (message as? RawCodexMessage.Notification)?.let {
                                    codexThreadEvent(it.method, it.params, it.extensions)
                                }
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
                                scope.launch(start = CoroutineStart.UNDISPATCHED) {
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
                                }
                            }
                        } catch (cancelled: CancellationException) {
                            throw cancelled
                        } catch (_: Throwable) {
                            disconnect(profile, generation)
                            return@withHostConnection
                        }
                        if (!sessions.installSubscription(profile.hostIdentity, generation, subscription)) {
                            gateway.disconnect(profile)
                            return@withHostConnection
                        }

                        // A reconnect must never leave the user looking at an old
                        // list. Loading the list first also restores the list screen
                        // for a profile that had no selected thread.
                        val selectedThreadId = state.profileViews[profile.hostIdentity]?.selectedThreadId
                        listThreads(profile, generation)
                        if (
                            sessions.isCurrent(profile.hostIdentity, generation) &&
                                state.profileViews[profile.hostIdentity]?.threadList == LoadPhase.Ready &&
                                selectedThreadId != null
                        ) {
                            readThread(profile, selectedThreadId, generation)
                        }
                    }
                }
            } catch (cancelled: CancellationException) {
                // A blocking platform connect may install its handle just as
                // the caller is cancelled. Close it before the per-Host
                // connection mutex admits a reconnect.
                sessions.retireHost(profile.hostIdentity)
                withContext(NonCancellable) { gateway.disconnect(profile) }
                publish(AppAction.Disconnected(profile.hostIdentity))
                throw cancelled
            }
        }
    }

    suspend fun listThreads(profile: HostProfile) {
        val generation = sessions.currentGeneration(profile.hostIdentity) ?: return
        listThreads(profile, generation)
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

    suspend fun startTurn(profile: HostProfile, threadId: String, text: String) {
        val generation = sessions.currentGeneration(profile.hostIdentity) ?: return
        if (!isConnected(profile.hostIdentity, generation)) return
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
        val cwd = state.profileViews[profile.hostIdentity]?.workingDirectoryPath.orEmpty()
        dispatchIfCurrent(profile.hostIdentity, generation) {
            AppAction.ThreadListLoading(profile.hostIdentity)
        }
        gateway.listThreads(profile, cwd).fold(
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
        publish(AppAction.Disconnected(profile.hostIdentity))
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
            threadList = LoadPhase.Idle,
            threadDetail = LoadPhase.Idle,
            interruptingTurnId = null,
        )
    },
)

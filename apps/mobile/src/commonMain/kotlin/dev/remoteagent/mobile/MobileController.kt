package dev.remoteagent.mobile

import kotlin.random.Random
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/**
 * Shared application module used by the Android Compose and iOS SwiftUI
 * presentation adapters. Platform UI code observes state and invokes these
 * intent-level operations; transport and reconciliation remain hidden here.
 */
internal class MobileController(
    private val gateway: HostGateway,
    repository: MobileRepository,
    persistenceScope: CoroutineScope,
    private val cacheLimits: MobileCacheLimits = MobileCacheLimits(maxTurnsPerThread = Int.MAX_VALUE),
    private val deferHistoryItemDetails: Boolean = false,
    private val clientUserMessageIdGenerator: () -> String = { "${Random.Default.nextLong()}-${Random.Default.nextLong()}" },
) {
    var state = repository.load().restoreDisconnected()
        private set
    /** Sanitized diagnostic only; persistence failures never become UI notices. */
    var lastPersistenceFailureType: String? = null
        private set
    private var saveRevision = 0L
    private val savedRevision = MutableStateFlow(0L)
    private val saves = Channel<Pair<Long, AppState>>(Channel.CONFLATED)
    private val saveJob = persistenceScope.launch {
        for (first in saves) {
            // Coalesce a fixed window; continuous streaming must not postpone saves forever.
            delay(200)
            val (revision, latest) = saves.tryReceive().getOrNull() ?: first
            lastPersistenceFailureType = try {
                withContext(Dispatchers.Default) { repository.save(latest) }
                null
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: Exception) {
                failure::class.simpleName ?: "PersistenceFailure"
            }
            savedRevision.value = revision
        }
    }

    suspend fun flushPersistence() {
        val target = saveRevision
        if (savedRevision.value >= target) return
        check(saveJob.isActive) { "Persistence worker is not running" }
        savedRevision.first { it >= target }
    }

    private var reconnectJob: Job? = null
    private var reconnectHostId: String? = null
    private var foregroundRefreshJob: Job? = null
    private var threadWatchTarget: ThreadWatchTarget? = null
    private var threadWatchJob: Job? = null
    private var threadWatchChanges: Channel<Unit>? = null
    private var threadWatchRevision = 0L

    private var historyNavigation = 0L
    private val startingConversations = mutableSetOf<String>()
    private val listLoads = mutableMapOf<String, Long>()
    private val pendingListRefresh = mutableSetOf<String>()

    private val eventMutex = Mutex()
    private val sessions = HostSessionCoordinator(cacheLimits)
    private val observers = mutableSetOf<(AppState) -> Unit>()

    /** Keep restoring the selected Host while the application scope is alive. */
    fun maintainConnection(scope: CoroutineScope): HostEventSubscription {
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

    private fun ensureVisibleThreadWatch(scope: CoroutineScope) {
        val profile = state.selectedProfile
        val view = state.selectedView
        val snapshot = profile?.let { host -> view.selectedThreadId?.let { state.cache.snapshot(host.id, it) } }
        val path = snapshot?.raw?.string("path")?.takeIf(String::isNotBlank)
        val generation = profile?.let { sessions.currentGeneration(it.id) }
        val target = if (profile != null && generation != null && path != null &&
            view.connection == ConnectionPhase.Connected &&
            (view.threadDetail is LoadPhase.Ready || view.threadDetail is LoadPhase.Failed) &&
            snapshot.summary.status == ThreadStatus.NotLoaded
        ) ThreadWatchTarget(profile, snapshot.summary.id, path, generation) else null
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
        val job = scope.launch(start = CoroutineStart.LAZY) {
            try {
                val registered = gateway.rawRequest(target.profile, "host/thread/watch", buildJsonObject {
                    put("watchId", revision)
                    put("threadId", target.threadId)
                    put("path", target.path)
                })
                if (registered is GatewayResult.Failure) {
                    if (threadWatchTarget == target && threadWatchRevision == revision) {
                        dispatch(AppAction.ThreadReadFailed(target.profile.id, "会話の自動更新を開始できません: ${registered.message}"))
                    }
                    return@launch
                }
                // Close the gap between the initial history read and installing
                // the OS watch, without replacing the screen with a loader.
                changes.trySend(Unit)
                for (change in changes) {
                    delay(100)
                    while (changes.tryReceive().isSuccess) { }
                    if (threadWatchTarget != target || threadWatchRevision != revision) return@launch
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
        threadWatchJob = job
        job.start()
    }

    private fun receiveThreadWatchNotification(hostIdentity: String, message: RawCodexMessage.Notification) {
        val target = threadWatchTarget ?: return
        val params = message.params.asObjectOrNull() ?: return
        if (target.profile.id != hostIdentity || params.long("watchId") != threadWatchRevision ||
            params.string("threadId") != target.threadId) return
        if (message.method == "host/thread/watchFailed") {
            dispatch(AppAction.ThreadReadFailed(hostIdentity, "会話の自動更新が停止しました。再読み込みしてください。"))
        } else threadWatchChanges?.trySend(Unit)
    }

    fun restoreConnection(scope: CoroutineScope) {
        if (state.selectedView.connection == ConnectionPhase.Connected) {
            if (foregroundRefreshJob?.isActive == true || reconnectJob?.isActive == true) return
            val profile = state.selectedProfile ?: return
            val generation = sessions.currentGeneration(profile.id) ?: return
            val job = scope.launch(start = CoroutineStart.LAZY) {
                refreshVisibleState(profile, generation)
            }
            foregroundRefreshJob = job
            job.start()
            return
        }
        if (state.selectedView.connection == ConnectionPhase.Connecting) return
        reconnectJob?.cancel()
        reconnectJob = null
        ensureSelectedConnection(scope)
    }

    fun openApp(scope: CoroutineScope) {
        state.selectedProfileId?.let { dispatch(AppAction.ThreadListOpened(it)) }
        restoreConnection(scope)
    }

    private fun ensureSelectedConnection(scope: CoroutineScope) {
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
        if (profile == null || state.showingPairing ||
            state.selectedView.connection == ConnectionPhase.Connected || reconnectJob?.isActive == true) return
        val job = scope.launch(start = CoroutineStart.LAZY) {
            var retryDelay = 1_000L
            while (state.selectedProfileId == profile.id) {
                connect(state.selectedProfile ?: break, scope)
                if (state.selectedView.connection == ConnectionPhase.Connected) break
                delay(retryDelay)
                retryDelay = (retryDelay * 2).coerceAtMost(30_000L)
            }
        }
        reconnectJob = job
        job.start()
    }

    fun observe(observer: (AppState) -> Unit): HostEventSubscription {
        observers += observer
        observer(state)
        return HostEventSubscription { observers -= observer }
    }

    fun dispatch(action: AppAction) {
        if (action is AppAction.ThreadSelected || action is AppAction.ThreadListOpened || action is AppAction.Disconnected || action is AppAction.ProfileSelected || action is AppAction.ProfileSelectionOpened || action is AppAction.NewThreadOpened) historyNavigation++
        if (action is AppAction.Disconnected) sessions.retireHost(action.hostIdentity)
        publish(action)
    }

    private fun publish(action: AppAction) {
        val updated = reduce(state, action, cacheLimits)
        if (updated === state) return
        state = updated
        saves.trySend(++saveRevision to state)
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
            success = { dispatch(AppAction.AddressesDiscovered(profile.id, it)) },
            failure = { dispatch(AppAction.ConnectFailed(profile.id, it)) },
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
            val hostIdentity = profile.id
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
        sessions.withHostConnection(profile.id) {
            if (state.profileViews[profile.id]?.connection == ConnectionPhase.Connected &&
                sessions.currentGeneration(profile.id) != null) return@withHostConnection
            // Start the generation before touching the transport. A previous
            // subscription is retired by the coordinator before connect can
            // deliver callbacks for the new generation.
            val generation = eventMutex.withLock {
                sessions.beginConnection(profile.id).also {
                    publish(AppAction.ConnectStarted(profile.id))
                }
            }
            try {
                val connectionProfile = when (val discovered = gateway.discover(profile)) {
                    is GatewayResult.Success -> discovered.value
                        .map(String::trim)
                        .filter(String::isNotEmpty)
                        .distinct()
                        .takeIf(List<String>::isNotEmpty)
                        ?.let { relayUrls ->
                            dispatch(AppAction.AddressesDiscovered(profile.id, relayUrls))
                            profile.copy(relayUrl = relayUrls.first())
                        }
                        ?: profile
                    is GatewayResult.Failure -> profile
                }
                if (!sessions.isCurrent(profile.id, generation)) return@withHostConnection

                when (val result = gateway.connect(connectionProfile)) {
                    is GatewayResult.Failure -> ifCurrent(profile.id, generation) {
                        sessions.retireHost(profile.id)
                        gateway.disconnect(connectionProfile)
                        dispatch(AppAction.ConnectFailed(profile.id, result.message))
                    }

                    is GatewayResult.Success -> {
                        if (!sessions.isCurrent(profile.id, generation)) {
                            gateway.disconnect(connectionProfile)
                            return@withHostConnection
                        }
                        dispatch(AppAction.ConnectSucceeded(profile.id))
                        val subscription = try {
                            gateway.subscribeRaw(
                                profile = connectionProfile,
                                onMessage = { message ->
                                    val notification = message as? RawCodexMessage.Notification
                                    val refreshProjects = notification?.method == "project/changed"
                                    val refreshThreads = notification?.method in ThreadListInvalidatingMethods
                                    val threadWatchEvent = notification?.method in ThreadWatchMethods
                                    val event = message
                                        .takeUnless { refreshProjects || refreshThreads || threadWatchEvent }
                                        ?.let(::codexThreadEvent)
                                    // Capture the generation and read barrier before
                                    // yielding. Native transports may invoke this
                                    // callback synchronously while a read is in flight.
                                    val current = sessions.isCurrent(profile.id, generation)
                                    val buffered = if (current) {
                                        event?.let {
                                            sessions.bufferEvent(profile.id, it).isHeld
                                        } == true
                                    } else {
                                        false
                                    }
                                    // Platform controllers provide their serialized
                                    // application/UI scope. Never reduce state
                                    // inline on a native poller thread.
                                    scope.launch {
                                        eventMutex.withLock {
                                            if (sessions.isCurrent(profile.id, generation)) {
                                                // Keep raw and typed projections in one
                                                // serialized transition. This preserves
                                                // wire arrival order for live output.
                                                dispatch(AppAction.HostMessageReceived(
                                                    profile.id, message, event?.takeUnless { buffered },
                                                ))
                                            }
                                        }
                                        if (sessions.isCurrent(profile.id, generation)) {
                                            if (refreshProjects || refreshThreads) listThreads(connectionProfile, generation)
                                            if (threadWatchEvent && notification != null) receiveThreadWatchNotification(profile.id, notification)
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
                        if (!sessions.installSubscription(profile.id, generation, subscription)) {
                            gateway.disconnect(connectionProfile)
                            return@withHostConnection
                        }

                        refreshVisibleState(connectionProfile, generation)
                    }
                }
            } catch (cancelled: CancellationException) {
                // A blocking platform connect may install its handle just as
                // the caller is cancelled. Close it before the per-Host
                // connection mutex admits a reconnect.
                withContext(NonCancellable) {
                    sessions.retireHost(profile.id)
                    gateway.disconnect(profile)
                    eventMutex.withLock { publish(AppAction.Disconnected(profile.id)) }
                }
                throw cancelled
            }
        }
    }

    suspend fun showThreadList(profile: HostProfile) {
        val generation = sessions.currentGeneration(profile.id) ?: return
        dispatchIfCurrent(profile.id, generation) { AppAction.ThreadListOpened(profile.id) }
        listThreads(profile, generation)
    }

    suspend fun listThreads(profile: HostProfile) {
        val generation = sessions.currentGeneration(profile.id) ?: return
        listThreads(profile, generation)
    }

    suspend fun expandTaskList(profile: HostProfile, projects: Boolean, projectId: String? = null) {
        dispatch(AppAction.ThreadListExpanded(profile.id, projects, projectId))
        val generation = sessions.currentGeneration(profile.id) ?: return
        listThreads(profile, generation, refresh = false)
    }

    suspend fun searchTaskList(profile: HostProfile, term: String) {
        if (state.profileViews[profile.id]?.threadSearchTerm == term.trim()) return
        dispatch(AppAction.ThreadListSearchChanged(profile.id, term.trim()))
        val generation = sessions.currentGeneration(profile.id) ?: return
        listThreads(profile, generation, refresh = false)
    }

    suspend fun readThread(profile: HostProfile, threadId: String) {
        val generation = sessions.currentGeneration(profile.id) ?: return
        readThread(profile, threadId, generation)
    }

    fun openNewThread(profile: HostProfile, cwd: String) {
        dispatch(AppAction.NewThreadOpened(profile.id, cwd))
    }

    suspend fun sendMessage(profile: HostProfile, text: String, attachments: List<CodexAttachment> = emptyList()): MessageSendResult {
        val generation = sessions.currentGeneration(profile.id) ?: return MessageSendResult(false, null)
        if (!isConnected(profile.id, generation) || (text.isBlank() && attachments.isEmpty())) return MessageSendResult(false, null)
        val view = state.profileViews[profile.id] ?: return MessageSendResult(false, null)
        view.selectedThreadId?.let { return MessageSendResult(startTurn(profile, it, text, attachments), it) }
        val cwd = view.newThreadCwd ?: return MessageSendResult(false, null)
        if (!startingConversations.add(profile.id)) return MessageSendResult(false, null)
        try {
            val result = gateway.startThread(profile, cwd)
            val snapshot = when (result) {
                is GatewayResult.Failure -> {
                    dispatchIfCurrent(profile.id, generation) { AppAction.ThreadStartFailed(profile.id, result.message) }
                    return MessageSendResult(false, null)
                }
                is GatewayResult.Success -> result.value
            }
            if (!isConnected(profile.id, generation)) return MessageSendResult(false, snapshot.summary.id)
            val current = state.profileViews[profile.id]
            dispatchIfCurrent(profile.id, generation) {
                AppAction.SnapshotReceived(profile.id, ThreadReadResult(snapshot, emptyList()),
                    select = current?.selectedThreadId == null && current?.newThreadCwd == cwd)
            }
            // Keep the created thread if the first turn fails. A retry uses the
            // normal send path and cannot create another empty conversation.
            return MessageSendResult(startTurn(profile, snapshot.summary.id, text, attachments), snapshot.summary.id)
        } finally {
            startingConversations.remove(profile.id)
        }
    }

    suspend fun startTurn(profile: HostProfile, threadId: String, text: String, attachments: List<CodexAttachment> = emptyList()): Boolean {
        var acknowledged = false
        val generation = sessions.currentGeneration(profile.id) ?: return false
        if (!isConnected(profile.id, generation)) return false

        val cachedSnapshot = state.cache
            .snapshot(profile.id, threadId)
        val clientId = clientUserMessageIdGenerator()
        val displayText = buildString {
            append(text)
            attachments.forEach { attachment ->
                if (attachment.isImage) return@forEach
                if (isNotEmpty()) append('\n')
                append(attachmentMessageLabel(attachment.isImage, attachment.path, attachment.name))
            }
        }
        fun submission(turnId: String?) = SubmittedMessage(
            clientId, displayText, turnId,
            cachedSnapshot?.turns?.firstOrNull { it.id == turnId }?.items?.lastOrNull()?.id,
            attachments.mapNotNull { if (it.isImage) it.path else null },
        )
        val activeTurnId = cachedSnapshot
            ?.turns
            ?.lastOrNull { it.status == TurnStatus.InProgress && it.id.isNotBlank() }
            ?.id
        if (activeTurnId != null) {
            gateway.steerTurn(profile, threadId, activeTurnId, text, attachments, clientId).fold(
                success = {
                    acknowledged = true
                    dispatchIfCurrent(profile.id, generation) {
                        AppAction.MessageAccepted(profile.id, threadId, submission(activeTurnId))
                    }
                },
                failure = { message ->
                    dispatchIfCurrent(profile.id, generation) {
                        AppAction.TurnFailed(profile.id, message)
                    }
                },
            )
            return acknowledged
        }

        val listedStatus = state.cache
            .profile(profile.id)
            .threadList
            .lastOrNull { it.id == threadId }
            ?.status
        if (cachedSnapshot?.summary?.status is ThreadStatus.Active || listedStatus is ThreadStatus.Active) {
            gateway.queueTurn(profile, threadId, text, attachments, clientId).fold(
                success = { _ ->
                    acknowledged = true
                    dispatchIfCurrent(profile.id, generation) {
                        AppAction.MessageAccepted(profile.id, threadId, submission(null))
                    }
                },
                failure = { message ->
                    dispatchIfCurrent(profile.id, generation) {
                        AppAction.TurnFailed(profile.id, message)
                    }
                },
            )
            return acknowledged
        }

        val cwd = cachedWorkingDirectory(profile.id, threadId)
        if (cwd == null) {
            dispatchIfCurrent(profile.id, generation) {
                AppAction.TurnFailed(profile.id, MissingThreadWorkingDirectoryMessage)
            }
            return acknowledged
        }
        gateway.startTurn(profile, threadId, cwd, text, attachments,
            resume = cachedSnapshot == null || cachedSnapshot.summary.status == ThreadStatus.NotLoaded,
            clientUserMessageId = clientId,
        ).fold(
            success = { turnId ->
                acknowledged = true
                dispatchIfCurrent(profile.id, generation) {
                    AppAction.MessageAccepted(profile.id, threadId, submission(turnId))
                }
                // turn/start acknowledges before rollout persistence. Keep the
                // live snapshot; subscribed item events carry the input/output.
            },
            failure = { message ->
                dispatchIfCurrent(profile.id, generation) {
                    AppAction.TurnFailed(profile.id, message)
                }
            },
        )
        return acknowledged
    }

    suspend fun respond(profile: HostProfile, requestId: kotlinx.serialization.json.JsonElement, response: kotlinx.serialization.json.JsonElement): GatewayResult<Unit> {
        val generation = sessions.currentGeneration(profile.id)
            ?: return GatewayResult.Failure("接続が切れています")
        if (!isConnected(profile.id, generation)) return GatewayResult.Failure("接続が切れています")
        val result = gateway.respondResult(profile, requestId, response)
        if (result is GatewayResult.Failure) dispatchIfCurrent(profile.id, generation) {
            AppAction.TurnFailed(profile.id, result.message)
        }
        return result
    }

    suspend fun interrupt(profile: HostProfile, threadId: String, turnId: String) {
        val generation = sessions.currentGeneration(profile.id) ?: return
        if (!isConnected(profile.id, generation)) return
        dispatchIfCurrent(profile.id, generation) {
            AppAction.InterruptStarted(profile.id, turnId)
        }
        gateway.interrupt(profile, threadId, turnId).fold(
            success = {
                dispatchIfCurrent(profile.id, generation) {
                    AppAction.InterruptFinished(profile.id)
                }
            },
            failure = { message ->
                dispatchIfCurrent(profile.id, generation) {
                    AppAction.InterruptFinished(profile.id)
                }
                dispatchIfCurrent(profile.id, generation) {
                    AppAction.TurnFailed(profile.id, message)
                }
            },
        )
    }

    private suspend fun refreshVisibleState(profile: HostProfile, generation: Long) {
        listThreads(profile, generation)
        state.profileViews[profile.id]?.selectedThreadId?.let { threadId ->
            readThread(profile, threadId, generation)
        }
    }

    private fun threadListQuery(hostIdentity: String): ThreadListQuery {
        val view = state.profileViews[hostIdentity] ?: ProfileViewState()
        return ThreadListQuery(view.visibleProjectCount, view.visibleChatCount, view.projectThreadLimits, view.threadSearchTerm)
    }

    private suspend fun listThreads(profile: HostProfile, generation: Long, refresh: Boolean = true) {
        if (!isConnected(profile.id, generation)) return
        if (listLoads[profile.id] == generation) {
            pendingListRefresh += profile.id
            return
        }
        listLoads[profile.id] = generation
        try {
            do {
                pendingListRefresh.remove(profile.id)
                val query = threadListQuery(profile.id)
                dispatchIfCurrent(profile.id, generation) { AppAction.ThreadListLoading(profile.id, append = !refresh) }
                val result = gateway.listThreads(profile, query)
                if (query != threadListQuery(profile.id)) continue
                when (result) {
                    is GatewayResult.Failure -> {
                        dispatchIfCurrent(profile.id, generation) { AppAction.ThreadListFailed(profile.id, result.message) }
                        return
                    }
                    is GatewayResult.Success -> {
                        val page = result.value
                        dispatchIfCurrent(profile.id, generation) {
                            AppAction.ThreadListLoaded(profile.id, page.threads, page.projects, page.moreProjectIds, page.hasMoreChats, page.hasMoreProjects)
                        }
                    }
                }
            } while (isConnected(profile.id, generation) && profile.id in pendingListRefresh)
        } finally {
            if (listLoads[profile.id] == generation) listLoads.remove(profile.id)
        }
    }

    suspend fun loadOlderHistory(profile: HostProfile, turnId: String? = null) {
        val generation = sessions.currentGeneration(profile.id) ?: return
        val view = state.profileViews[profile.id] ?: return
        val threadId = view.selectedThreadId ?: return
        if (view.loadingHistory || !isConnected(profile.id, generation)) return
        val snapshot = state.cache.snapshot(profile.id, threadId) ?: return
        val cursor = if (turnId == null) snapshot.olderTurnsCursor else
            snapshot.turns.firstOrNull { it.id == turnId }?.olderItemsCursor
        if (turnId == null && cursor == null) return
        if (turnId != null && snapshot.turns.firstOrNull { it.id == turnId }?.hasOlderItems != true) return
        val navigation = historyNavigation
        dispatch(AppAction.HistoryLoading(profile.id, true))
        try {
            val result = CommonCodexClient(gateway, deferItemDetails = deferHistoryItemDetails)
                .readOlderHistory(profile, threadId, cursor, turnId)
            eventMutex.withLock {
                if (!isConnected(profile.id, generation) || historyNavigation != navigation) return@withLock
                val current = state.cache.snapshot(profile.id, threadId) ?: return@withLock
                val currentCursor = if (turnId == null) current.olderTurnsCursor else
                    current.turns.firstOrNull { it.id == turnId }?.olderItemsCursor
                if (currentCursor != cursor || (turnId != null && current.turns.firstOrNull { it.id == turnId }?.hasOlderItems != true)) return@withLock
                when (result) {
                    is GatewayResult.Success -> dispatch(AppAction.HistoryReceived(profile.id,
                        mergeOlderHistory(current, result.value, turnId)))
                    is GatewayResult.Failure -> dispatch(AppAction.HistoryLoading(profile.id, false, result.message))
                }
            }
        } finally {
            if (isConnected(profile.id, generation) && historyNavigation == navigation &&
                state.profileViews[profile.id]?.loadingHistory == true) dispatch(AppAction.HistoryLoading(profile.id, false))
        }
    }

    private suspend fun readThread(profile: HostProfile, threadId: String, generation: Long, background: Boolean = false) {
        if (!isConnected(profile.id, generation)) return
        if (background) {
            if (state.profileViews[profile.id]?.selectedThreadId != threadId) return
        } else {
            dispatchIfCurrent(profile.id, generation) { AppAction.ThreadSelected(profile.id, threadId) }
            dispatchIfCurrent(profile.id, generation) { AppAction.ThreadReadLoading(profile.id, threadId) }
        }
        val token = sessions.beginRead(profile.id, threadId, generation) ?: return
        try {
            val result = gateway.readThread(profile, threadId)
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
                        dispatch(
                            AppAction.ThreadReadFailed(
                                profile.id,
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
                                        profile.id,
                                        snapshot.copy(bufferedEvents = buffered),
                                    ),
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
            sessions.finishRead(token)
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
        val ThreadWatchMethods = setOf("host/thread/changed", "host/thread/watchFailed")
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
        if (!sessions.isCurrent(profile.id, generation)) return
        sessions.retireHost(profile.id)
        gateway.disconnect(profile)
        eventMutex.withLock { publish(AppAction.Disconnected(profile.id)) }
    }
}

private data class ThreadWatchTarget(val profile: HostProfile, val threadId: String, val path: String, val generation: Long)

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

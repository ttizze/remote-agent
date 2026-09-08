package dev.remoteagent.mobile

import kotlin.random.Random
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.withContext

/**
 * Shared application module used by the Android Compose and iOS SwiftUI presentation adapters. Platform UI code
 * observes state and invokes these intent-level operations; transport and reconciliation remain hidden here.
 */
internal class MobileController(
    internal val gateway: HostGateway,
    repository: MobileRepository,
    persistenceScope: CoroutineScope,
    private val cacheLimits: MobileCacheLimits = MobileCacheLimits(maxTurnsPerThread = Int.MAX_VALUE),
    deferHistoryItemDetails: Boolean = false,
    internal val clientUserMessageIdGenerator: () -> String = {
        "${Random.Default.nextLong()}-${Random.Default.nextLong()}"
    },
) {
    var state = repository.load().restoreDisconnected()
        private set

    /** Sanitized diagnostic only; persistence failures never become UI notices. */
    var lastPersistenceFailureType: String? = null
        private set

    private var saveRevision = 0L
    private var persistenceDirty = false
    private val savedRevision = MutableStateFlow(0L)
    private val saves = Channel<Pair<Long, AppState>>(Channel.CONFLATED)
    private val saveJob = persistenceScope.launch {
        for (first in saves) {
            // Coalesce durable checkpoints; streamed deltas remain in memory until a checkpoint or flush.
            delay(PERSISTENCE_COALESCE_MS)
            val (revision, latest) = saves.tryReceive().getOrNull() ?: first
            lastPersistenceFailureType =
                try {
                    withContext(Dispatchers.Default) { repository.save(latest) }
                    null
                } catch (cancelled: CancellationException) {
                    throw cancelled
                } catch (failure: MobilePersistenceException) {
                    failure.cause?.let { it::class.simpleName } ?: "PersistenceFailure"
                } catch (failure: IllegalStateException) {
                    failure::class.simpleName ?: "PersistenceFailure"
                } catch (failure: IllegalArgumentException) {
                    failure::class.simpleName ?: "PersistenceFailure"
                }
            savedRevision.value = revision
        }
    }

    suspend fun flushPersistence() {
        if (persistenceDirty) saveState()
        val target = saveRevision
        if (savedRevision.value >= target) return
        check(saveJob.isActive) { "Persistence worker is not running" }
        savedRevision.first { it >= target }
    }

    internal var reconnectJob: Job? = null
    internal var reconnectHostId: String? = null
    internal var foregroundRefreshJob: Job? = null
    internal var threadWatchTarget: ThreadWatchTarget? = null
    internal var threadWatchJob: Job? = null
    internal var threadWatchChanges: Channel<Unit>? = null
    internal var threadWatchRevision = 0L

    internal var historyNavigation = 0L
    internal val startingConversations = mutableSetOf<String>()
    internal val listLoads = mutableMapOf<String, Long>()
    internal val pendingListRefresh = mutableSetOf<String>()

    internal val eventMutex = Mutex()
    internal val sessions = HostSessionCoordinator(cacheLimits)
    internal val historyClient = CommonCodexClient(gateway, deferItemDetails = deferHistoryItemDetails)
    internal val observers = mutableSetOf<(AppState) -> Unit>()

    fun observe(observer: (AppState) -> Unit): HostEventSubscription {
        observers += observer
        observer(state)
        return HostEventSubscription { observers -= observer }
    }

    fun dispatch(action: AppAction) {
        when (action) {
            is AppAction.ThreadSelected,
            is AppAction.ThreadListOpened,
            is AppAction.Disconnected,
            is AppAction.ProfileSelected,
            is AppAction.ProfileSelectionOpened,
            is AppAction.NewThreadOpened -> historyNavigation++
            else -> Unit
        }
        if (action is AppAction.Disconnected) sessions.retireHost(action.hostIdentity)
        publish(action)
    }

    internal fun publish(action: AppAction) {
        val updated = reduce(state, action, cacheLimits)
        if (updated === state) return
        state = updated
        persistenceDirty = true
        val checkpoint =
            when (action) {
                is AppAction.HostEventReceived -> action.event is ThreadEvent.TurnCompleted
                is AppAction.SnapshotReceived -> action.result.thread.turns.none { it.status == TurnStatus.InProgress }
                else -> true
            }
        if (checkpoint) saveState()
        observers.toList().forEach { it(state) }
    }

    private fun saveState() {
        persistenceDirty = false
        saves.trySend(++saveRevision to state)
    }

    suspend fun pair(contents: String, nowMs: Long) {
        when (val parsed = parsePairingQr(contents, nowMs)) {
            is PairingQrResult.Invalid -> dispatch(AppAction.PairingFailed(parsed.reason.name))
            is PairingQrResult.Valid ->
                gateway
                    .pair(parsed.payload)
                    .fold(
                        success = { dispatch(AppAction.ProfilePaired(it)) },
                        failure = { dispatch(AppAction.PairingFailed(it)) },
                    )
        }
    }

    suspend fun discover(profile: HostProfile) {
        gateway
            .discover(profile)
            .fold(
                success = { dispatch(AppAction.AddressesDiscovered(profile.id, it)) },
                failure = { dispatch(AppAction.ConnectFailed(profile.id, it)) },
            )
    }

    internal fun isConnected(hostIdentity: String, generation: Long): Boolean =
        sessions.isCurrent(hostIdentity, generation) &&
            state.profileViews[hostIdentity]?.connection == ConnectionPhase.Connected

    internal inline fun ifCurrent(hostIdentity: String, generation: Long, block: () -> Unit) {
        if (sessions.isCurrent(hostIdentity, generation)) block()
    }

    internal inline fun dispatchIfCurrent(hostIdentity: String, generation: Long, action: () -> AppAction) {
        ifCurrent(hostIdentity, generation) { dispatch(action()) }
    }
}

private fun AppState.restoreDisconnected(): AppState =
    copy(
        profileViews =
            profileViews.mapValues { (_, view) ->
                view.copy(
                    connection = ConnectionPhase.Disconnected,
                    threadList = LoadPhase.Idle,
                    threadDetail = LoadPhase.Idle,
                    interruptingTurnId = null,
                )
            }
    )

private const val PERSISTENCE_COALESCE_MS = 200L

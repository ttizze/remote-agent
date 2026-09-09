package dev.remoteagent.mobile

import kotlin.random.Random
import kotlinx.atomicfu.AtomicRef
import kotlinx.atomicfu.atomic
import kotlinx.atomicfu.getAndUpdate
import kotlinx.atomicfu.update
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

/** Immutable mobile lifecycle value. Rust owns the shared agent transitions used by [reduce]. */
internal data class MobileApp(
    val state: AppState,
    val effects: MobileEffects,
    val saveRevision: Long = 0,
    val persistenceDirty: Boolean = false,
    val reconnectJob: Job? = null,
    val reconnectHostId: String? = null,
    val foregroundRefreshJob: Job? = null,
    val watch: MobileThreadWatch? = null,
    val nextWatchRevision: Long = 0,
    val historyNavigation: Long = 0,
    val startingConversations: Set<String> = emptySet(),
    val listLoads: Map<String, Long> = emptyMap(),
    val pendingListRefresh: Set<String> = emptySet(),
    val settingsContext: SettingsContext? = null,
    val accountPolling: Job? = null,
    val settingsState: AgentSettingsState = AgentSettingsState(),
    val settingsObservers: Set<(AgentSettingsState) -> Unit> = emptySet(),
    val observers: Set<(AppState) -> Unit> = emptySet(),
)

/** Concrete I/O capabilities and resource handles; transitions never execute these effects. */
internal data class MobileEffects(
    val gateway: HostGateway,
    val scope: CoroutineScope,
    val cacheLimits: MobileCacheLimits,
    val deferHistoryItemDetails: Boolean,
    val newMessageId: () -> String,
    val saves: Channel<Pair<Long, AppState>>,
    val saved: MutableStateFlow<SavedMobileState>,
    val saveJob: Job,
    val sessions: AtomicRef<HostSessions>,
    val eventMutex: Mutex = Mutex(),
)

internal data class SavedMobileState(val revision: Long = 0, val failureType: String? = null)

internal fun mobileApp(
    gateway: HostGateway,
    repository: MobileRepository,
    persistenceScope: CoroutineScope,
    cacheLimits: MobileCacheLimits = MobileCacheLimits(maxTurnsPerThread = Int.MAX_VALUE),
    deferHistoryItemDetails: Boolean = false,
    clientUserMessageIdGenerator: () -> String = { "${Random.Default.nextLong()}-${Random.Default.nextLong()}" },
): AtomicRef<MobileApp> {
    val initial = repository.load().restoreDisconnected()
    val saves = Channel<Pair<Long, AppState>>(Channel.CONFLATED)
    val saved = MutableStateFlow(SavedMobileState())
    val saveJob = persistenceScope.launch {
        for (first in saves) {
            delay(PERSISTENCE_COALESCE_MS)
            val (revision, latest) = saves.tryReceive().getOrNull() ?: first
            val failure =
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
            saved.value = SavedMobileState(revision, failure)
        }
    }
    return atomic(
        MobileApp(
            initial,
            MobileEffects(
                gateway,
                persistenceScope,
                cacheLimits,
                deferHistoryItemDetails,
                clientUserMessageIdGenerator,
                saves,
                saved,
                saveJob,
                hostSessions(cacheLimits),
            ),
        )
    )
}

internal val AtomicRef<MobileApp>.state: AppState
    get() = value.state
internal val AtomicRef<MobileApp>.settingsState: AgentSettingsState
    get() = value.settingsState
internal val AtomicRef<MobileApp>.lastPersistenceFailureType: String?
    get() = value.effects.saved.value.failureType

internal suspend fun AtomicRef<MobileApp>.flushPersistence() {
    if (value.persistenceDirty) saveState()
    val target = value.saveRevision
    val effects = value.effects
    if (effects.saved.value.revision >= target) return
    check(effects.saveJob.isActive) { "Persistence worker is not running" }
    effects.saved.first { it.revision >= target }
}

internal fun AtomicRef<MobileApp>.observe(observer: (AppState) -> Unit): HostEventSubscription {
    update { it.copy(observers = it.observers + observer) }
    observer(state)
    return HostEventSubscription { update { it.copy(observers = it.observers - observer) } }
}

internal fun AtomicRef<MobileApp>.dispatch(action: AppAction) {
    if (action is AppAction.Disconnected) value.effects.sessions.retireHost(action.hostIdentity)
    publish(action, navigate = true)
}

internal fun AtomicRef<MobileApp>.publish(action: AppAction, navigate: Boolean = false) {
    val previous = getAndUpdate { it.after(action, navigate) }
    if (previous.state === state) return
    syncSettingsHost()
    val checkpoint =
        when (action) {
            is AppAction.HostEventReceived -> action.event.kind == ConversationEventKind.TurnCompleted
            is AppAction.SnapshotReceived -> action.result.thread.turns.none { it.status == TurnStatus.InProgress }
            else -> true
        }
    if (checkpoint) saveState()
    value.observers.forEach { it(state) }
}

private fun MobileApp.after(action: AppAction, navigate: Boolean): MobileApp {
    val moved =
        navigate &&
            when (action) {
                is AppAction.ThreadSelected,
                is AppAction.ThreadListOpened,
                is AppAction.Disconnected,
                is AppAction.ProfileSelected,
                is AppAction.ProfileSelectionOpened,
                is AppAction.NewThreadOpened -> true
                else -> false
            }
    val updated = reduce(state, action, effects.cacheLimits)
    if (updated === state && !moved) return this
    return copy(
        state = updated,
        historyNavigation = historyNavigation + if (moved) 1 else 0,
        persistenceDirty = persistenceDirty || updated !== state,
    )
}

private fun AtomicRef<MobileApp>.saveState() {
    val previous = getAndUpdate { it.copy(persistenceDirty = false, saveRevision = it.saveRevision + 1) }
    previous.effects.saves.trySend(previous.saveRevision + 1 to previous.state)
}

internal suspend fun AtomicRef<MobileApp>.pair(contents: String, nowMs: Long) {
    when (val parsed = parsePairingQr(contents, nowMs)) {
        is PairingQrResult.Invalid -> dispatch(AppAction.PairingFailed(parsed.reason.name))
        is PairingQrResult.Valid ->
            value.effects.gateway
                .pair(parsed.payload)
                .fold(
                    success = { dispatch(AppAction.ProfilePaired(it)) },
                    failure = { dispatch(AppAction.PairingFailed(it)) },
                )
    }
}

internal suspend fun AtomicRef<MobileApp>.discover(profile: HostProfile) {
    value.effects.gateway
        .discover(profile)
        .fold(
            success = { dispatch(AppAction.AddressesDiscovered(profile.id, it)) },
            failure = { dispatch(AppAction.ConnectFailed(profile.id, it)) },
        )
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

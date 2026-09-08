package dev.remoteagent.mobile

/** Public connection metadata; device keys and relay tokens live in platform secure storage. */
data class HostProfile(
    val runnerId: String,
    val name: String,
    val relayUrl: String,
    val hostIdentity: String,
    val deviceIdentityReference: String,
) {
    /** The pinned host key is stable across relay route changes. */
    val id: String
        get() = hostIdentity
}

sealed interface ConnectionPhase {
    data object Disconnected : ConnectionPhase

    data object Connecting : ConnectionPhase

    data object Connected : ConnectionPhase

    data class Failed(val message: String) : ConnectionPhase
}

sealed interface LoadPhase {
    data object Idle : LoadPhase

    data object Loading : LoadPhase

    data object Ready : LoadPhase

    data class Failed(val message: String) : LoadPhase
}

data class ProfileViewState(
    val connection: ConnectionPhase = ConnectionPhase.Disconnected,
    val workingDirectoryPath: String = "",
    val threadList: LoadPhase = LoadPhase.Idle,
    val loadingMoreThreads: Boolean = false,
    val visibleProjectCount: Int = 5,
    val visibleChatCount: Int = 5,
    val projectThreadLimits: Map<String, Int> = emptyMap(),
    val threadSearchTerm: String = "",
    val moreProjectIds: Set<String> = emptySet(),
    val hasMoreChats: Boolean = false,
    val hasMoreProjects: Boolean = false,
    val selectedThreadId: String? = null,
    val unreadCompletedThreadIds: Set<String> = emptySet(),
    val newThreadCwd: String? = null,
    val threadDetail: LoadPhase = LoadPhase.Idle,
    val loadingHistory: Boolean = false,
    val interruptingTurnId: String? = null,
    val notice: String? = null,
)

data class AppState(
    val profiles: List<HostProfile> = emptyList(),
    val selectedProfileId: String? = null,
    val profileViews: Map<String, ProfileViewState> = emptyMap(),
    val cache: MobileCache = MobileCache(),
    val showingPairing: Boolean = false,
    val pairingError: String? = null,
) {
    val selectedProfile: HostProfile?
        get() = profiles.firstOrNull { it.id == selectedProfileId }

    val selectedView: ProfileViewState
        get() = selectedProfileId?.let { profileViews[it] } ?: ProfileViewState()
}

internal const val QUEUED_TURN_DELIVERY_NOTICE = "現在の処理が完了した後にメッセージを送信します。"

sealed interface AppAction {
    sealed interface Profile : AppAction

    sealed interface Connection : AppAction

    sealed interface Navigation : AppAction

    sealed interface ThreadList : AppAction

    sealed interface History : AppAction

    sealed interface Turn : AppAction

    data object PairingOpened : Profile

    data object PairingDismissed : Profile

    data object ProfileSelectionOpened : Profile

    data class ProfilePaired(val profile: HostProfile) : Profile

    data class PairingFailed(val message: String) : Profile

    data class ProfileSelected(val hostIdentity: String) : Profile

    data class AddressesDiscovered(val hostIdentity: String, val addresses: List<String>) : Profile

    data class ConnectStarted(val hostIdentity: String) : Connection

    data class ConnectSucceeded(val hostIdentity: String) : Connection

    data class ConnectFailed(val hostIdentity: String, val message: String) : Connection

    data class WorkingDirectoryChanged(val hostIdentity: String, val path: String) : Navigation

    data class ThreadListLoading(val hostIdentity: String, val append: Boolean = false) : ThreadList

    data class ThreadListLoaded(
        val hostIdentity: String,
        val threads: List<ThreadSummary>,
        val projects: List<CodexProject> = emptyList(),
        val moreProjectIds: Set<String> = emptySet(),
        val hasMoreChats: Boolean = false,
        val hasMoreProjects: Boolean = false,
    ) : ThreadList

    data class ThreadListExpanded(val hostIdentity: String, val projects: Boolean, val projectId: String? = null) :
        ThreadList

    data class ThreadListSearchChanged(val hostIdentity: String, val term: String) : ThreadList

    data class ThreadListFailed(val hostIdentity: String, val message: String) : ThreadList

    data class NewThreadOpened(val hostIdentity: String, val cwd: String) : Navigation

    data class ThreadSelected(val hostIdentity: String, val threadId: String) : Navigation

    data class ThreadListOpened(val hostIdentity: String) : Navigation

    data class ThreadReadLoading(val hostIdentity: String, val threadId: String) : History

    data class SnapshotReceived(val hostIdentity: String, val result: ThreadReadResult, val select: Boolean = true) :
        History

    data class HistoryLoading(val hostIdentity: String, val loading: Boolean, val error: String? = null) : History

    data class HistoryReceived(val hostIdentity: String, val thread: ThreadSnapshot) : History

    data class ThreadReadFailed(val hostIdentity: String, val message: String) : History

    data class ThreadStartFailed(val hostIdentity: String, val message: String) : Turn

    data class MessageAccepted(val hostIdentity: String, val threadId: String, val submission: SubmittedMessage) : Turn

    data class TurnFailed(val hostIdentity: String, val message: String) : Turn

    data class InterruptStarted(val hostIdentity: String, val turnId: String) : Turn

    data class InterruptFinished(val hostIdentity: String) : Turn

    data class HostEventReceived(val hostIdentity: String, val event: ThreadEvent) : Turn

    data class Disconnected(val hostIdentity: String) : Connection
}

fun reduce(state: AppState, action: AppAction, cacheLimits: MobileCacheLimits = MobileCacheLimits()): AppState =
    when (action) {
        is AppAction.Profile -> reduceProfile(state, action)
        is AppAction.Connection -> reduceConnection(state, action)
        is AppAction.Navigation -> reduceNavigation(state, action)
        is AppAction.ThreadList -> reduceThreadList(state, action, cacheLimits)
        is AppAction.History -> reduceHistory(state, action, cacheLimits)
        is AppAction.Turn -> reduceTurn(state, action, cacheLimits)
    }

internal fun AppState.hasProfile(hostIdentity: String): Boolean = profiles.any { it.id == hostIdentity }

internal fun AppState.isConnected(hostIdentity: String): Boolean =
    profileViews[hostIdentity]?.connection == ConnectionPhase.Connected

internal inline fun AppState.updateView(
    hostIdentity: String,
    transform: (ProfileViewState) -> ProfileViewState,
): AppState =
    if (!hasProfile(hostIdentity)) this
    else
        copy(
            profileViews = profileViews + (hostIdentity to transform(profileViews[hostIdentity] ?: ProfileViewState()))
        )

internal inline fun AppState.updateViewIfConnected(
    hostIdentity: String,
    transform: (ProfileViewState) -> ProfileViewState,
): AppState =
    updateView(hostIdentity) { view -> if (view.connection == ConnectionPhase.Connected) transform(view) else view }

internal inline fun AppState.updateProfile(hostIdentity: String, transform: (HostProfile) -> AppState): AppState =
    profiles.firstOrNull { it.id == hostIdentity }?.let(transform) ?: this

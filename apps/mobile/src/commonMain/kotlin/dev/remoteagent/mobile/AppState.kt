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
    val id: String get() = hostIdentity
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

internal const val QueuedTurnDeliveryNotice = "現在の処理が完了した後にメッセージを送信します。"

sealed interface AppAction {
    data object PairingOpened : AppAction
    data object PairingDismissed : AppAction
    data object ProfileSelectionOpened : AppAction
    data class ProfilePaired(val profile: HostProfile) : AppAction
    data class PairingFailed(val message: String) : AppAction
    data class ProfileSelected(val hostIdentity: String) : AppAction
    data class AddressesDiscovered(val hostIdentity: String, val addresses: List<String>) : AppAction
    data class ConnectStarted(val hostIdentity: String) : AppAction
    data class ConnectSucceeded(val hostIdentity: String) : AppAction
    data class ConnectFailed(val hostIdentity: String, val message: String) : AppAction
    data class WorkingDirectoryChanged(val hostIdentity: String, val path: String) : AppAction
    data class ThreadListLoading(val hostIdentity: String, val append: Boolean = false) : AppAction
    data class ThreadListLoaded(val hostIdentity: String, val threads: List<ThreadSummary>, val projects: List<CodexProject> = emptyList(), val moreProjectIds: Set<String> = emptySet(), val hasMoreChats: Boolean = false, val hasMoreProjects: Boolean = false) : AppAction
    data class ThreadListExpanded(val hostIdentity: String, val projects: Boolean, val projectId: String? = null) : AppAction
    data class ThreadListSearchChanged(val hostIdentity: String, val term: String) : AppAction
    data class ThreadListFailed(val hostIdentity: String, val message: String) : AppAction
    data class NewThreadOpened(val hostIdentity: String, val cwd: String) : AppAction
    data class ThreadSelected(val hostIdentity: String, val threadId: String) : AppAction
    data class ThreadListOpened(val hostIdentity: String) : AppAction
    data class ThreadReadLoading(val hostIdentity: String, val threadId: String) : AppAction
    data class SnapshotReceived(val hostIdentity: String, val result: ThreadReadResult, val select: Boolean = true) : AppAction
    data class HistoryLoading(val hostIdentity: String, val loading: Boolean, val error: String? = null) : AppAction
    data class HistoryReceived(val hostIdentity: String, val thread: ThreadSnapshot) : AppAction
    data class ThreadReadFailed(val hostIdentity: String, val message: String) : AppAction
    data class ThreadStartFailed(val hostIdentity: String, val message: String) : AppAction
    data class MessageAccepted(val hostIdentity: String, val threadId: String, val submission: SubmittedMessage) : AppAction
    data class TurnFailed(val hostIdentity: String, val message: String) : AppAction
    data class InterruptStarted(val hostIdentity: String, val turnId: String) : AppAction
    data class InterruptFinished(val hostIdentity: String) : AppAction
    data class HostEventReceived(val hostIdentity: String, val event: ThreadEvent) : AppAction
    data class Disconnected(val hostIdentity: String) : AppAction
}

fun reduce(
    state: AppState,
    action: AppAction,
    cacheLimits: MobileCacheLimits = MobileCacheLimits(),
): AppState = when (action) {
    AppAction.PairingOpened -> state.copy(showingPairing = true, pairingError = null)

    AppAction.PairingDismissed -> if (state.profiles.isEmpty()) state else state.copy(showingPairing = false, pairingError = null)

    AppAction.ProfileSelectionOpened -> state.copy(selectedProfileId = null, showingPairing = false)

    is AppAction.ProfilePaired -> {
        val profiles = state.profiles.filterNot { it.id == action.profile.id } + action.profile
        state.copy(
            profiles = profiles,
            selectedProfileId = action.profile.id,
            profileViews = state.profileViews + (action.profile.id to ProfileViewState()),
            showingPairing = false,
            pairingError = null,
        )
    }

    is AppAction.PairingFailed -> state.copy(pairingError = action.message)

    is AppAction.ProfileSelected -> if (state.hasProfile(action.hostIdentity)) {
        state.copy(selectedProfileId = action.hostIdentity, pairingError = null)
            .updateView(action.hostIdentity) { it.copy(loadingHistory = false) }
    } else {
        state
    }

    is AppAction.AddressesDiscovered -> state.updateProfile(action.hostIdentity) { profile ->
        state.copy(profiles = state.profiles.map {
            if (it.id == profile.id) {
                action.addresses.firstOrNull()?.let { relayUrl -> profile.copy(relayUrl = relayUrl) } ?: profile
            } else it
        })
    }

    is AppAction.ConnectStarted -> state.updateView(action.hostIdentity) {
        it.copy(connection = ConnectionPhase.Connecting, loadingHistory = false, notice = null)
    }

    is AppAction.ConnectSucceeded -> state.updateView(action.hostIdentity) {
        it.copy(
            connection = ConnectionPhase.Connected,
            threadList = LoadPhase.Idle,
            threadDetail = LoadPhase.Idle,
            notice = null,
        )
    }

    is AppAction.ConnectFailed -> state.updateView(action.hostIdentity) {
        it.copy(connection = ConnectionPhase.Failed(action.message), loadingHistory = false, notice = action.message)
    }

    is AppAction.WorkingDirectoryChanged -> state.updateViewIfConnected(action.hostIdentity) {
        it.copy(
            workingDirectoryPath = action.path,
            newThreadCwd = null,
            threadList = LoadPhase.Idle,
            selectedThreadId = null,
            threadDetail = LoadPhase.Idle,
        )
    }

    is AppAction.ThreadListLoading -> state.updateViewIfConnected(action.hostIdentity) {
        if (action.append) it.copy(loadingMoreThreads = true, notice = null)
        else it.copy(threadList = LoadPhase.Loading, loadingMoreThreads = false, notice = null)
    }

    is AppAction.ThreadListLoaded -> if (state.isConnected(action.hostIdentity)) {
        val cache = reconcileProjectList(state.cache, action.hostIdentity, action.projects, cacheLimits)
        state.copy(cache = reconcileThreadList(cache, action.hostIdentity, action.threads, cacheLimits))
            .updateView(action.hostIdentity) { it.copy(
                threadList = LoadPhase.Ready,
                loadingMoreThreads = false, moreProjectIds = action.moreProjectIds, hasMoreChats = action.hasMoreChats, hasMoreProjects = action.hasMoreProjects,
            ) }
    } else state

    is AppAction.ThreadListFailed -> state.updateViewIfConnected(action.hostIdentity) {
        it.copy(threadList = LoadPhase.Failed(action.message), loadingMoreThreads = false, notice = action.message)
    }

    is AppAction.ThreadListExpanded -> state.updateView(action.hostIdentity) {
        if (action.projectId != null) it.copy(projectThreadLimits = it.projectThreadLimits + (action.projectId to ((it.projectThreadLimits[action.projectId] ?: 5) + 10)))
        else if (action.projects) it.copy(visibleProjectCount = it.visibleProjectCount + 10)
        else it.copy(visibleChatCount = it.visibleChatCount + 10)
    }

    is AppAction.ThreadListSearchChanged -> state.updateView(action.hostIdentity) {
        it.copy(threadSearchTerm = action.term, visibleProjectCount = 5, visibleChatCount = 5, projectThreadLimits = emptyMap())
    }

    is AppAction.NewThreadOpened -> state.updateView(action.hostIdentity) {
        it.copy(loadingHistory = false, selectedThreadId = null, newThreadCwd = action.cwd.trim(), threadDetail = LoadPhase.Ready, notice = null)
    }

    is AppAction.ThreadSelected -> state.updateViewIfConnected(action.hostIdentity) {
        it.copy(loadingHistory = false, selectedThreadId = action.threadId, newThreadCwd = null, threadDetail = LoadPhase.Idle, notice = null)
    }

    is AppAction.ThreadListOpened -> state.updateView(action.hostIdentity) {
        it.copy(loadingHistory = false, selectedThreadId = null, newThreadCwd = null, threadDetail = LoadPhase.Idle, notice = null, visibleProjectCount = 5, visibleChatCount = 5, projectThreadLimits = emptyMap(), threadSearchTerm = "")
    }

    is AppAction.ThreadReadLoading -> state.updateViewIfConnected(action.hostIdentity) {
        if (it.selectedThreadId == action.threadId) it.copy(threadDetail = LoadPhase.Loading) else it
    }

    is AppAction.SnapshotReceived -> if (state.isConnected(action.hostIdentity)) {
        state.copy(cache = reconcileThreadRead(state.cache, action.hostIdentity, action.result, cacheLimits))
            .updateView(action.hostIdentity) {
                if (!action.select) it else it.copy(
                    selectedThreadId = action.result.thread.summary.id,
                    unreadCompletedThreadIds = it.unreadCompletedThreadIds - action.result.thread.summary.id,
                    newThreadCwd = null,
                    threadDetail = LoadPhase.Ready,
                    notice = null,
                )
            }
    } else state

    is AppAction.HistoryLoading -> state.updateViewIfConnected(action.hostIdentity) {
        it.copy(loadingHistory = action.loading, notice = action.error)
    }

    is AppAction.HistoryReceived -> if (state.isConnected(action.hostIdentity)) {
        val profile = state.cache.profile(action.hostIdentity)
        state.copy(cache = state.cache.copy(profiles = state.cache.profiles + (action.hostIdentity to
            profile.copy(snapshots = profile.snapshots + (action.thread.summary.id to action.thread)))))
    } else state

    is AppAction.ThreadReadFailed -> state.updateViewIfConnected(action.hostIdentity) {
        it.copy(threadDetail = LoadPhase.Failed(action.message), notice = action.message)
    }

    is AppAction.ThreadStartFailed -> state.updateViewIfConnected(action.hostIdentity) {
        // Keep the empty chat and its draft visible when creation fails.
        it.copy(notice = action.message)
    }

    is AppAction.MessageAccepted -> if (state.isConnected(action.hostIdentity)) {
        state.copy(cache = acknowledgeMessage(state.cache, action.hostIdentity, action.threadId, action.submission, cacheLimits))
            .updateView(action.hostIdentity) {
                it.copy(notice = if (action.submission.turnId == null) QueuedTurnDeliveryNotice else null)
            }
    } else state

    is AppAction.TurnFailed -> state.updateViewIfConnected(action.hostIdentity) {
        it.copy(notice = action.message)
    }

    is AppAction.InterruptStarted -> state.updateViewIfConnected(action.hostIdentity) {
        it.copy(interruptingTurnId = action.turnId, notice = null)
    }

    is AppAction.InterruptFinished -> state.updateView(action.hostIdentity) {
        it.copy(interruptingTurnId = null)
    }

    is AppAction.HostEventReceived -> if (state.isConnected(action.hostIdentity)) {
        val cache = applyLiveEvent(state.cache, action.hostIdentity, action.event, cacheLimits)
        val updated = if (cache === state.cache) state else state.copy(cache = cache)
        val event = action.event
        if (event !is ThreadEvent.TurnStarted && event !is ThreadEvent.TurnCompleted) updated
        else updated.updateViewIfConnected(action.hostIdentity) { view ->
            when {
                event is ThreadEvent.TurnStarted -> view.copy(
                    unreadCompletedThreadIds = view.unreadCompletedThreadIds - event.threadId,
                )
                event is ThreadEvent.TurnCompleted && event.status == TurnStatus.Completed -> {
                    val isViewingThread = state.selectedProfileId == action.hostIdentity &&
                        !state.showingPairing && view.selectedThreadId == event.threadId &&
                        view.threadDetail == LoadPhase.Ready
                    if (isViewingThread) view else view.copy(
                        unreadCompletedThreadIds = view.unreadCompletedThreadIds + event.threadId,
                    )
                }
                else -> view
            }
        }
    } else {
        state
    }

    is AppAction.Disconnected -> state.updateView(action.hostIdentity) {
        it.copy(
            connection = ConnectionPhase.Disconnected,
            loadingHistory = false,
            loadingMoreThreads = false,
            threadList = LoadPhase.Idle,
            threadDetail = LoadPhase.Idle,
            interruptingTurnId = null,
        )
    }
}

private fun AppState.hasProfile(hostIdentity: String): Boolean = profiles.any { it.id == hostIdentity }

private fun AppState.isConnected(hostIdentity: String): Boolean =
    profileViews[hostIdentity]?.connection == ConnectionPhase.Connected

private inline fun AppState.updateView(
    hostIdentity: String,
    transform: (ProfileViewState) -> ProfileViewState,
): AppState = if (!hasProfile(hostIdentity)) this else copy(
    profileViews = profileViews + (hostIdentity to transform(profileViews[hostIdentity] ?: ProfileViewState())),
)

private inline fun AppState.updateViewIfConnected(
    hostIdentity: String,
    transform: (ProfileViewState) -> ProfileViewState,
): AppState = updateView(hostIdentity) { view ->
    if (view.connection == ConnectionPhase.Connected) transform(view) else view
}

private inline fun AppState.updateProfile(
    hostIdentity: String,
    transform: (HostProfile) -> AppState,
): AppState = profiles.firstOrNull { it.id == hostIdentity }?.let(transform) ?: this

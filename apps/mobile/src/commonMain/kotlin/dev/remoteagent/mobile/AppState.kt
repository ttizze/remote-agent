package dev.remoteagent.mobile

/** The saved, non-secret state for one trusted PC Host. */
data class HostProfile(
    val hostIdentity: String,
    val name: String,
    val addresses: List<String>,
    val deviceIdentityReference: String,
) {
    /** A PC Host Identity, rather than an address, is the stable profile key. */
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
    val projectList: LoadPhase = LoadPhase.Idle,
    val threadList: LoadPhase = LoadPhase.Idle,
    val selectedThreadId: String? = null,
    val threadDetail: LoadPhase = LoadPhase.Idle,
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
        get() = profiles.firstOrNull { it.hostIdentity == selectedProfileId }

    val selectedView: ProfileViewState
        get() = selectedProfileId?.let { profileViews[it] } ?: ProfileViewState()

    // Kept as derived properties so the first vertical-slice callers remain simple.
    val connection: ConnectionPhase get() = selectedView.connection
    val workingDirectoryPath: String get() = selectedView.workingDirectoryPath
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
    data class ProjectListLoading(val hostIdentity: String) : AppAction
    data class ProjectListLoaded(val hostIdentity: String, val projects: List<CodexProject>) : AppAction
    data class ProjectListFailed(val hostIdentity: String, val message: String) : AppAction
    data class ThreadListLoading(val hostIdentity: String) : AppAction
    data class ThreadListLoaded(val hostIdentity: String, val threads: List<ThreadSummary>) : AppAction
    data class ThreadListFailed(val hostIdentity: String, val message: String) : AppAction
    data class ThreadSelected(val hostIdentity: String, val threadId: String) : AppAction
    data class ThreadListOpened(val hostIdentity: String) : AppAction
    data class ThreadReadLoading(val hostIdentity: String, val threadId: String) : AppAction
    data class SnapshotReceived(val hostIdentity: String, val result: ThreadReadResult) : AppAction
    data class ThreadReadFailed(val hostIdentity: String, val message: String) : AppAction
    data class ThreadStartFailed(val hostIdentity: String, val message: String) : AppAction
    data class TurnStartAcknowledged(val hostIdentity: String, val threadId: String, val turnId: String) : AppAction
    data class TurnQueued(val hostIdentity: String, val threadId: String, val queueId: String) : AppAction
    data class TurnFailed(val hostIdentity: String, val message: String) : AppAction
    data class InterruptStarted(val hostIdentity: String, val turnId: String) : AppAction
    data class InterruptFinished(val hostIdentity: String) : AppAction
    data class LiveEventReceived(val hostIdentity: String, val event: ThreadEvent) : AppAction
    data class RawMessageReceived(val hostIdentity: String, val message: RawCodexMessage) : AppAction
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
        val profiles = state.profiles.filterNot { it.hostIdentity == action.profile.hostIdentity } + action.profile
        state.copy(
            profiles = profiles,
            selectedProfileId = action.profile.hostIdentity,
            profileViews = state.profileViews + (action.profile.hostIdentity to ProfileViewState()),
            showingPairing = false,
            pairingError = null,
        )
    }

    is AppAction.PairingFailed -> state.copy(pairingError = action.message)

    is AppAction.ProfileSelected -> if (state.hasProfile(action.hostIdentity)) {
        state.copy(selectedProfileId = action.hostIdentity, pairingError = null)
    } else {
        state
    }

    is AppAction.AddressesDiscovered -> state.updateProfile(action.hostIdentity) { profile ->
        state.copy(profiles = state.profiles.map {
            if (it.hostIdentity == profile.hostIdentity) profile.copy(addresses = action.addresses.distinct()) else it
        })
    }

    is AppAction.ConnectStarted -> state.updateView(action.hostIdentity) {
        it.copy(connection = ConnectionPhase.Connecting, notice = null)
    }

    is AppAction.ConnectSucceeded -> state.updateView(action.hostIdentity) {
        it.copy(
            connection = ConnectionPhase.Connected,
            projectList = LoadPhase.Idle,
            threadList = LoadPhase.Idle,
            selectedThreadId = null,
            threadDetail = LoadPhase.Idle,
            notice = null,
        )
    }

    is AppAction.ConnectFailed -> state.updateView(action.hostIdentity) {
        it.copy(connection = ConnectionPhase.Failed(action.message), notice = action.message)
    }

    is AppAction.WorkingDirectoryChanged -> state.updateViewIfConnected(action.hostIdentity) {
        it.copy(
            workingDirectoryPath = action.path,
            threadList = LoadPhase.Idle,
            selectedThreadId = null,
            threadDetail = LoadPhase.Idle,
        )
    }

    is AppAction.ProjectListLoading -> state.updateViewIfConnected(action.hostIdentity) {
        it.copy(projectList = LoadPhase.Loading, notice = null)
    }

    is AppAction.ProjectListLoaded -> if (state.isConnected(action.hostIdentity)) {
        state.copy(cache = reconcileProjectList(state.cache, action.hostIdentity, action.projects, cacheLimits))
            .updateView(action.hostIdentity) { it.copy(projectList = LoadPhase.Ready) }
    } else state

    is AppAction.ProjectListFailed -> state.updateViewIfConnected(action.hostIdentity) {
        it.copy(projectList = LoadPhase.Failed(action.message), notice = action.message)
    }

    is AppAction.ThreadListLoading -> state.updateViewIfConnected(action.hostIdentity) {
        it.copy(threadList = LoadPhase.Loading, notice = null)
    }

    is AppAction.ThreadListLoaded -> if (state.isConnected(action.hostIdentity)) {
        state.copy(cache = reconcileThreadList(state.cache, action.hostIdentity, action.threads, cacheLimits))
            .updateView(action.hostIdentity) { it.copy(threadList = LoadPhase.Ready) }
    } else state

    is AppAction.ThreadListFailed -> state.updateViewIfConnected(action.hostIdentity) {
        it.copy(threadList = LoadPhase.Failed(action.message), notice = action.message)
    }

    is AppAction.ThreadSelected -> state.updateViewIfConnected(action.hostIdentity) {
        it.copy(selectedThreadId = action.threadId, threadDetail = LoadPhase.Idle, notice = null)
    }

    is AppAction.ThreadListOpened -> state.updateViewIfConnected(action.hostIdentity) {
        it.copy(selectedThreadId = null, threadDetail = LoadPhase.Idle, notice = null)
    }

    is AppAction.ThreadReadLoading -> state.updateViewIfConnected(action.hostIdentity) {
        if (it.selectedThreadId == action.threadId) it.copy(threadDetail = LoadPhase.Loading) else it
    }

    is AppAction.SnapshotReceived -> if (state.isConnected(action.hostIdentity)) {
        state.copy(cache = reconcileThreadRead(state.cache, action.hostIdentity, action.result, cacheLimits))
            .updateView(action.hostIdentity) {
                it.copy(
                    selectedThreadId = action.result.thread.summary.id,
                    threadDetail = LoadPhase.Ready,
                    notice = null,
                )
            }
    } else state

    is AppAction.ThreadReadFailed -> state.updateViewIfConnected(action.hostIdentity) {
        it.copy(threadDetail = LoadPhase.Failed(action.message), notice = action.message)
    }

    is AppAction.ThreadStartFailed -> state.updateViewIfConnected(action.hostIdentity) {
        // Starting a thread is a detail action. Keep the already loaded list
        // visible when it fails instead of sending the user back to cwd input.
        it.copy(notice = action.message)
    }

    is AppAction.TurnStartAcknowledged -> if (state.isConnected(action.hostIdentity)) {
        state.copy(
            cache = acknowledgeTurnStart(
                state.cache,
                action.hostIdentity,
                action.threadId,
                action.turnId,
                cacheLimits,
            ),
        ).updateView(action.hostIdentity) { it.copy(notice = null) }
    } else state

    is AppAction.TurnQueued -> state.updateViewIfConnected(action.hostIdentity) {
        it.copy(notice = QueuedTurnDeliveryNotice)
    }

    is AppAction.TurnFailed -> state.updateViewIfConnected(action.hostIdentity) {
        it.copy(notice = action.message)
    }

    is AppAction.InterruptStarted -> state.updateViewIfConnected(action.hostIdentity) {
        it.copy(interruptingTurnId = action.turnId, notice = null)
    }

    is AppAction.InterruptFinished -> state.updateView(action.hostIdentity) {
        it.copy(interruptingTurnId = null)
    }

    is AppAction.LiveEventReceived -> if (state.isConnected(action.hostIdentity)) {
        state.copy(cache = applyLiveEvent(state.cache, action.hostIdentity, action.event, cacheLimits))
    } else {
        state
    }

    is AppAction.RawMessageReceived -> if (state.hasProfile(action.hostIdentity)) {
        state.copy(cache = retainRawMessage(state.cache, action.hostIdentity, action.message, cacheLimits))
    } else {
        state
    }

    is AppAction.Disconnected -> state.updateView(action.hostIdentity) {
        it.copy(
            connection = ConnectionPhase.Disconnected,
            projectList = LoadPhase.Idle,
            threadList = LoadPhase.Idle,
            threadDetail = LoadPhase.Idle,
            interruptingTurnId = null,
        )
    }
}

private fun AppState.hasProfile(hostIdentity: String): Boolean = profiles.any { it.hostIdentity == hostIdentity }

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
): AppState = profiles.firstOrNull { it.hostIdentity == hostIdentity }?.let(transform) ?: this

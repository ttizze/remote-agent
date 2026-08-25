package dev.remoteagent.mobile

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch

enum class IosScreen {
    Pairing,
    Profiles,
    Connect,
    Connecting,
    WorkingDirectory,
    Threads,
    Thread,
}

enum class IosLoadState { Idle, Loading, Ready, Failed }

data class IosProfileView(
    val id: String,
    val name: String,
    val hostIdentity: String,
)

data class IosThreadSummaryView(
    val id: String,
    val title: String,
    val preview: String,
    val workingDirectory: String,
    val projectId: String?,
    val isActive: Boolean,
)

data class IosProjectView(
    val id: String,
    val name: String,
    val roots: List<String>,
)

class IosItemView internal constructor(
    val id: String,
    val kind: String,
    val title: String,
    val collapsedBody: String,
    val isCollapsible: Boolean,
    val contentVersion: String,
    private val expandedBodyProvider: () -> String,
) {
    fun expandedBody(): String = expandedBodyProvider()
}

data class IosTurnView(
    val id: String,
    val status: String,
    val isInProgress: Boolean,
    val userMessages: List<IosItemView>,
    val activitySummary: String?,
    val activityItems: List<IosItemView>,
    val responses: List<IosItemView>,
    val activityInitiallyExpanded: Boolean,
    val activityCanCollapse: Boolean,
    val error: IosTurnErrorView?,
    val pendingRequests: List<IosTurnRequestView>,
)

data class IosTurnErrorView(
    val title: String,
    val message: String,
    val details: String?,
    val isReconnecting: Boolean,
    val isRetryable: Boolean,
)

data class IosTurnRequestView(
    val id: String,
    val kind: String,
    val title: String,
    val body: String,
)

data class IosThreadView(
    val id: String,
    val title: String,
    val turns: List<IosTurnView>,
)

data class IosAppViewState(
    val screen: IosScreen,
    val profiles: List<IosProfileView>,
    val selectedProfileId: String?,
    val selectedProfileName: String?,
    val addresses: List<String>,
    val pairingError: String?,
    val connectionError: String?,
    val workingDirectory: String,
    val projectLoadState: IosLoadState,
    val projectLoadError: String?,
    val projects: List<IosProjectView>,
    val threadLoadState: IosLoadState,
    val threadLoadError: String?,
    val threads: List<IosThreadSummaryView>,
    val selectedThread: IosThreadView?,
    val notice: String?,
    val interruptingTurnId: String?,
)

/**
 * The single interface SwiftUI needs from the shared application module.
 * Calls are intent-level and state is published as immutable view snapshots.
 */
class IosAppController {
    private val dependencies = IosMobileDependencies()
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)
    private val controller = MobileController(dependencies.gateway, dependencies.repository)
    private var observation: HostEventSubscription? = null

    init {
        // Reconnection must go through the application controller so the
        // shared state receives the fresh task list and selected-task
        // Snapshot.  Calling the gateway directly would leave SwiftUI
        // displaying the pre-background cache indefinitely.
        IosLifecycleBridge.onRestoreAfterForeground = { restoreAfterForeground() }
    }

    fun currentState(): IosAppViewState = controller.state.toIosViewState()

    fun observe(observer: (IosAppViewState) -> Unit) {
        observation?.cancel()
        observation = controller.observe { observer(it.toIosViewState()) }
    }

    fun close() {
        observation?.cancel()
        observation = null
        IosLifecycleBridge.onRestoreAfterForeground = null
        scope.cancel()
    }

    fun openPairing() = controller.dispatch(AppAction.PairingOpened)
    fun dismissPairing() = controller.dispatch(AppAction.PairingDismissed)
    fun showProfiles() = controller.dispatch(AppAction.ProfileSelectionOpened)
    fun selectProfile(hostIdentity: String) = controller.dispatch(AppAction.ProfileSelected(hostIdentity))

    fun pair(contents: String, nowMs: Long) {
        scope.launch { controller.pair(contents, nowMs) }
    }

    fun discover() = withSelectedProfile { profile -> controller.discover(profile) }

    fun connect() = withSelectedProfile { profile -> controller.connect(profile, scope) }

    fun restoreAfterForeground() = connect()

    fun updateWorkingDirectory(path: String) {
        val profile = controller.state.selectedProfile ?: return
        controller.dispatch(AppAction.WorkingDirectoryChanged(profile.hostIdentity, path))
    }

    fun refreshTaskList() = withSelectedProfile { profile ->
        controller.listProjects(profile)
        controller.listThreads(profile)
    }

    fun startTask(cwd: String, firstPrompt: String) = withSelectedProfile { profile ->
        controller.startThread(profile, cwd, firstPrompt)
    }

    fun openThread(threadId: String) = withSelectedProfile { profile -> controller.readThread(profile, threadId) }

    fun showThreadList() {
        val profile = controller.state.selectedProfile ?: return
        controller.dispatch(AppAction.ThreadListOpened(profile.hostIdentity))
    }

    fun sendTurn(text: String) {
        val profile = controller.state.selectedProfile ?: return
        val threadId = controller.state.selectedView.selectedThreadId ?: return
        scope.launch { controller.startTurn(profile, threadId, text) }
    }

    fun interrupt(turnId: String) {
        val profile = controller.state.selectedProfile ?: return
        val threadId = controller.state.selectedView.selectedThreadId ?: return
        scope.launch { controller.interrupt(profile, threadId, turnId) }
    }

    private fun withSelectedProfile(block: suspend (HostProfile) -> Unit) {
        val profile = controller.state.selectedProfile ?: return
        scope.launch { block(profile) }
    }
}

private fun AppState.toIosViewState(): IosAppViewState {
    val profile = selectedProfile
    val view = selectedView
    val profileCache = profile?.let { cache.profile(it.hostIdentity) }
    val snapshot = view.selectedThreadId?.let { profileCache?.snapshots?.get(it) }
    val screen = when {
        showingPairing || profiles.isEmpty() -> IosScreen.Pairing
        profile == null -> IosScreen.Profiles
        view.connection == ConnectionPhase.Connecting -> IosScreen.Connecting
        view.connection is ConnectionPhase.Disconnected || view.connection is ConnectionPhase.Failed -> IosScreen.Connect
        view.selectedThreadId == null -> IosScreen.Threads
        else -> IosScreen.Thread
    }
    return IosAppViewState(
        screen = screen,
        profiles = profiles.map { IosProfileView(it.id, it.name, it.hostIdentity) },
        selectedProfileId = selectedProfileId,
        selectedProfileName = profile?.name,
        addresses = profile?.addresses.orEmpty(),
        pairingError = pairingError,
        connectionError = (view.connection as? ConnectionPhase.Failed)?.message,
        workingDirectory = view.workingDirectoryPath,
        projectLoadState = view.projectList.toIosLoadState(),
        projectLoadError = (view.projectList as? LoadPhase.Failed)?.message,
        projects = profileCache?.projects.orEmpty().map { project ->
            IosProjectView(project.id, project.name, project.roots.map(WorkingDirectory::path))
        },
        threadLoadState = view.threadList.toIosLoadState(),
        threadLoadError = (view.threadList as? LoadPhase.Failed)?.message,
        threads = profileCache?.threadList.orEmpty().map { summary ->
            IosThreadSummaryView(
                id = summary.id,
                title = summary.name ?: summary.preview.ifBlank { "無題のタスク" },
                preview = summary.preview,
                workingDirectory = summary.workingDirectory.path,
                projectId = summary.projectId,
                isActive = summary.status is ThreadStatus.Active,
            )
        },
        selectedThread = snapshot?.toIosThreadView(),
        notice = view.notice,
        interruptingTurnId = view.interruptingTurnId,
    )
}

private fun LoadPhase.toIosLoadState(): IosLoadState = when (this) {
    LoadPhase.Idle -> IosLoadState.Idle
    LoadPhase.Loading -> IosLoadState.Loading
    LoadPhase.Ready -> IosLoadState.Ready
    is LoadPhase.Failed -> IosLoadState.Failed
}

private fun ThreadSnapshot.toIosThreadView(): IosThreadView = IosThreadView(
    id = summary.id,
    title = summary.name ?: summary.preview.ifBlank { "タスク" },
    turns = turns.map { turn ->
        val presentation = turn.toThreadTurnPresentation()
        IosTurnView(
            id = turn.id,
            status = turn.status.name,
            isInProgress = turn.status == TurnStatus.InProgress,
            userMessages = presentation.userMessages.map(CodexItem::toIosItemView),
            activitySummary = presentation.activitySummary,
            activityItems = presentation.activityItems.map(CodexItem::toIosItemView),
            responses = presentation.responses.map(CodexItem::toIosItemView),
            activityInitiallyExpanded = presentation.activityInitiallyExpanded,
            activityCanCollapse = presentation.activityCanCollapse,
            error = presentation.error?.let { error ->
                IosTurnErrorView(
                    error.title,
                    error.message,
                    error.details,
                    error.isReconnecting,
                    error.isRetryable,
                )
            },
            pendingRequests = presentation.pendingRequests.map { request ->
                IosTurnRequestView(request.id, request.kind, request.title, request.body)
            },
        )
    },
)

private fun CodexItem.toIosItemView(): IosItemView = toThreadItemPresentation().let { presentation ->
    IosItemView(
        id = presentation.id,
        kind = presentation.kind,
        title = presentation.title,
        collapsedBody = presentation.collapsedBody,
        isCollapsible = presentation.isCollapsible,
        contentVersion = threadItemContentVersion(),
        expandedBodyProvider = { expandedThreadItemBody() },
    )
}

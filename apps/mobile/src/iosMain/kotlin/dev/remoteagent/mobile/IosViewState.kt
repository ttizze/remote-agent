package dev.remoteagent.mobile

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonPrimitive

enum class IosScreen {
    Pairing,
    Profiles,
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
    val isDeferred: Boolean,
    val imageSources: List<String>,
    private val expandedBodyProvider: () -> String,
) {
    fun expandedBody(): String = expandedBodyProvider()
}

data class IosTurnView(
    val id: String,
    val turnId: String,
    val hasOlderItems: Boolean,
    val openingUserMessage: IosItemView?,
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
    val requestIdJson: String,
    val method: String,
    val paramsJson: String,
    val kind: String,
    val title: String,
    val body: String,
)

data class IosThreadView(
    val id: String,
    val title: String,
    val turns: List<IosTurnView>,
    val queuedMessages: List<IosItemView>,
    val hasOlderTurns: Boolean,
)

data class IosAppViewState(
    val screen: IosScreen,
    val isConnected: Boolean,
    val isConnecting: Boolean,
    val profiles: List<IosProfileView>,
    val selectedProfileId: String?,
    val selectedProfileName: String?,
    val pairingError: String?,
    val connectionError: String?,
    val workingDirectory: String,
    val projects: List<IosProjectView>,
    val threadLoadState: IosLoadState,
    val threadLoadError: String?,
    val threads: List<IosThreadSummaryView>,
    val hasMoreProjects: Boolean,
    val visibleProjectCount: Int,
    val loadingMoreThreads: Boolean,
    val loadingHistory: Boolean,
    val moreProjectIds: Set<String>,
    val hasMoreChats: Boolean,
    val selectedThreadId: String?,
    val isNewThread: Boolean,
    val notice: String?,
    val interruptingTurnId: String?,
)

/** One projector per controller. Retain only the currently selected conversation. */
internal class IosViewStateProjector {
    private var hostId: String? = null
    private var snapshot: ThreadSnapshot? = null
    var thread: IosThreadView? = null
        private set
    private var turns = emptyList<ProjectedTurn>()
    private var appSource: AppState? = null
    private var app: IosAppViewState? = null

    fun project(state: AppState): IosAppViewState {
        val selected = state.selectedProfile?.let { host ->
            state.selectedView.selectedThreadId?.let { state.cache.snapshot(host.id, it) }
        }
        if (hostId != state.selectedProfileId || snapshot?.summary?.id != selected?.summary?.id) {
            snapshot = null
            thread = null
            turns = emptyList()
        }
        hostId = state.selectedProfileId
        if (selected !== snapshot) {
            thread = selected?.let(::projectThread)
            if (selected == null) turns = emptyList()
            snapshot = selected
        }
        val previous = appSource
        val previousApp = app
        val view = state.selectedView
        val profile = state.selectedProfileId?.let { state.cache.profile(it) }
        val previousProfile = previous?.let { source -> source.selectedProfileId?.let { source.cache.profile(it) } }
        val directory = view.newThreadCwd ?: selected?.summary?.workingDirectory?.path ?: view.workingDirectoryPath
        appSource = state
        // Body/raw-message changes must not rebuild or publish navigation and title lists.
        if (previous != null && previousApp != null &&
            state.profiles === previous.profiles && state.selectedProfileId == previous.selectedProfileId &&
            state.showingPairing == previous.showingPairing && state.pairingError == previous.pairingError &&
            view == previous.selectedView && directory == previousApp.workingDirectory &&
            profile?.projects === previousProfile?.projects && profile?.threadList === previousProfile?.threadList
        ) return previousApp
        val updated = state.toIosViewState()
        val published = if (previousApp != null && updated == previousApp) previousApp else updated
        app = published
        return published
    }

    private fun projectThread(source: ThreadSnapshot): IosThreadView {
        val pending = source.submittedMessages.groupBy { it.turnId }
        val updated = source.turns.mapIndexed { index, turn ->
            val submissions = pending[turn.id].orEmpty()
            val previous = turns.getOrNull(index)
            // Immutable turns keep their identity through unrelated live updates.
            // Accepted inputs are a separate input until their native echo arrives.
            if (previous != null && previous.source === turn && previous.submissions == submissions) previous
                else ProjectedTurn(turn, submissions, turn.toIosTurnViews(submissions, source.summary))
        }
        turns = updated
        return IosThreadView(
            id = source.summary.id,
            title = source.summary.name ?: source.summary.preview.ifBlank { "タスク" },
            turns = updated.flatMap { it.views },
            queuedMessages = pending[null].orEmpty().map {
                CodexItem.UserMessage(it.clientId, it.text, it.clientId, it.imageSources).toIosItemView()
            },
            hasOlderTurns = source.olderTurnsCursor != null,
        )
    }

    private class ProjectedTurn(
        val source: CodexTurn,
        val submissions: List<SubmittedMessage>,
        val views: List<IosTurnView>,
    )
}

private fun AppState.toIosViewState(): IosAppViewState {
    val profile = selectedProfile
    val view = selectedView
    val profileCache = profile?.let { cache.profile(it.id) }
    val snapshot = view.selectedThreadId?.let { profileCache?.snapshots?.get(it) }
    val screen = when {
        showingPairing || profiles.isEmpty() -> IosScreen.Pairing
        profile == null -> IosScreen.Profiles
        view.selectedThreadId == null && view.newThreadCwd == null -> IosScreen.Threads
        else -> IosScreen.Thread
    }
    return IosAppViewState(
        screen = screen,
        isConnected = view.connection == ConnectionPhase.Connected,
        isConnecting = view.connection == ConnectionPhase.Connecting,
        profiles = profiles.map { IosProfileView(it.id, it.name, it.id) },
        selectedProfileId = selectedProfileId,
        selectedProfileName = profile?.name,
        pairingError = pairingError,
        connectionError = (view.connection as? ConnectionPhase.Failed)?.message,
        workingDirectory = view.newThreadCwd ?: snapshot?.summary?.workingDirectory?.path ?: view.workingDirectoryPath,
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
        hasMoreProjects = view.hasMoreProjects,
        visibleProjectCount = view.visibleProjectCount,
        loadingMoreThreads = view.loadingMoreThreads,
        loadingHistory = view.loadingHistory,
        moreProjectIds = view.moreProjectIds,
        hasMoreChats = view.hasMoreChats,
        selectedThreadId = view.selectedThreadId,
        isNewThread = view.newThreadCwd != null,
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

private fun CodexTurn.toIosTurnViews(submissions: List<SubmittedMessage>, summary: ThreadSummary): List<IosTurnView> {
    val deferredIds = (raw?.get("deferredItemIds") as? JsonArray).orEmpty()
        .mapNotNull { (it as? JsonPrimitive)?.content }.toSet()
    val segments = if (submissions.isEmpty()) toThreadTurnPresentations() else
        ThreadSnapshot(summary, listOf(this), submittedMessages = submissions).conversationSegments()
    return segments.map { presentation ->
        IosTurnView(
            id = presentation.id,
            turnId = this.id,
            hasOlderItems = presentation.id == this.id && this.hasOlderItems,
            openingUserMessage = if (presentation.id == this.id) this.raw?.get("openingUserMessage")
                ?.let(::codexItem)?.takeUnless { item -> this.items.any { it.id == item.id } }?.toIosItemView() else null,
            status = this.status.name,
            isInProgress = presentation.isLastSegment && this.status == TurnStatus.InProgress,
            userMessages = presentation.userMessages.map { it.toIosItemView() },
            activitySummary = presentation.activitySummary,
            activityItems = presentation.activityItems.map { it.toIosItemView(it.id in deferredIds) },
            responses = presentation.responses.map { it.toIosItemView() },
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
                this.pendingRequests.first { it.id == request.id }.let { raw ->
                    IosTurnRequestView(request.id, raw.wireId.toString(), raw.method, raw.params.toString(), request.kind, request.title, request.body)
                }
            },
        )
    }
}

private fun CodexItem.toIosItemView(isDeferred: Boolean = false): IosItemView = toThreadItemPresentation().let { presentation ->
    IosItemView(
        id = (this as? CodexItem.UserMessage)?.clientId ?: presentation.id,
        kind = if (this is CodexItem.AgentMessage && phase == AgentMessagePhase.Commentary) "commentary" else presentation.kind,
        title = presentation.title,
        collapsedBody = presentation.collapsedBody,
        isCollapsible = presentation.isCollapsible,
        contentVersion = "${threadItemContentVersion()}:$isDeferred",
        isDeferred = isDeferred,
        imageSources = (this as? CodexItem.UserMessage)?.imageSources.orEmpty(),
        expandedBodyProvider = { expandedThreadItemBody() },
    )
}

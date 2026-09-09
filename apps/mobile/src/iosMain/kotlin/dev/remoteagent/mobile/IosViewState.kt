package dev.remoteagent.mobile

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull

enum class IosScreen {
    Pairing,
    Profiles,
    Threads,
    Thread,
}

enum class IosLoadState {
    Idle,
    Loading,
    Ready,
    Failed,
}

data class IosProfileView(val id: String, val name: String, val hostIdentity: String)

data class IosThreadSummaryView(
    val id: String,
    val title: String,
    val preview: String,
    val workingDirectory: String,
    val projectId: String?,
    val isActive: Boolean,
    val hasUnreadCompletion: Boolean,
)

data class IosProjectView(val id: String, val name: String, val roots: List<String>)

class IosItemView internal constructor(internal val source: CodexItem, val isDeferred: Boolean = false) {
    val id: String
    val kind: String
    val title: String
    val collapsedBody: String
    val isCollapsible: Boolean
    val contentVersion: String
    val imageSources: List<String>

    init {
        val presentation = source.toThreadItemPresentation()
        id = (source as? CodexItem.UserMessage)?.clientId ?: presentation.id
        kind = presentation.kind
        title = presentation.title
        collapsedBody = presentation.collapsedBody
        isCollapsible = presentation.isCollapsible
        contentVersion = "${source.threadItemContentVersion()}:$isDeferred"
        imageSources = source.iosImageSources()
    }

    fun expandedBody(): String = source.expandedThreadItemBody()
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
    val requestJson: String,
    val paramsJson: String,
    val form: String,
    val title: String,
    val body: String,
    val questions: List<RequestQuestion>,
    val decisions: List<String>,
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

/** One immutable projection value per UI owner; unchanged turns and items retain identity. */
internal data class IosViewProjection(
    val source: AppState,
    val snapshot: ThreadSnapshot?,
    val thread: IosThreadView?,
    val turns: List<ProjectedTurn>,
    val app: IosAppViewState,
)

internal fun projectIosView(state: AppState, previous: IosViewProjection? = null): IosViewProjection {
    if (previous?.source === state) return previous
    val selected =
        state.selectedProfile?.let { host ->
            state.selectedView.selectedThreadId?.let { state.cache.snapshot(host.id, it) }
        }
    val sameThread =
        previous?.source?.selectedProfileId == state.selectedProfileId &&
            previous?.snapshot?.summary?.id == selected?.summary?.id
    val unchanged = sameThread && previous?.snapshot === selected
    val turns =
        if (unchanged) previous?.turns.orEmpty()
        else selected?.let { projectTurns(it, if (sameThread) previous?.turns.orEmpty() else emptyList()) }.orEmpty()
    val thread = if (unchanged) previous?.thread else selected?.projectThread(turns)
    val view = state.selectedView
    val directory = view.newThreadCwd ?: selected?.summary?.workingDirectory?.path ?: view.workingDirectoryPath
    val app = state.projectApp(previous, directory)
    return IosViewProjection(state, selected, thread, turns, app)
}

private fun ThreadSnapshot.projectThread(turns: List<ProjectedTurn>): IosThreadView =
    IosThreadView(
        id = summary.id,
        title = summary.name ?: summary.preview.ifBlank { "タスク" },
        turns = turns.flatMap { it.views },
        queuedMessages =
            submittedMessages
                .filter { it.turnId == null }
                .map { IosItemView(CodexItem.UserMessage(it.clientId, it.text, it.clientId, it.imageSources)) },
        hasOlderTurns = olderTurnsCursor != null,
    )

private fun AppState.projectApp(previous: IosViewProjection?, directory: String): IosAppViewState =
    previous
        ?.takeIf {
            sameNavigationAs(it.source) && sameProfileListsAs(it.source) && directory == it.app.workingDirectory
        }
        ?.app ?: toIosViewState().let { updated -> previous?.app?.takeIf { it == updated } ?: updated }

internal data class ProjectedTurn(
    val source: CodexTurn,
    val submissions: List<SubmittedMessage>,
    val views: List<IosTurnView>,
    val items: Map<String, IosItemView>,
)

private fun projectTurns(source: ThreadSnapshot, previousTurns: List<ProjectedTurn>): List<ProjectedTurn> {
    val pending = source.submittedMessages.groupBy { it.turnId }
    return source.turns.mapIndexed { index, turn ->
        val submissions = pending[turn.id].orEmpty()
        val previous = previousTurns.getOrNull(index)?.takeIf { it.source.id == turn.id }
        if (previous != null && previous.source === turn && previous.submissions == submissions) previous
        else {
            // Build only the current membership. Old projections remain immutable,
            // removed items are released, and unchanged item bodies are never copied.
            val items = mutableMapOf<String, IosItemView>()
            val views =
                turn.toIosTurnViews(submissions) { item, deferred ->
                    val cached = previous?.items?.get(item.id)
                    val view =
                        cached?.takeIf { it.source === item && it.isDeferred == deferred }
                            ?: IosItemView(item, deferred)
                    items[item.id] = view
                    view
                }
            ProjectedTurn(turn, submissions, views, items)
        }
    }
}

private fun AppState.toIosViewState(): IosAppViewState {
    val profile = selectedProfile
    val view = selectedView
    val profileCache = profile?.let { cache.profile(it.id) }
    val snapshot = view.selectedThreadId?.let { profileCache?.snapshots?.get(it) }
    val screen =
        when {
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
        projects =
            profileCache?.projects.orEmpty().map { project ->
                IosProjectView(project.id, project.name, project.roots.map(WorkingDirectory::path))
            },
        threadLoadState = view.threadList.toIosLoadState(),
        threadLoadError = (view.threadList as? LoadPhase.Failed)?.message,
        threads =
            profileCache?.threadList.orEmpty().map { summary ->
                IosThreadSummaryView(
                    id = summary.id,
                    title = summary.name ?: summary.preview.ifBlank { "無題のタスク" },
                    preview = summary.preview,
                    workingDirectory = summary.workingDirectory.path,
                    projectId = summary.projectId,
                    isActive = summary.status is ThreadStatus.Active,
                    hasUnreadCompletion = summary.id in view.unreadCompletedThreadIds,
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

private fun LoadPhase.toIosLoadState(): IosLoadState =
    when (this) {
        LoadPhase.Idle -> IosLoadState.Idle
        LoadPhase.Loading -> IosLoadState.Loading
        LoadPhase.Ready -> IosLoadState.Ready
        is LoadPhase.Failed -> IosLoadState.Failed
    }

private inline fun CodexTurn.toIosTurnViews(
    submissions: List<SubmittedMessage>,
    itemView: (CodexItem, Boolean) -> IosItemView,
): List<IosTurnView> {
    val deferredIds =
        (raw?.get("deferredItemIds") as? JsonArray).orEmpty().mapNotNull { (it as? JsonPrimitive)?.content }.toSet()
    val segments = toThreadTurnPresentations(submissions)
    return segments.map { presentation ->
        IosTurnView(
            id = presentation.id,
            turnId = this.id,
            hasOlderItems = presentation.id == this.id && this.hasOlderItems,
            openingUserMessage =
                if (presentation.id == this.id)
                    this.raw
                        ?.get("openingUserMessage")
                        ?.let(::codexItem)
                        ?.takeUnless { item -> this.items.any { it.id == item.id } }
                        ?.let(::IosItemView)
                else null,
            status = this.status.name,
            isInProgress = presentation.isLastSegment && this.status == TurnStatus.InProgress,
            userMessages = presentation.userMessages.map { itemView(it, false) },
            activitySummary = presentation.activitySummary,
            activityItems = presentation.activityItems.map { itemView(it, it.id in deferredIds) },
            responses = presentation.responses.map { itemView(it, false) },
            activityInitiallyExpanded = presentation.activityInitiallyExpanded,
            activityCanCollapse = presentation.activityCanCollapse,
            error =
                presentation.error?.let { error ->
                    IosTurnErrorView(error.title, error.message, error.details, error.isReconnecting, error.isRetryable)
                },
            pendingRequests =
                presentation.pendingRequests.map { request ->
                    this.pendingRequests
                        .first { it.id == request.id }
                        .let { raw ->
                            IosTurnRequestView(
                                request.id,
                                kotlinx.serialization.json
                                    .buildJsonObject {
                                        put("id", raw.wireId)
                                        put("method", kotlinx.serialization.json.JsonPrimitive(raw.method))
                                        put("params", raw.params)
                                    }
                                    .toString(),
                                raw.params.toString(),
                                request.form,
                                request.title,
                                request.body,
                                request.questions,
                                request.decisions,
                            )
                        }
                },
        )
    }
}

private fun CodexItem.iosImageSources(): List<String> =
    when (this) {
        is CodexItem.UserMessage -> imageSources
        is CodexItem.Unknown ->
            if (codexType == "imageGeneration") {
                val path = (raw["savedPath"] as? JsonPrimitive)?.contentOrNull?.takeIf(String::isNotBlank)
                val result = (raw["result"] as? JsonPrimitive)?.contentOrNull?.takeIf(String::isNotBlank)
                listOfNotNull(path ?: result?.let { "data:image/png;base64,$it" })
            } else emptyList()
        else -> emptyList()
    }

private fun AppState.sameNavigationAs(previous: AppState): Boolean {
    if (profiles !== previous.profiles || selectedProfileId != previous.selectedProfileId) return false
    return showingPairing == previous.showingPairing &&
        pairingError == previous.pairingError &&
        selectedView == previous.selectedView
}

private fun AppState.sameProfileListsAs(previous: AppState): Boolean {
    val profile = selectedProfileId?.let { cache.profile(it) }
    val previousProfile = previous.selectedProfileId?.let { previous.cache.profile(it) }
    return profile?.projects === previousProfile?.projects && profile?.threadList === previousProfile?.threadList
}

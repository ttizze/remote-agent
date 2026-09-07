package dev.remoteagent.mobile

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

enum class IosScreen {
    Pairing,
    Profiles,
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
    val selectedThread: IosThreadView?,
    val isNewThread: Boolean,
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
    private val controller = MobileController(dependencies.gateway, dependencies.repository, deferHistoryItemDetails = true)
    private var observation: HostEventSubscription? = null
    private var connectionObservation: HostEventSubscription? = null

    init {
        IosLifecycleBridge.onRestoreAfterForeground = { restoreAfterForeground() }
        controller.openApp(scope)
        connectionObservation = controller.maintainConnection(scope)
    }

    fun currentState(): IosAppViewState = controller.state.toIosViewState()

    fun observe(observer: (IosAppViewState) -> Unit) {
        observation?.cancel()
        observation = controller.observe { observer(it.toIosViewState()) }
    }

    fun close() {
        observation?.cancel()
        observation = null
        connectionObservation?.cancel()
        connectionObservation = null
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

    fun connect() = controller.restoreConnection(scope)

    fun listModels(completion: (List<CodexModel>?, String?) -> Unit) {
        val profile = controller.state.selectedProfile ?: run { completion(null, "接続先が選択されていません"); return }
        scope.launch {
            when (val result = dependencies.gateway.listModels(profile)) {
                is GatewayResult.Success -> completion(result.value, null)
                is GatewayResult.Failure -> completion(null, result.message)
            }
        }
    }

    fun setTurnOptions(hostIdentity: String, model: String?, effort: String?) {
        dependencies.gateway.setTurnOptions(hostIdentity, CodexTurnOptions(model, effort))
    }

    fun restoreAfterForeground() = controller.openApp(scope)

    fun updateWorkingDirectory(path: String) {
        val profile = controller.state.selectedProfile ?: return
        controller.dispatch(AppAction.WorkingDirectoryChanged(profile.id, path))
    }

    fun refreshTaskList() = withSelectedProfile { profile ->
        controller.listThreads(profile)
    }

    fun expandTaskList(projects: Boolean, projectId: String?) = withSelectedProfile { profile ->
        controller.expandTaskList(profile, projects, projectId)
    }

    fun searchTaskList(term: String) = withSelectedProfile { profile -> controller.searchTaskList(profile, term) }

    fun openNewThread(cwd: String) {
        controller.state.selectedProfile?.let { controller.openNewThread(it, cwd) }
    }

    fun openThread(threadId: String) = withSelectedProfile { profile -> controller.readThread(profile, threadId) }

    fun loadOlderHistory(turnId: String?) = withSelectedProfile { profile -> controller.loadOlderHistory(profile, turnId) }

    fun showThreadList() = withSelectedProfile { profile -> controller.showThreadList(profile) }

    fun sendTurn(text: String, attachments: List<CodexAttachment>, completion: (Boolean, String?) -> Unit) {
        val profile = controller.state.selectedProfile ?: run { completion(false, null); return }
        scope.launch {
            val result = controller.sendMessage(profile, text, attachments)
            completion(result.accepted, result.threadId)
        }
    }

    fun respond(requestIdJson: String, responseJson: String, completion: (String?) -> Unit) {
        val profile = controller.state.selectedProfile ?: run { completion("接続先が選択されていません"); return }
        scope.launch {
            try {
                when (val result = controller.respond(profile, Json.parseToJsonElement(requestIdJson), Json.parseToJsonElement(responseJson))) {
                    is GatewayResult.Success -> completion(null)
                    is GatewayResult.Failure -> completion(result.message)
                }
            } catch (failure: IllegalArgumentException) { completion(failure.message ?: "JSONが無効です") }
        }
    }

    fun readItemDetails(threadId: String, turnId: String, itemId: String, completion: (String?, String?) -> Unit) {
        val profile = controller.state.selectedProfile ?: run { completion(null, "接続先が選択されていません"); return }
        scope.launch {
            when (val result = dependencies.gateway.readItemDetails(profile, threadId, turnId, itemId)) {
                is GatewayResult.Success -> completion(result.value, null)
                is GatewayResult.Failure -> completion(null, result.message)
            }
        }
    }

    fun transcribeAudio(audio: String, completion: (String?, String?) -> Unit) {
        val profile = controller.state.selectedProfile ?: run { completion(null, "接続先が選択されていません"); return }
        scope.launch {
            val result = dependencies.gateway.rawRequest(profile, "host/dictation/transcribe", buildJsonObject {
                put("audio", audio)
            })
            when (result) {
                is GatewayResult.Success -> {
                    val text = (result.value as? JsonObject)?.get("text") as? JsonPrimitive
                    if (text?.isString == true && text.content.isNotBlank()) completion(text.content, null)
                    else completion(null, "文字起こしの応答が無効です。")
                }
                is GatewayResult.Failure -> completion(null, result.message)
            }
        }
    }

    /** Files and reviews use the same authenticated RPC connection as the task. */
    fun workspaceRequest(method: String, paramsJson: String, completion: (String?, String?) -> Unit) {
        val profile = controller.state.selectedProfile ?: run { completion(null, "接続先が選択されていません"); return }
        if (method !in setOf("host/file/list", "host/file/read", "host/file/write", "host/workspace/review")) {
            completion(null, "未対応のファイル操作です"); return
        }
        scope.launch {
            try {
                completeJson(dependencies.gateway.rawRequest(profile, method, Json.parseToJsonElement(paramsJson)), completion)
            } catch (failure: IllegalArgumentException) { completion(null, failure.message ?: "JSONが無効です") }
        }
    }

    fun transfer(paramsJson: String, completion: (String?, String?) -> Unit) {
        val profile = controller.state.selectedProfile ?: run { completion(null, "接続先が選択されていません"); return }
        scope.launch {
            try {
                completeJson(dependencies.gateway.transfer(profile, Json.parseToJsonElement(paramsJson)), completion)
            } catch (failure: IllegalArgumentException) { completion(null, failure.message ?: "転送に失敗しました") }
        }
    }

    private fun completeJson(result: GatewayResult<JsonElement>, completion: (String?, String?) -> Unit) = when (result) {
        is GatewayResult.Success -> completion(result.value.toString(), null)
        is GatewayResult.Failure -> completion(null, result.message)
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

internal fun AppState.toIosViewState(): IosAppViewState {
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
        selectedThread = snapshot?.toIosThreadView(),
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

private fun ThreadSnapshot.toIosThreadView(): IosThreadView = IosThreadView(
    id = summary.id,
    hasOlderTurns = olderTurnsCursor != null,
    title = summary.name ?: summary.preview.ifBlank { "タスク" },
    queuedMessages = submittedMessages.filter { it.turnId == null }.map { CodexItem.UserMessage(it.clientId, it.text, it.clientId, it.imageSources).toIosItemView() },
    turns = conversationSegments().map { presentation ->
        val turn = turns.first { it.id == presentation.turnId }
        val deferredIds = (turn.raw?.get("deferredItemIds") as? JsonArray).orEmpty()
            .mapNotNull { (it as? JsonPrimitive)?.content }.toSet()
        IosTurnView(
            id = presentation.id,
            turnId = turn.id,
            hasOlderItems = presentation.id == turn.id && turn.hasOlderItems,
            openingUserMessage = if (presentation.id == turn.id) turn.raw?.get("openingUserMessage")
                ?.let(::codexItem)?.takeUnless { item -> turn.items.any { it.id == item.id } }?.toIosItemView() else null,
            status = turn.status.name,
            isInProgress = presentation.isLastSegment && turn.status == TurnStatus.InProgress,
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
                turn.pendingRequests.first { it.id == request.id }.let { raw ->
                    IosTurnRequestView(request.id, raw.wireId.toString(), raw.method, raw.params.toString(), request.kind, request.title, request.body)
                }
            },
        )
    },
)

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

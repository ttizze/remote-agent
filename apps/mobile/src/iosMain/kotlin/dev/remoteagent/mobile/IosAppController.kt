package dev.remoteagent.mobile

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/**
 * The single interface SwiftUI needs from the shared application module.
 * Calls are intent-level and state is published as immutable view snapshots.
 */
class IosAppController {
    private val dependencies = IosMobileDependencies()
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)
    private val controller = MobileController(dependencies.gateway, dependencies.repository, scope, deferHistoryItemDetails = true)
    private val viewProjector = IosViewStateProjector()
    private var observation: HostEventSubscription? = null
    private var connectionObservation: HostEventSubscription? = null
    private var rendering: Job? = null

    init {
        IosLifecycleBridge.onRestoreAfterForeground = { restoreAfterForeground() }
        IosLifecycleBridge.onPersistBeforeBackground = { completion ->
            scope.launch {
                try { controller.flushPersistence() } finally { completion() }
            }
        }
        controller.openApp(scope)
        connectionObservation = controller.maintainConnection(scope)
    }

    fun currentState(): IosAppViewState = viewProjector.project(controller.state)

    fun currentThread(): IosThreadView? {
        viewProjector.project(controller.state)
        return viewProjector.thread
    }

    fun observe(observer: (IosAppViewState, IosThreadView?) -> Unit) {
        observation?.cancel()
        rendering?.cancel()
        val updates = Channel<AppState>(Channel.CONFLATED)
        observation = controller.observe { updates.trySend(it) }
        rendering = scope.launch {
            var previousApp: IosAppViewState? = null
            var previousThread: IosThreadView? = null
            for (first in updates) {
                // Reconcile every event in common state; project only the latest frame.
                delay(16)
                val app = viewProjector.project(updates.tryReceive().getOrNull() ?: first)
                val thread = viewProjector.thread
                if (app !== previousApp || thread !== previousThread) {
                    previousApp = app
                    previousThread = thread
                    observer(app, thread)
                }
            }
        }
    }

    fun close() {
        observation?.cancel()
        observation = null
        rendering?.cancel()
        rendering = null
        connectionObservation?.cancel()
        connectionObservation = null
        IosLifecycleBridge.onRestoreAfterForeground = null
        IosLifecycleBridge.onPersistBeforeBackground = null
        scope.launch {
            try { controller.flushPersistence() } finally { scope.cancel() }
        }
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

    fun restoreAfterForeground() = controller.restoreConnection(scope)

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

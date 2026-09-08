package dev.remoteagent.mobile

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

class IosConversationActions
internal constructor(
    private val controller: MobileController,
    private val scope: CoroutineScope,
    private val dependencies: IosMobileDependencies,
) {
    fun sendTurn(text: String, attachments: List<CodexAttachment>, completion: (Boolean, String?) -> Unit) {
        val profile =
            controller.state.selectedProfile
                ?: run {
                    completion(false, null)
                    return
                }
        scope.launch {
            val result = controller.sendMessage(profile, text, attachments)
            completion(result.accepted, result.threadId)
        }
    }

    fun respond(requestIdJson: String, responseJson: String, completion: (String?) -> Unit) {
        val profile =
            controller.state.selectedProfile
                ?: run {
                    completion("接続先が選択されていません")
                    return
                }
        scope.launch {
            try {
                when (
                    val result =
                        controller.respond(
                            profile,
                            Json.parseToJsonElement(requestIdJson),
                            Json.parseToJsonElement(responseJson),
                        )
                ) {
                    is GatewayResult.Success -> completion(null)
                    is GatewayResult.Failure -> completion(result.message)
                }
            } catch (failure: IllegalArgumentException) {
                completion(failure.message ?: "JSONが無効です")
            }
        }
    }

    fun accountRequest(method: String, paramsJson: String, completion: (String?, String?) -> Unit) {
        val profile =
            controller.state.selectedProfile
                ?: run {
                    completion(null, "接続先が選択されていません")
                    return
                }
        if (
            method !in
                setOf(
                    "host/account/list",
                    "host/account/select",
                    "host/account/login/start",
                    "host/account/login/status",
                    "host/account/login/cancel",
                )
        ) {
            completion(null, "未対応のアカウント操作です")
            return
        }
        scope.launch {
            try {
                when (
                    val result = dependencies.gateway.rawRequest(profile, method, Json.parseToJsonElement(paramsJson))
                ) {
                    is GatewayResult.Success -> completion(result.value.toString(), null)
                    is GatewayResult.Failure -> completion(null, result.message)
                }
            } catch (failure: IllegalArgumentException) {
                completion(null, failure.message ?: "JSONが無効です")
            }
        }
    }

    fun forkThread(threadId: String, lastTurnId: String, completion: (String?, String?) -> Unit) {
        val profile =
            controller.state.selectedProfile
                ?: run {
                    completion(null, "接続先が選択されていません")
                    return
                }
        scope.launch {
            when (
                val result =
                    dependencies.gateway.rawRequest(
                        profile,
                        "thread/fork",
                        buildJsonObject {
                            put("threadId", threadId)
                            put("lastTurnId", lastTurnId)
                            put("excludeTurns", true)
                        },
                    )
            ) {
                is GatewayResult.Success -> completion(result.value.toString(), null)
                is GatewayResult.Failure -> completion(null, result.message)
            }
        }
    }

    fun readItemDetails(threadId: String, turnId: String, itemId: String, completion: (String?, String?) -> Unit) {
        val profile =
            controller.state.selectedProfile
                ?: run {
                    completion(null, "接続先が選択されていません")
                    return
                }
        scope.launch {
            when (val result = dependencies.gateway.codex.readItemDetails(profile, threadId, turnId, itemId)) {
                is GatewayResult.Success -> completion(result.value, null)
                is GatewayResult.Failure -> completion(null, result.message)
            }
        }
    }

    fun readSessionImages(threadId: String, completion: (List<String>?, String?) -> Unit) {
        val profile =
            controller.state.selectedProfile
                ?: run {
                    completion(null, "接続先が選択されていません")
                    return
                }
        scope.launch {
            try {
                val sources = dependencies.gateway.sessionImages(profile, threadId)
                completion(sources, null)
            } catch (cancelled: kotlinx.coroutines.CancellationException) {
                throw cancelled
            } catch (failure: IllegalStateException) {
                completion(null, failure.message)
            }
        }
    }

    fun transcribeAudio(audio: String, completion: (String?, String?) -> Unit) {
        val profile =
            controller.state.selectedProfile
                ?: run {
                    completion(null, "接続先が選択されていません")
                    return
                }
        scope.launch {
            val result =
                dependencies.gateway.rawRequest(
                    profile,
                    "host/dictation/transcribe",
                    buildJsonObject { put("audio", audio) },
                )
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

    fun interrupt(turnId: String) {
        val profile = controller.state.selectedProfile ?: return
        val threadId = controller.state.selectedView.selectedThreadId ?: return
        scope.launch { controller.interrupt(profile, threadId, turnId) }
    }

    fun listModels(completion: (List<CodexModel>?, String?) -> Unit) {
        val profile =
            controller.state.selectedProfile
                ?: run {
                    completion(null, "接続先が選択されていません")
                    return
                }
        scope.launch {
            when (val result = dependencies.gateway.codex.listModels(profile)) {
                is GatewayResult.Success -> completion(result.value, null)
                is GatewayResult.Failure -> completion(null, result.message)
            }
        }
    }

    fun setTurnOptions(hostIdentity: String, model: String?, effort: String?) {
        dependencies.gateway.codex.setTurnOptions(hostIdentity, CodexTurnOptions(model, effort))
    }
}

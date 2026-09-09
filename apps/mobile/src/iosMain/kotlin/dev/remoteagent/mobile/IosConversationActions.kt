package dev.remoteagent.mobile

import kotlinx.atomicfu.AtomicRef
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonPrimitive

class IosConversationActions
internal constructor(private val controller: AtomicRef<MobileApp>, private val scope: CoroutineScope) {
    fun sendTurn(
        text: String,
        attachments: List<CodexAttachment>,
        options: CodexTurnOptions,
        completion: (Boolean, String?) -> Unit,
    ) {
        val profile =
            controller.state.selectedProfile
                ?: run {
                    completion(false, null)
                    return
                }
        scope.launch {
            val result = controller.sendMessage(profile, text, attachments, options)
            completion(result.accepted, result.threadId)
        }
    }

    fun respond(requestJson: String, answer: RequestAnswer, completion: (String?) -> Unit) {
        val profile =
            controller.state.selectedProfile
                ?: run {
                    completion("接続先が選択されていません")
                    return
                }
        scope.launch {
            try {
                when (val result = controller.respond(profile, Json.parseToJsonElement(requestJson), answer)) {
                    is GatewayResult.Success -> completion(null)
                    is GatewayResult.Failure -> completion(result.message)
                }
            } catch (failure: IllegalArgumentException) {
                completion(failure.message ?: "JSONが無効です")
            }
        }
    }

    fun forkThread(threadId: String, lastTurnId: String, completion: (String?, String?) -> Unit) {
        scope.launch {
            when (val result = controller.forkThread(threadId, lastTurnId)) {
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
            when (
                val result =
                    controller.value.effects.gateway
                        .command(profile, AgentCommand.ReadItem(threadId, turnId, itemId))
                        .mapGateway { codexItem((it as JsonObject).getValue("item")).expandedThreadItemBody() }
            ) {
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
                val sources = controller.value.effects.gateway.sessionImages(profile, threadId)
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
            val result = controller.value.effects.gateway.command(profile, AgentCommand.Transcribe(audio))
            when (result) {
                is GatewayResult.Success -> completion(result.value.jsonPrimitive.content, null)
                is GatewayResult.Failure -> completion(null, result.message)
            }
        }
    }

    fun interrupt(turnId: String) {
        val profile = controller.state.selectedProfile ?: return
        val threadId = controller.state.selectedView.selectedThreadId ?: return
        scope.launch { controller.interrupt(profile, threadId, turnId) }
    }
}

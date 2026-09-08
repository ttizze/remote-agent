package dev.remoteagent.mobile

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.put

internal suspend fun MobileController.startTurn(
    profile: HostProfile,
    threadId: String,
    text: String,
    attachments: List<CodexAttachment> = emptyList(),
    options: CodexTurnOptions = CodexTurnOptions(),
): Boolean {
    val generation = connectedGeneration(profile.id) ?: return false
    val cached = state.cache.snapshot(profile.id, threadId)
    val input = CodexTurnInput(text, attachments, clientUserMessageIdGenerator())
    val displayText = input.displayText()
    val listed = state.cache.profile(profile.id).threadList.lastOrNull { it.id == threadId }
    val plan = planTurnSubmission(cached, listed)
    var acceptedTurnId = (plan as? SendPlan.Steer)?.turnId
    val result =
        when (plan) {
            is SendPlan.Steer -> gateway.codex.steerTurn(profile, threadId, plan.turnId, input)
            SendPlan.Queue -> gateway.codex.queueTurn(profile, threadId, input)
            is SendPlan.Start ->
                gateway.codex.startTurn(profile, threadId, plan.cwd, input, plan.resume, options).also {
                    if (it is GatewayResult.Success) acceptedTurnId = it.value
                }
            is SendPlan.Reject -> GatewayResult.Failure(plan.message)
        }
    return when (result) {
        is GatewayResult.Failure -> {
            dispatchIfCurrent(profile.id, generation) { AppAction.TurnFailed(profile.id, result.message) }
            false
        }
        is GatewayResult.Success -> {
            dispatchIfCurrent(profile.id, generation) {
                AppAction.MessageAccepted(profile.id, threadId, input.submission(cached, displayText, acceptedTurnId))
            }
            // Acceptance precedes persistence; subscribed items carry the subsequent body updates.
            true
        }
    }
}

@Serializable
internal sealed interface SendPlan {
    @Serializable @SerialName("steer") data class Steer(val turnId: String) : SendPlan

    @Serializable @SerialName("queue") data object Queue : SendPlan

    @Serializable @SerialName("start") data class Start(val cwd: String, val resume: Boolean) : SendPlan

    @Serializable @SerialName("reject") data class Reject(val message: String) : SendPlan
}

private val sendPlanJson = Json { classDiscriminator = "action" }

internal fun planTurnSubmission(snapshot: ThreadSnapshot?, listed: ThreadSummary?): SendPlan {
    fun summary(value: ThreadSummary) = buildJsonObject {
        put("cwd", value.workingDirectory.path)
        put("status", Json.encodeToJsonElement(ThreadStatus.serializer(), value.status))
    }
    val request = buildJsonObject {
        put("operation", "sendPlan")
        put(
            "snapshot",
            snapshot?.let {
                buildJsonObject {
                    summary(it.summary).forEach { (key, value) -> put(key, value) }
                    put(
                        "turns",
                        JsonArray(
                            it.turns.map { turn ->
                                buildJsonObject {
                                    put("id", turn.id)
                                    put("status", Json.encodeToJsonElement(TurnStatus.serializer(), turn.status))
                                }
                            }
                        ),
                    )
                }
            } ?: JsonNull,
        )
        put("listed", listed?.let(::summary) ?: JsonNull)
    }
    return sendPlanJson.decodeFromString<SendPlan>(nativeConversationPresentation(request.toString()))
}

private fun CodexTurnInput.displayText(): String = buildString {
    append(text)
    for (attachment in attachments) {
        if (!attachment.isImage) {
            if (isNotEmpty()) append('\n')
            append(attachmentMessageLabel(attachment.isImage, attachment.path, attachment.name))
        }
    }
}

private fun CodexTurnInput.submission(cached: ThreadSnapshot?, displayText: String, turnId: String?) =
    SubmittedMessage(
        clientUserMessageId,
        displayText,
        turnId,
        cached?.turns?.firstOrNull { it.id == turnId }?.items?.lastOrNull()?.id,
        attachments.mapNotNull { if (it.isImage) it.path else null },
    )

internal suspend fun MobileController.respond(
    profile: HostProfile,
    requestId: kotlinx.serialization.json.JsonElement,
    response: kotlinx.serialization.json.JsonElement,
): GatewayResult<Unit> {
    val generation = connectedGeneration(profile.id) ?: return GatewayResult.Failure("接続が切れています")
    val result = gateway.respondResult(profile, requestId, response)
    if (result is GatewayResult.Failure)
        dispatchIfCurrent(profile.id, generation) { AppAction.TurnFailed(profile.id, result.message) }
    return result
}

internal suspend fun MobileController.interrupt(profile: HostProfile, threadId: String, turnId: String) {
    val generation = sessions.currentGeneration(profile.id) ?: return
    if (!isConnected(profile.id, generation)) return
    dispatchIfCurrent(profile.id, generation) { AppAction.InterruptStarted(profile.id, turnId) }
    gateway.codex
        .interrupt(profile, threadId, turnId)
        .fold(
            success = { dispatchIfCurrent(profile.id, generation) { AppAction.InterruptFinished(profile.id) } },
            failure = { message ->
                dispatchIfCurrent(profile.id, generation) { AppAction.InterruptFinished(profile.id) }
                dispatchIfCurrent(profile.id, generation) { AppAction.TurnFailed(profile.id, message) }
            },
        )
}

internal fun MobileController.cachedWorkingDirectory(hostIdentity: String, threadId: String): String? {
    val cache = state.cache.profile(hostIdentity)
    val snapshotCwd = cache.snapshots[threadId]?.summary?.workingDirectory?.path
    if (!snapshotCwd.isNullOrBlank()) return snapshotCwd
    return cache.threadList.firstOrNull { it.id == threadId }?.workingDirectory?.path?.takeIf(String::isNotBlank)
}

internal suspend fun <T> GatewayResult<T>.fold(success: suspend (T) -> Unit, failure: suspend (String) -> Unit) =
    when (this) {
        is GatewayResult.Success -> success(value)
        is GatewayResult.Failure -> failure(message)
    }

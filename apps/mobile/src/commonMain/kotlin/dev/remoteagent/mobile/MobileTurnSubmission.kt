package dev.remoteagent.mobile

import kotlinx.atomicfu.AtomicRef
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.put

internal suspend fun AtomicRef<MobileApp>.startTurn(
    profile: HostProfile,
    threadId: String,
    text: String,
    attachments: List<CodexAttachment> = emptyList(),
    options: CodexTurnOptions = CodexTurnOptions(),
): Boolean {
    val generation = connectedGeneration(profile.id) ?: return false
    val cached = state.cache.snapshot(profile.id, threadId)
    val input = CodexTurnInput(text, attachments, value.effects.newMessageId())
    val displayText = input.displayText()
    val listed = state.cache.profile(profile.id).threadList.lastOrNull { it.id == threadId }
    val result =
        value.effects.gateway
            .command(
                profile,
                AgentCommand.SendTurn(
                    threadId,
                    cached.submissionMetadata(),
                    listed.submissionMetadata(),
                    input,
                    options.model,
                    options.effort,
                ),
            )
            .mapGateway { (it as JsonPrimitive).contentOrNull }
    return when (result) {
        is GatewayResult.Failure -> {
            dispatchIfCurrent(profile.id, generation) { AppAction.TurnFailed(profile.id, result.message) }
            false
        }
        is GatewayResult.Success -> {
            dispatchIfCurrent(profile.id, generation) {
                AppAction.MessageAccepted(profile.id, threadId, input.submission(cached, displayText, result.value))
            }
            // Acceptance precedes persistence; subscribed items carry the subsequent body updates.
            true
        }
    }
}

internal fun ThreadSummary?.submissionMetadata(): JsonElement =
    this?.let {
        buildJsonObject {
            put("cwd", it.workingDirectory.path)
            put("status", Json.encodeToJsonElement(ThreadStatus.serializer(), it.status))
        }
    } ?: JsonNull

internal fun ThreadSnapshot?.submissionMetadata(): JsonElement =
    this?.let {
        buildJsonObject {
            (it.summary.submissionMetadata() as JsonObject).forEach { (key, value) -> put(key, value) }
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
    } ?: JsonNull

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

internal suspend fun AtomicRef<MobileApp>.respond(
    profile: HostProfile,
    request: kotlinx.serialization.json.JsonElement,
    answer: RequestAnswer,
): GatewayResult<Unit> {
    if (connectedGeneration(profile.id) == null) return GatewayResult.Failure("接続が切れています")
    // The request editor owns validation errors; a rejected answer is not a failed turn.
    return value.effects.gateway.command(profile, AgentCommand.Respond(request, answer)).mapGateway { Unit }
}

internal suspend fun AtomicRef<MobileApp>.interrupt(profile: HostProfile, threadId: String, turnId: String) {
    val generation = value.effects.sessions.currentGeneration(profile.id) ?: return
    if (!isConnected(profile.id, generation)) return
    dispatchIfCurrent(profile.id, generation) { AppAction.InterruptStarted(profile.id, turnId) }
    value.effects.gateway
        .command(profile, AgentCommand.InterruptTurn(threadId, turnId))
        .fold(
            success = { dispatchIfCurrent(profile.id, generation) { AppAction.InterruptFinished(profile.id) } },
            failure = { message ->
                dispatchIfCurrent(profile.id, generation) { AppAction.InterruptFinished(profile.id) }
                dispatchIfCurrent(profile.id, generation) { AppAction.TurnFailed(profile.id, message) }
            },
        )
}

internal fun AtomicRef<MobileApp>.cachedWorkingDirectory(hostIdentity: String, threadId: String): String? {
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

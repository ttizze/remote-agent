package dev.remoteagent.mobile

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

internal fun codexThreadEvent(
    method: String,
    params: JsonElement,
    extensions: JsonObject = emptyJsonObject(),
): ThreadEvent {
    val raw = params.asObjectOrNull() ?: JsonObject(mapOf("value" to params))
    val threadId = raw.string("threadId").orEmpty()
    val kind =
        Json.decodeFromString<String>(
            nativeConversationPresentation(
                buildJsonObject {
                        put("operation", "eventKind")
                        put("method", method)
                    }
                    .toString()
            )
        )
    return when (kind) {
        "turnStarted",
        "turnCompleted" -> turnLifecycleEvent(threadId, raw, kind == "turnStarted")
        "itemStarted",
        "itemCompleted" -> itemLifecycleEvent(threadId, raw, kind == "itemStarted")
        "agentMessageDelta",
        "reasoningDelta",
        "reasoningSummaryDelta",
        "commandOutputDelta",
        "fileChangeOutputDelta" -> contentDeltaEvent(kind, threadId, raw)
        "error" ->
            ThreadEvent.Error(
                threadId = threadId,
                turnId = raw.string("turnId").orEmpty(),
                error =
                    codexTurnError(raw.childObject("error") ?: emptyJsonObject())
                        .copy(willRetry = raw.boolean("willRetry") == true),
                willRetry = raw.boolean("willRetry") == true,
            )
        "requestResolved" ->
            ThreadEvent.RequestResolved(
                threadId = threadId,
                requestId = raw["requestId"]?.stringOrNull() ?: raw["requestId"].toString(),
            )
        "threadStatusChanged" -> ThreadEvent.ThreadStatusChanged(threadId, status = codexThreadStatus(raw["status"]))
        "guardianReviewChanged" ->
            ThreadEvent.GuardianReviewChanged(
                threadId = threadId,
                turnId = raw.string("turnId").orEmpty(),
                reviewId = raw.string("reviewId").orEmpty(),
                status = raw.childObject("review")?.string("status").orEmpty(),
                raw = raw,
            )
        else -> ThreadEvent.Unknown(threadId, raw.string("turnId").orEmpty(), method, raw, extensions)
    }
}

private fun turnLifecycleEvent(threadId: String, raw: JsonObject, started: Boolean): ThreadEvent {
    val turn = raw.childObject("turn")
    val turnId = turn?.string("id").orEmpty()
    val startedAt = unixSecondsToMilliseconds(turn?.long("startedAt"))
    return if (started) ThreadEvent.TurnStarted(threadId, turnId, TurnStatus.InProgress, startedAt)
    else
        ThreadEvent.TurnCompleted(
            threadId,
            turnId,
            codexTurnStatus(turn?.string("status")),
            startedAt,
            unixSecondsToMilliseconds(turn?.long("completedAt")),
            nonNegative(turn?.long("durationMs")),
            turn?.childObject("error")?.let(::codexTurnError),
        )
}

private fun itemLifecycleEvent(threadId: String, raw: JsonObject, started: Boolean): ThreadEvent {
    val turnId = raw.string("turnId").orEmpty()
    val item = codexItem(raw["item"] ?: emptyJsonObject())
    return if (started) ThreadEvent.ItemStarted(threadId, turnId, item)
    else ThreadEvent.ItemCompleted(threadId, turnId, item)
}

private fun contentDeltaEvent(kind: String, threadId: String, raw: JsonObject): ThreadEvent {
    val turnId = raw.string("turnId").orEmpty()
    val itemId = raw.string("itemId").orEmpty()
    val delta = raw.string("delta").orEmpty()
    return when (kind) {
        "agentMessageDelta" -> ThreadEvent.AgentMessageDelta(threadId, turnId, itemId, delta)
        "reasoningDelta" -> ThreadEvent.ReasoningDelta(threadId, turnId, itemId, delta)
        "reasoningSummaryDelta" -> ThreadEvent.ReasoningSummaryDelta(threadId, turnId, itemId, delta)
        "commandOutputDelta" -> ThreadEvent.CommandOutputDelta(threadId, turnId, itemId, delta)
        "fileChangeOutputDelta" -> ThreadEvent.FileChangeOutputDelta(threadId, turnId, itemId, delta)
        else -> error("Not a content delta: $kind")
    }
}

internal fun codexThreadEvent(message: RawCodexMessage): ThreadEvent =
    when (message) {
        is RawCodexMessage.Notification -> codexThreadEvent(message.method, message.params, message.extensions)
        is RawCodexMessage.ServerRequest -> {
            val params = message.params.asObjectOrNull() ?: JsonObject(mapOf("value" to message.params))
            ThreadEvent.RequestStarted(
                threadId = params.string("threadId").orEmpty(),
                turnId = params.string("turnId").orEmpty(),
                request =
                    CodexServerRequest(
                        id = message.id.stringOrNull() ?: message.id.toString(),
                        method = message.method,
                        params = params,
                        wireId = message.id,
                    ),
            )
        }
    }

package dev.remoteagent.mobile

import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject

internal fun codexThreadEvent(
    method: String,
    params: JsonElement,
    extensions: JsonObject = emptyJsonObject(),
): ThreadEvent {
    val raw = params.asObjectOrNull() ?: JsonObject(mapOf("value" to params))
    val threadId = raw.string("threadId").orEmpty()
    val kind = ConversationEventKind.entries[nativeClassifyEvent(method)]
    return when (kind) {
        ConversationEventKind.TurnStarted,
        ConversationEventKind.TurnCompleted ->
            turnLifecycleEvent(threadId, raw, kind == ConversationEventKind.TurnStarted)
        ConversationEventKind.ItemStarted,
        ConversationEventKind.ItemCompleted ->
            itemLifecycleEvent(threadId, raw, kind == ConversationEventKind.ItemStarted)
        ConversationEventKind.AgentMessageDelta,
        ConversationEventKind.ReasoningDelta,
        ConversationEventKind.ReasoningSummaryDelta,
        ConversationEventKind.CommandOutputDelta,
        ConversationEventKind.FileChangeOutputDelta -> contentDeltaEvent(kind, threadId, raw)
        ConversationEventKind.Error ->
            ThreadEvent.Error(
                threadId = threadId,
                turnId = raw.string("turnId").orEmpty(),
                error =
                    codexTurnError(raw.childObject("error") ?: emptyJsonObject())
                        .copy(willRetry = raw.boolean("willRetry") == true),
                willRetry = raw.boolean("willRetry") == true,
            )
        ConversationEventKind.RequestResolved ->
            ThreadEvent.RequestResolved(
                threadId = threadId,
                requestId = raw["requestId"]?.stringOrNull() ?: raw["requestId"].toString(),
            )
        ConversationEventKind.ThreadStatusChanged ->
            ThreadEvent.ThreadStatusChanged(threadId, status = codexThreadStatus(raw["status"]))
        ConversationEventKind.GuardianReviewChanged ->
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
    val turn = codexTurn(raw["turn"] ?: emptyJsonObject())
    return if (started) ThreadEvent.TurnStarted(threadId, turn) else ThreadEvent.TurnCompleted(threadId, turn)
}

private fun itemLifecycleEvent(threadId: String, raw: JsonObject, started: Boolean): ThreadEvent {
    val turnId = raw.string("turnId").orEmpty()
    val item = codexItem(raw["item"] ?: emptyJsonObject())
    return if (started) ThreadEvent.ItemStarted(threadId, turnId, item)
    else ThreadEvent.ItemCompleted(threadId, turnId, item)
}

private fun contentDeltaEvent(kind: ConversationEventKind, threadId: String, raw: JsonObject): ThreadEvent {
    val turnId = raw.string("turnId").orEmpty()
    val itemId = raw.string("itemId").orEmpty()
    val delta = raw.string("delta").orEmpty()
    return when (kind) {
        ConversationEventKind.AgentMessageDelta -> ThreadEvent.AgentMessageDelta(threadId, turnId, itemId, delta)
        ConversationEventKind.ReasoningDelta -> ThreadEvent.ReasoningDelta(threadId, turnId, itemId, delta)
        ConversationEventKind.ReasoningSummaryDelta ->
            ThreadEvent.ReasoningSummaryDelta(threadId, turnId, itemId, delta)
        ConversationEventKind.CommandOutputDelta -> ThreadEvent.CommandOutputDelta(threadId, turnId, itemId, delta)
        ConversationEventKind.FileChangeOutputDelta ->
            ThreadEvent.FileChangeOutputDelta(threadId, turnId, itemId, delta)
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

package dev.remoteagent.mobile

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/** Native body storage adapter; Rust decides the state transition. */
internal fun ThreadSnapshot.applyConversationEvent(event: ThreadEvent): ThreadSnapshot {
    val turnIndex = turns.indexOfLast { it.id == event.turnId }
    val current = turns.getOrNull(turnIndex)
    val itemId = event.storageItemId
    val itemIndex = if (itemId == null) -1 else current?.items?.indexOfFirst { it.id == itemId } ?: -1
    val delta = event.storageDelta
    val decision = conversationTransition(event, current, itemIndex, delta)
    return when (decision.action) {
        "ignore" -> this
        "threadStatus" -> copy(summary = summary.copy(status = (event as ThreadEvent.ThreadStatusChanged).status))
        "resolveRequest" -> resolveRequest((event as ThreadEvent.RequestResolved).requestId)
        else -> {
            val updated =
                if (decision.action == "turn") applyTurnLifecycle(current, event, decision)
                else current?.applyItemTransition(event, decision, itemIndex, delta)
            if (updated == null || updated == current) this
            else
                copy(
                    turns =
                        if (turnIndex < 0) turns + updated else turns.toMutableList().also { it[turnIndex] = updated }
                )
        }
    }
}

private fun ThreadSnapshot.resolveRequest(id: String): ThreadSnapshot {
    val updated = turns.map { turn ->
        if (turn.pendingRequests.none { it.id == id }) turn
        else turn.copy(pendingRequests = turn.pendingRequests.filterNot { it.id == id })
    }
    return if (updated.indices.all { updated[it] === turns[it] }) this else copy(turns = updated)
}

private fun conversationTransition(
    event: ThreadEvent,
    current: CodexTurn?,
    itemIndex: Int,
    delta: String,
): ConversationTransition {
    val request = buildJsonObject {
        put("operation", "transition")
        put(
            "event",
            buildJsonObject {
                put("kind", event.conversationKind)
                when (event) {
                    is ThreadEvent.TurnStarted -> put("status", "inProgress")
                    is ThreadEvent.TurnCompleted -> {
                        put("status", Json.encodeToJsonElement(TurnStatus.serializer(), event.status))
                        put("hasError", event.error != null)
                    }
                    is ThreadEvent.GuardianReviewChanged -> put("status", event.status)
                    is ThreadEvent.Error -> put("willRetry", event.error.willRetry)
                    else -> Unit
                }
                put("emptyDelta", delta.isEmpty())
            },
        )
        put(
            "current",
            buildJsonObject {
                current?.let {
                    put("turnStatus", Json.encodeToJsonElement(TurnStatus.serializer(), it.status))
                    put("retryingError", it.error?.willRetry == true)
                    it.items.getOrNull(itemIndex)?.let { item -> put("itemType", item.conversationType) }
                }
            },
        )
    }
    return Json.decodeFromString<ConversationTransition>(nativeConversationPresentation(request.toString()))
}

private fun applyTurnLifecycle(current: CodexTurn?, event: ThreadEvent, decision: ConversationTransition): CodexTurn {
    val incoming =
        when (event) {
            is ThreadEvent.TurnStarted -> CodexTurn(event.turnId, event.status, startedAtMs = event.startedAtMs)
            is ThreadEvent.TurnCompleted ->
                CodexTurn(
                    event.turnId,
                    event.status,
                    startedAtMs = event.startedAtMs,
                    completedAtMs = event.completedAtMs,
                    durationMs = event.durationMs,
                    error = event.error,
                )
            else -> error("Lifecycle transition requires a turn event")
        }
    return (current ?: incoming).copy(
        status = Json.decodeFromJsonElement(TurnStatus.serializer(), JsonPrimitive(decision.status!!)),
        startedAtMs = incoming.startedAtMs ?: current?.startedAtMs,
        completedAtMs = incoming.completedAtMs ?: current?.completedAtMs,
        durationMs = incoming.durationMs ?: current?.durationMs,
        error = if (decision.clearError) null else incoming.error ?: current?.error,
    )
}

private fun CodexTurn.applyItemTransition(
    event: ThreadEvent,
    decision: ConversationTransition,
    itemIndex: Int,
    delta: String,
): CodexTurn? {
    return when (decision.action) {
        "item" -> {
            val item = event.storageItem()
            this.copy(
                items =
                    if (itemIndex < 0) this.items + item else this.items.toMutableList().also { it[itemIndex] = item }
            )
        }
        "removeItem" ->
            if (itemIndex < 0) null else this.copy(items = this.items.toMutableList().also { it.removeAt(itemIndex) })
        "append" ->
            this.copy(
                items = this.items.toMutableList().also { it[itemIndex] = it[itemIndex].appendConversationText(delta) }
            )
        "error" -> this.copy(error = (event as ThreadEvent.Error).error)
        "request" -> {
            val request = (event as ThreadEvent.RequestStarted).request
            this.copy(pendingRequests = this.pendingRequests.replaceById(request.id, request) { it.id })
        }
        else -> error("Unknown conversation transition: ${decision.action}")
    }
}

private val ThreadEvent.storageItemId: String?
    get() =
        when (val event = this) {
            is ThreadEvent.ItemStarted -> event.item.id
            is ThreadEvent.ItemCompleted -> event.item.id
            is ThreadEvent.AgentMessageDelta -> event.itemId
            is ThreadEvent.ReasoningDelta -> event.itemId
            is ThreadEvent.ReasoningSummaryDelta -> event.itemId
            is ThreadEvent.CommandOutputDelta -> event.itemId
            is ThreadEvent.FileChangeOutputDelta -> event.itemId
            is ThreadEvent.GuardianReviewChanged -> event.reviewId
            else -> null
        }

private val ThreadEvent.storageDelta: String
    get() =
        when (val event = this) {
            is ThreadEvent.AgentMessageDelta -> event.delta
            is ThreadEvent.ReasoningDelta -> event.delta
            is ThreadEvent.ReasoningSummaryDelta -> event.delta
            is ThreadEvent.CommandOutputDelta -> event.delta
            is ThreadEvent.FileChangeOutputDelta -> event.delta
            else -> ""
        }

@Serializable
private data class ConversationTransition(
    val action: String,
    val status: String?,
    val field: String?,
    val clearError: Boolean,
)

// These mappings describe the native types, not event acceptance or precedence.
internal val CodexItem.conversationType: String
    get() =
        when (this) {
            is CodexItem.UserMessage -> "userMessage"
            is CodexItem.AgentMessage -> "agentMessage"
            is CodexItem.Reasoning -> "reasoning"
            is CodexItem.CommandExecution -> "commandExecution"
            is CodexItem.FileChange -> "fileChange"
            is CodexItem.Unknown -> codexType
        }
private val ThreadEvent.conversationKind: String
    get() =
        when (this) {
            is ThreadEvent.TurnStarted -> "turnStarted"
            is ThreadEvent.TurnCompleted -> "turnCompleted"
            is ThreadEvent.ItemStarted -> "itemStarted"
            is ThreadEvent.ItemCompleted -> "itemCompleted"
            is ThreadEvent.AgentMessageDelta -> "agentMessageDelta"
            is ThreadEvent.ReasoningDelta -> "reasoningDelta"
            is ThreadEvent.ReasoningSummaryDelta -> "reasoningSummaryDelta"
            is ThreadEvent.CommandOutputDelta -> "commandOutputDelta"
            is ThreadEvent.FileChangeOutputDelta -> "fileChangeOutputDelta"
            is ThreadEvent.Error -> "error"
            is ThreadEvent.RequestStarted -> "requestStarted"
            is ThreadEvent.RequestResolved -> "requestResolved"
            is ThreadEvent.ThreadStatusChanged -> "threadStatusChanged"
            is ThreadEvent.GuardianReviewChanged -> "guardianReviewChanged"
            is ThreadEvent.Unknown -> "unknown"
        }

private fun CodexItem.appendConversationText(delta: String): CodexItem =
    when (this) {
        is CodexItem.AgentMessage -> copy(text = text + delta)
        is CodexItem.Reasoning -> copy(summary = summary + delta)
        is CodexItem.CommandExecution -> copy(output = output + delta)
        is CodexItem.FileChange ->
            copy(
                changes =
                    when {
                        changes.isEmpty() -> listOf(FileUpdateChange("", FileUpdateKind.Update, delta))
                        else ->
                            changes.toMutableList().also {
                                it[it.lastIndex] = it.last().copy(diff = it.last().diff + delta)
                            }
                    }
            )
        else -> error("Shared transition selected an item without text storage")
    }

private fun ThreadEvent.storageItem(): CodexItem =
    when (this) {
        is ThreadEvent.ItemStarted -> item
        is ThreadEvent.ItemCompleted -> item
        is ThreadEvent.GuardianReviewChanged -> CodexItem.Unknown(reviewId, "automaticApprovalReview", raw)
        else -> error("Item transition requires an item event")
    }

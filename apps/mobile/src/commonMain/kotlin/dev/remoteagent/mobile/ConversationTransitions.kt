package dev.remoteagent.mobile

import kotlin.jvm.JvmInline
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put

private enum class ConversationMutation {
    Ignore,
    ThreadStatus,
    Turn,
    Item,
    RemoveItem,
    Append,
    Error,
    Request,
    ResolveRequest,
}

internal expect fun nativeClassifyEvent(method: String): Int

internal expect fun nativeConversationTransition(kind: Int, status: Int, currentStatus: Int, item: Int, flags: Int): Int

/** Native body storage adapter; Rust decides the state transition. */
internal fun ThreadSnapshot.applyConversationEvent(event: ThreadEvent): ThreadSnapshot {
    val turnIndex = turns.indexOfLast { it.id == event.turnId }
    val current = turns.getOrNull(turnIndex)
    val itemId = event.storageItemId
    val itemIndex = if (itemId == null) -1 else current?.items?.indexOfFirst { it.id == itemId } ?: -1
    val delta = event.storageDelta
    val decision = conversationTransition(event, current, itemIndex, delta)
    return when (decision.action) {
        ConversationMutation.Ignore -> this
        ConversationMutation.ThreadStatus ->
            copy(summary = summary.copy(status = (event as ThreadEvent.ThreadStatusChanged).status))
        ConversationMutation.ResolveRequest -> resolveRequest((event as ThreadEvent.RequestResolved).requestId)
        else -> {
            val updated =
                if (decision.action == ConversationMutation.Turn) applyTurnLifecycle(current, event)
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
    val status = event.wireStatus
    val item =
        when (current?.items?.getOrNull(itemIndex)) {
            is CodexItem.AgentMessage -> NativeItemKind.AgentMessage
            is CodexItem.Reasoning -> NativeItemKind.Reasoning
            is CodexItem.CommandExecution -> NativeItemKind.CommandExecution
            is CodexItem.FileChange -> NativeItemKind.FileChange
            else -> NativeItemKind.Unknown
        }.ordinal
    val flags =
        (if (event is ThreadEvent.TurnCompleted && event.turn.error != null) 1 else 0) or
            (if (event is ThreadEvent.Error && event.error.willRetry) 2 else 0) or
            (if (delta.isEmpty()) EMPTY_DELTA else 0) or
            (if (current?.error?.willRetry == true) RETRYING_ERROR else 0)
    return ConversationTransition(
        nativeConversationTransition(
            event.conversationKind.ordinal,
            status,
            current?.status?.wireStatus ?: 0,
            item,
            flags,
        )
    )
}

// Stable C/JNI codes documented in mobile_client.h.
private enum class NativeTurnStatus {
    Absent,
    InProgress,
    Completed,
    Failed,
    Interrupted,
    Approved,
}

private enum class NativeItemKind {
    Unknown,
    AgentMessage,
    Reasoning,
    CommandExecution,
    FileChange,
}

private const val EMPTY_DELTA = 4
private const val RETRYING_ERROR = 8
private const val ACTION_MASK = 255
private const val MILLISECONDS_PER_SECOND = 1000
private val ThreadEvent.wireStatus: Int
    get() =
        when (this) {
            is ThreadEvent.TurnStarted -> NativeTurnStatus.InProgress.ordinal
            is ThreadEvent.TurnCompleted -> turn.status.wireStatus
            is ThreadEvent.GuardianReviewChanged ->
                if (status == "approved") NativeTurnStatus.Approved.ordinal else NativeTurnStatus.Absent.ordinal
            else -> NativeTurnStatus.Absent.ordinal
        }
private val TurnStatus.wireStatus: Int
    get() =
        when (this) {
            TurnStatus.InProgress -> NativeTurnStatus.InProgress
            TurnStatus.Completed -> NativeTurnStatus.Completed
            TurnStatus.Failed -> NativeTurnStatus.Failed
            TurnStatus.Interrupted -> NativeTurnStatus.Interrupted
        }.ordinal

private fun applyTurnLifecycle(current: CodexTurn?, event: ThreadEvent): CodexTurn {
    val incoming =
        when (event) {
            is ThreadEvent.TurnStarted -> event.turn
            is ThreadEvent.TurnCompleted -> event.turn
            else -> error("Lifecycle transition requires a turn event")
        }
    val result =
        Json.parseToJsonElement(
                nativeConversationPresentation(
                    buildJsonObject {
                            put("operation", "turnLifecycle")
                            put("started", event is ThreadEvent.TurnStarted)
                            put("previous", current?.lifecycleMetadata("previous") ?: JsonNull)
                            put("incoming", incoming.lifecycleMetadata("incoming"))
                        }
                        .toString()
                )
            )
            .jsonObject
    val items =
        result.getValue("items").jsonArray.map { token ->
            val reference = token.jsonObject
            (if (reference.string("source") == "previous") requireNotNull(current).items else incoming.items)[
                reference.getValue("index").jsonPrimitive.int]
        }
    return codexTurn(result.without("items")).copy(items = items, pendingRequests = current?.pendingRequests.orEmpty())
}

/** Metadata and source positions only: text/tool bodies remain in native storage. */
private fun CodexTurn.lifecycleMetadata(source: String): JsonObject = buildJsonObject {
    raw?.forEach { (key, value) -> put(key, value) }
    put("id", id)
    put("status", Json.encodeToJsonElement(TurnStatus.serializer(), status))
    startedAtMs?.let { put("startedAt", it / MILLISECONDS_PER_SECOND) }
    completedAtMs?.let { put("completedAt", it / MILLISECONDS_PER_SECOND) }
    durationMs?.let { put("durationMs", it) }
    error?.let { put("error", Json.encodeToJsonElement(CodexTurnError.serializer(), it)) }
    put(
        "items",
        JsonArray(
            items.mapIndexed { index, item ->
                buildJsonObject {
                    put("id", item.id)
                    put("source", source)
                    put("index", index)
                }
            }
        ),
    )
}

private fun CodexTurn.applyItemTransition(
    event: ThreadEvent,
    decision: ConversationTransition,
    itemIndex: Int,
    delta: String,
): CodexTurn? {
    return when (decision.action) {
        ConversationMutation.Item -> {
            val item = event.storageItem()
            this.copy(
                items =
                    if (itemIndex < 0) this.items + item else this.items.toMutableList().also { it[itemIndex] = item }
            )
        }
        ConversationMutation.RemoveItem ->
            if (itemIndex < 0) null else this.copy(items = this.items.toMutableList().also { it.removeAt(itemIndex) })
        ConversationMutation.Append ->
            this.copy(
                items = this.items.toMutableList().also { it[itemIndex] = it[itemIndex].appendConversationText(delta) }
            )
        ConversationMutation.Error -> this.copy(error = (event as ThreadEvent.Error).error)
        ConversationMutation.Request -> {
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

@JvmInline
private value class ConversationTransition(val code: Int) {
    val action: ConversationMutation
        get() = ConversationMutation.entries[code and ACTION_MASK]
}

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
private val ThreadEvent.conversationKind: ConversationEventKind
    get() =
        when (this) {
            is ThreadEvent.TurnStarted -> ConversationEventKind.TurnStarted
            is ThreadEvent.TurnCompleted -> ConversationEventKind.TurnCompleted
            is ThreadEvent.ItemStarted -> ConversationEventKind.ItemStarted
            is ThreadEvent.ItemCompleted -> ConversationEventKind.ItemCompleted
            is ThreadEvent.AgentMessageDelta -> ConversationEventKind.AgentMessageDelta
            is ThreadEvent.ReasoningDelta -> ConversationEventKind.ReasoningDelta
            is ThreadEvent.ReasoningSummaryDelta -> ConversationEventKind.ReasoningSummaryDelta
            is ThreadEvent.CommandOutputDelta -> ConversationEventKind.CommandOutputDelta
            is ThreadEvent.FileChangeOutputDelta -> ConversationEventKind.FileChangeOutputDelta
            is ThreadEvent.Error -> ConversationEventKind.Error
            is ThreadEvent.RequestStarted -> ConversationEventKind.RequestStarted
            is ThreadEvent.RequestResolved -> ConversationEventKind.RequestResolved
            is ThreadEvent.ThreadStatusChanged -> ConversationEventKind.ThreadStatusChanged
            is ThreadEvent.GuardianReviewChanged -> ConversationEventKind.GuardianReviewChanged
            is ThreadEvent.Unknown -> ConversationEventKind.Unknown
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

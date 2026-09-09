package dev.remoteagent.mobile

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

// Stable C/JNI storage actions; Rust owns acceptance and lifecycle precedence.
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

internal expect fun nativeClassifyEvent(method: String): Int

internal expect fun nativeConversationTransition(kind: Int, status: Int, currentStatus: Int, item: Int, flags: Int): Int

/** Apply a shared storage action without re-encoding native message bodies. */
internal fun ThreadSnapshot.applyConversationEvent(event: RawCodexMessage): ThreadSnapshot {
    val params = event.paramsObject
    val turnIndex = turns.indexOfLast { it.id == event.turnId }
    val current = turns.getOrNull(turnIndex)
    val itemId = event.itemId
    val itemIndex = if (itemId == null) -1 else current?.items?.indexOfFirst { it.id == itemId } ?: -1
    val delta = params.string("delta").orEmpty()
    val action = conversationMutation(event, current, itemIndex, delta)
    return when (action) {
        ConversationMutation.Ignore -> this
        ConversationMutation.ThreadStatus -> copy(summary = summary.copy(status = codexThreadStatus(params["status"])))
        ConversationMutation.ResolveRequest ->
            resolveRequest(params["requestId"]?.stringOrNull() ?: params["requestId"].toString())
        else -> {
            val updated =
                if (action == ConversationMutation.Turn) applyTurnLifecycle(current, event)
                else current?.applyItemMutation(event, action, itemIndex, delta)
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

private fun conversationMutation(
    event: RawCodexMessage,
    current: CodexTurn?,
    itemIndex: Int,
    delta: String,
): ConversationMutation {
    val params = event.paramsObject
    val flags =
        (if (params.childObject("turn")?.get("error")?.let { it != JsonNull } == true) HAS_TERMINAL_ERROR else 0) or
            (if (params.boolean("willRetry") == true) WILL_RETRY else 0) or
            (if (delta.isEmpty()) EMPTY_DELTA else 0) or
            (if (current?.error?.willRetry == true) HAS_RETRYING_ERROR else 0)
    return ConversationMutation.entries[
            nativeConversationTransition(
                event.kind.ordinal,
                event.nativeStatus,
                current?.status?.wireStatus ?: 0,
                current?.items?.getOrNull(itemIndex).nativeKind,
                flags,
            ) and ACTION_MASK]
}

private val RawCodexMessage.nativeStatus: Int
    get() =
        when (kind) {
            ConversationEventKind.TurnStarted -> NativeTurnStatus.InProgress.ordinal
            ConversationEventKind.TurnCompleted ->
                codexTurnStatus(paramsObject.childObject("turn")?.string("status")).wireStatus
            ConversationEventKind.GuardianReviewChanged ->
                if (paramsObject.childObject("review")?.string("status") == "approved")
                    NativeTurnStatus.Approved.ordinal
                else NativeTurnStatus.Absent.ordinal
            else -> NativeTurnStatus.Absent.ordinal
        }

private val CodexItem?.nativeKind: Int
    get() =
        when (this) {
            is CodexItem.AgentMessage -> NativeItemKind.AgentMessage
            is CodexItem.Reasoning -> NativeItemKind.Reasoning
            is CodexItem.CommandExecution -> NativeItemKind.CommandExecution
            is CodexItem.FileChange -> NativeItemKind.FileChange
            else -> NativeItemKind.Unknown
        }.ordinal

private val TurnStatus.wireStatus: Int
    get() =
        when (this) {
            TurnStatus.InProgress -> NativeTurnStatus.InProgress
            TurnStatus.Completed -> NativeTurnStatus.Completed
            TurnStatus.Failed -> NativeTurnStatus.Failed
            TurnStatus.Interrupted -> NativeTurnStatus.Interrupted
        }.ordinal

private fun applyTurnLifecycle(current: CodexTurn?, event: RawCodexMessage): CodexTurn {
    val incoming = codexTurn(event.paramsObject["turn"] ?: emptyJsonObject())
    val result =
        Json.parseToJsonElement(
                nativeConversationPresentation(
                    buildJsonObject {
                        put("operation", "turnLifecycle")
                        put("started", event.kind == ConversationEventKind.TurnStarted)
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

private fun CodexTurn.applyItemMutation(
    event: RawCodexMessage,
    action: ConversationMutation,
    itemIndex: Int,
    delta: String,
): CodexTurn? {
    val params = event.paramsObject
    return when (action) {
        ConversationMutation.Item -> {
            val item = event.conversationItem()
            copy(items = if (itemIndex < 0) items + item else items.toMutableList().also { it[itemIndex] = item })
        }
        ConversationMutation.RemoveItem ->
            if (itemIndex < 0) null else copy(items = items.toMutableList().also { it.removeAt(itemIndex) })
        ConversationMutation.Append ->
            copy(items = items.toMutableList().also { it[itemIndex] = it[itemIndex].appendConversationText(delta) })
        ConversationMutation.Error ->
            copy(
                error =
                    codexTurnError(params.childObject("error") ?: emptyJsonObject())
                        .copy(willRetry = params.boolean("willRetry") == true)
            )
        ConversationMutation.Request -> copy(pendingRequests = pendingRequests.replaceById(event.id, event) { it.id })
        else -> error("Unknown conversation storage action: $action")
    }
}

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

private fun CodexItem.appendConversationText(delta: String): CodexItem =
    when (this) {
        is CodexItem.AgentMessage -> copy(text = text + delta)
        is CodexItem.Reasoning -> copy(summary = summary + delta)
        is CodexItem.CommandExecution -> copy(output = output + delta)
        is CodexItem.FileChange ->
            copy(
                changes =
                    if (changes.isEmpty()) listOf(FileUpdateChange("", FileUpdateKind.Update, delta))
                    else
                        changes.toMutableList().also {
                            it[it.lastIndex] = it.last().copy(diff = it.last().diff + delta)
                        }
            )
        else -> error("Shared transition selected an item without text storage")
    }

private const val HAS_TERMINAL_ERROR = 1
private const val WILL_RETRY = 2
private const val EMPTY_DELTA = 4
private const val HAS_RETRYING_ERROR = 8
private const val ACTION_MASK = 255
private const val MILLISECONDS_PER_SECOND = 1_000L

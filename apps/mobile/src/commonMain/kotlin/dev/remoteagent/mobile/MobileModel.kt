package dev.remoteagent.mobile

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/** UI projections of Codex data. Raw Codex values remain available on every projection. */
@Serializable
data class WorkingDirectory(val path: String)

/** Read-only projection of a local Codex Desktop Project. */
@Serializable
data class CodexProject(
    val id: String,
    val name: String,
    val roots: List<WorkingDirectory>,
    val position: Long,
    val createdAtMs: Long,
    val updatedAtMs: Long,
    val raw: JsonObject? = null,
)

@Serializable
sealed interface ThreadStatus {
    @Serializable @SerialName("notLoaded") data object NotLoaded : ThreadStatus
    @Serializable @SerialName("idle") data object Idle : ThreadStatus
    @Serializable @SerialName("systemError") data object SystemError : ThreadStatus
    @Serializable @SerialName("active") data class Active(val activeFlags: List<String>) : ThreadStatus
}

@Serializable
enum class TurnStatus {
    @SerialName("completed") Completed,
    @SerialName("interrupted") Interrupted,
    @SerialName("failed") Failed,
    @SerialName("inProgress") InProgress,
}

@Serializable
enum class AgentMessagePhase {
    @SerialName("commentary") Commentary,
    @SerialName("final_answer") FinalAnswer,
}

@Serializable
enum class CommandExecutionStatus {
    @SerialName("inProgress") InProgress,
    @SerialName("completed") Completed,
    @SerialName("failed") Failed,
    @SerialName("declined") Declined,
}

@Serializable
enum class FileChangeStatus {
    @SerialName("inProgress") InProgress,
    @SerialName("completed") Completed,
    @SerialName("failed") Failed,
    @SerialName("declined") Declined,
}

@Serializable
enum class FileUpdateKind {
    @SerialName("add") Add,
    @SerialName("update") Update,
    @SerialName("delete") Delete,
}

@Serializable
data class FileUpdateChange(val path: String, val kind: FileUpdateKind, val diff: String)

@Serializable
data class ThreadSummary(
    val id: String,
    val name: String? = null,
    val preview: String,
    val workingDirectory: WorkingDirectory,
    /** Desktop-explicit assignment, or App Server assignment when Desktop has no opinion. */
    val projectId: String? = null,
    val createdAtMs: Long,
    val updatedAtMs: Long,
    val status: ThreadStatus,
    val raw: JsonObject? = null,
)

@Serializable
data class SubmittedMessage(
    val clientId: String,
    val text: String,
    val turnId: String?,
    val afterItemId: String?,
    val imageSources: List<String> = emptyList(),
)

@Serializable
data class ThreadSnapshot(
    val summary: ThreadSummary,
    val turns: List<CodexTurn> = emptyList(),
    val raw: JsonObject? = null,
    val submittedMessages: List<SubmittedMessage> = emptyList(),
)

@Serializable
data class ThreadReadResult(val thread: ThreadSnapshot, val bufferedEvents: List<ThreadEvent>)

data class MessageSendResult(val accepted: Boolean, val threadId: String?)

@Serializable
data class CodexTurn(
    val id: String,
    val status: TurnStatus,
    val items: List<CodexItem> = emptyList(),
    val raw: JsonObject? = null,
    val startedAtMs: Long? = null,
    val completedAtMs: Long? = null,
    val durationMs: Long? = null,
    val error: CodexTurnError? = null,
    val pendingRequests: List<CodexServerRequest> = emptyList(),
)

@Serializable
data class CodexTurnError(
    val message: String,
    val additionalDetails: String? = null,
    val codexErrorInfo: JsonElement? = null,
    val willRetry: Boolean = false,
)

@Serializable
data class CodexServerRequest(
    val id: String,
    val method: String,
    val params: JsonObject,
    val wireId: JsonElement = kotlinx.serialization.json.JsonPrimitive(id),
)

/** A typed, user-visible Codex item. */
@Serializable
sealed interface CodexItem {
    val id: String

    @Serializable @SerialName("userMessage")
    data class UserMessage(
        override val id: String,
        val text: String,
        val clientId: String? = null,
        val imageSources: List<String> = emptyList(),
    ) : CodexItem

    @Serializable @SerialName("agentMessage")
    data class AgentMessage(
        override val id: String,
        val text: String,
        val phase: AgentMessagePhase? = null,
    ) : CodexItem

    @Serializable @SerialName("reasoning")
    data class Reasoning(override val id: String, val summary: String) : CodexItem

    @Serializable @SerialName("commandExecution")
    data class CommandExecution(
        override val id: String,
        val command: String,
        val cwd: String? = null,
        val output: String,
        val status: CommandExecutionStatus,
        val exitCode: Int? = null,
    ) : CodexItem

    @Serializable @SerialName("fileChange")
    data class FileChange(
        override val id: String,
        val changes: List<FileUpdateChange>,
        val status: FileChangeStatus,
    ) : CodexItem

    /** A future Codex item is still rendered/cached as raw JSON instead of disappearing. */
    @Serializable @SerialName("unknown")
    data class Unknown(
        override val id: String,
        val codexType: String,
        val raw: JsonObject,
    ) : CodexItem
}

@Serializable
sealed interface ThreadEvent {
    val threadId: String
    val turnId: String

    @Serializable @SerialName("turnStarted")
    data class TurnStarted(
        override val threadId: String,
        override val turnId: String,
        val status: TurnStatus,
        val startedAtMs: Long? = null,
    ) : ThreadEvent

    @Serializable @SerialName("turnCompleted")
    data class TurnCompleted(
        override val threadId: String,
        override val turnId: String,
        val status: TurnStatus,
        val startedAtMs: Long? = null,
        val completedAtMs: Long? = null,
        val durationMs: Long? = null,
        val error: CodexTurnError? = null,
    ) : ThreadEvent

    @Serializable @SerialName("itemStarted")
    data class ItemStarted(override val threadId: String, override val turnId: String, val item: CodexItem) : ThreadEvent

    @Serializable @SerialName("agentMessageDelta")
    data class AgentMessageDelta(override val threadId: String, override val turnId: String, val itemId: String, val delta: String) : ThreadEvent

    @Serializable @SerialName("reasoningDelta")
    data class ReasoningDelta(override val threadId: String, override val turnId: String, val itemId: String, val delta: String) : ThreadEvent

    @Serializable @SerialName("reasoningSummaryDelta")
    data class ReasoningSummaryDelta(override val threadId: String, override val turnId: String, val itemId: String, val delta: String) : ThreadEvent

    @Serializable @SerialName("commandOutputDelta")
    data class CommandOutputDelta(override val threadId: String, override val turnId: String, val itemId: String, val delta: String) : ThreadEvent

    @Serializable @SerialName("fileChangeOutputDelta")
    data class FileChangeOutputDelta(override val threadId: String, override val turnId: String, val itemId: String, val delta: String) : ThreadEvent

    @Serializable @SerialName("error")
    data class Error(
        override val threadId: String,
        override val turnId: String,
        val error: CodexTurnError,
        val willRetry: Boolean,
    ) : ThreadEvent

    @Serializable @SerialName("requestStarted")
    data class RequestStarted(
        override val threadId: String,
        override val turnId: String,
        val request: CodexServerRequest,
    ) : ThreadEvent

    @Serializable @SerialName("requestResolved")
    data class RequestResolved(
        override val threadId: String,
        override val turnId: String = "",
        val requestId: String,
    ) : ThreadEvent

    @Serializable @SerialName("threadStatusChanged")
    data class ThreadStatusChanged(
        override val threadId: String,
        override val turnId: String = "",
        val status: ThreadStatus,
    ) : ThreadEvent

    @Serializable @SerialName("guardianReviewChanged")
    data class GuardianReviewChanged(
        override val threadId: String,
        override val turnId: String,
        val reviewId: String,
        val status: String,
        val raw: JsonObject,
    ) : ThreadEvent

    @Serializable @SerialName("itemCompleted")
    data class ItemCompleted(override val threadId: String, override val turnId: String, val item: CodexItem) : ThreadEvent

    /** A notification that this mobile build does not understand yet. */
    @Serializable @SerialName("unknown")
    data class Unknown(
        override val threadId: String,
        override val turnId: String,
        val method: String,
        val raw: JsonObject,
        val extensions: JsonObject = JsonObject(emptyMap()),
    ) : ThreadEvent
}

internal fun ThreadSnapshot.upsertTurn(turn: CodexTurn): ThreadSnapshot = copy(
    turns = turns.replaceById(turn.id, turn) { it.id },
)

/** Native body storage adapter; Rust decides the state transition. */
internal fun ThreadSnapshot.applyConversationEvent(event: ThreadEvent): ThreadSnapshot {
    val turnIndex = turns.indexOfLast { it.id == event.turnId }
    val current = turns.getOrNull(turnIndex)
    val itemId = when (event) {
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
    val itemIndex = if (itemId == null) -1 else current?.items?.indexOfFirst { it.id == itemId } ?: -1
    val delta = when (event) {
        is ThreadEvent.AgentMessageDelta -> event.delta
        is ThreadEvent.ReasoningDelta -> event.delta
        is ThreadEvent.ReasoningSummaryDelta -> event.delta
        is ThreadEvent.CommandOutputDelta -> event.delta
        is ThreadEvent.FileChangeOutputDelta -> event.delta
        else -> ""
    }
    val request = buildJsonObject {
        put("operation", "transition")
        put("event", buildJsonObject {
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
        })
        put("current", buildJsonObject {
            current?.let {
                put("turnStatus", Json.encodeToJsonElement(TurnStatus.serializer(), it.status))
                put("retryingError", it.error?.willRetry == true)
                it.items.getOrNull(itemIndex)?.let { item -> put("itemType", item.conversationType) }
            }
        })
    }
    val decision = Json.decodeFromString<ConversationTransition>(nativeConversationPresentation(request.toString()))
    when (decision.action) {
        "ignore" -> return this
        "threadStatus" -> return copy(summary = summary.copy(status = (event as ThreadEvent.ThreadStatusChanged).status))
        "resolveRequest" -> {
            val id = (event as ThreadEvent.RequestResolved).requestId
            val updated = turns.map { turn ->
                if (turn.pendingRequests.none { it.id == id }) turn
                else turn.copy(pendingRequests = turn.pendingRequests.filterNot { it.id == id })
            }
            return if (updated.indices.all { updated[it] === turns[it] }) this else copy(turns = updated)
        }
    }
    val updated = if (decision.action == "turn") {
        val incoming = when (event) {
            is ThreadEvent.TurnStarted -> CodexTurn(event.turnId, event.status, startedAtMs = event.startedAtMs)
            is ThreadEvent.TurnCompleted -> CodexTurn(event.turnId, event.status, startedAtMs = event.startedAtMs,
                completedAtMs = event.completedAtMs, durationMs = event.durationMs, error = event.error)
            else -> error("Lifecycle transition requires a turn event")
        }
        (current ?: incoming).copy(
            status = Json.decodeFromJsonElement(TurnStatus.serializer(), JsonPrimitive(decision.status!!)),
            startedAtMs = incoming.startedAtMs ?: current?.startedAtMs,
            completedAtMs = incoming.completedAtMs ?: current?.completedAtMs,
            durationMs = incoming.durationMs ?: current?.durationMs,
            error = if (decision.clearError) null else incoming.error ?: current?.error,
        )
    } else {
        if (current == null) return this
        when (decision.action) {
            "item" -> {
                val item = when (event) {
                    is ThreadEvent.ItemStarted -> event.item
                    is ThreadEvent.ItemCompleted -> event.item
                    is ThreadEvent.GuardianReviewChanged -> CodexItem.Unknown(event.reviewId, "automaticApprovalReview", event.raw)
                    else -> error("Item transition requires an item event")
                }
                current.copy(items = if (itemIndex < 0) current.items + item else current.items.toMutableList().also { it[itemIndex] = item })
            }
            "removeItem" -> if (itemIndex < 0) return this else current.copy(items = current.items.toMutableList().also { it.removeAt(itemIndex) })
            "append" -> current.copy(items = current.items.toMutableList().also { it[itemIndex] = it[itemIndex].appendConversationText(delta) })
            "error" -> current.copy(error = (event as ThreadEvent.Error).error)
            "request" -> {
                val request = (event as ThreadEvent.RequestStarted).request
                current.copy(pendingRequests = current.pendingRequests.replaceById(request.id, request) { it.id })
            }
            else -> error("Unknown conversation transition: ${decision.action}")
        }
    }
    return if (updated == current) this else copy(turns = if (turnIndex < 0) turns + updated else turns.toMutableList().also { it[turnIndex] = updated })
}

@Serializable
private data class ConversationTransition(val action: String, val status: String?, val field: String?, val clearError: Boolean)

// These mappings describe the native types, not event acceptance or precedence.
internal val CodexItem.conversationType: String get() = when (this) {
    is CodexItem.UserMessage -> "userMessage"
    is CodexItem.AgentMessage -> "agentMessage"
    is CodexItem.Reasoning -> "reasoning"
    is CodexItem.CommandExecution -> "commandExecution"
    is CodexItem.FileChange -> "fileChange"
    is CodexItem.Unknown -> codexType
}
private val ThreadEvent.conversationKind: String get() = when (this) {
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
private fun CodexItem.appendConversationText(delta: String): CodexItem = when (this) {
    is CodexItem.AgentMessage -> copy(text = text + delta)
    is CodexItem.Reasoning -> copy(summary = summary + delta)
    is CodexItem.CommandExecution -> copy(output = output + delta)
    is CodexItem.FileChange -> copy(changes = when {
        changes.isEmpty() -> listOf(FileUpdateChange("", FileUpdateKind.Update, delta))
        else -> changes.toMutableList().also { it[it.lastIndex] = it.last().copy(diff = it.last().diff + delta) }
    })
    else -> error("Shared transition selected an item without text storage")
}

private inline fun <T> List<T>.replaceById(id: String, value: T, idOf: (T) -> String): List<T> {
    val index = indexOfFirst { idOf(it) == id }
    return if (index < 0) this + value else if (this[index] == value) this else toMutableList().also { it[index] = value }
}

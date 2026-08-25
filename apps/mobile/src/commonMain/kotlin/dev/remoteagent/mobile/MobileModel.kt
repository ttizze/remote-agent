package dev.remoteagent.mobile

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonObject

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
data class ThreadSnapshot(
    val summary: ThreadSummary,
    val turns: List<CodexTurn> = emptyList(),
    val raw: JsonObject? = null,
)

@Serializable
data class ThreadReadResult(val thread: ThreadSnapshot, val bufferedEvents: List<ThreadEvent>)

data class ThreadStartResult(val thread: ThreadSnapshot, val turnId: String)

@Serializable
data class CodexTurn(
    val id: String,
    val status: TurnStatus,
    val items: List<CodexItem> = emptyList(),
    val raw: JsonObject? = null,
)

/** A typed, user-visible Codex item. */
@Serializable
sealed interface CodexItem {
    val id: String

    @Serializable @SerialName("userMessage")
    data class UserMessage(override val id: String, val text: String) : CodexItem

    @Serializable @SerialName("agentMessage")
    data class AgentMessage(override val id: String, val text: String) : CodexItem

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
    data class TurnStarted(override val threadId: String, override val turnId: String, val status: TurnStatus) : ThreadEvent

    @Serializable @SerialName("turnCompleted")
    data class TurnCompleted(override val threadId: String, override val turnId: String, val status: TurnStatus) : ThreadEvent

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

internal fun ThreadSnapshot.upsertItem(turnId: String, item: CodexItem): ThreadSnapshot = copy(
    turns = turns.map { turn ->
        if (turn.id == turnId) turn.copy(items = turn.items.replaceById(item.id, item) { it.id }) else turn
    },
)

internal fun ThreadSnapshot.changeTurnStatus(turnId: String, status: TurnStatus): ThreadSnapshot = copy(
    turns = turns.map { turn -> if (turn.id == turnId) turn.copy(status = status) else turn },
)

internal fun ThreadSnapshot.appendDelta(turnId: String, itemId: String, delta: String, kind: DeltaKind): ThreadSnapshot = copy(
    turns = turns.map { turn ->
        if (turn.id != turnId) turn else turn.copy(items = turn.items.map { item -> item.appendDelta(itemId, delta, kind) })
    },
)

internal enum class DeltaKind { AgentMessage, Reasoning, CommandOutput, FileChangeOutput }

private fun CodexItem.appendDelta(itemId: String, delta: String, kind: DeltaKind): CodexItem {
    if (id != itemId) return this
    return when (this) {
        is CodexItem.AgentMessage -> if (kind == DeltaKind.AgentMessage) copy(text = text + delta) else this
        is CodexItem.Reasoning -> if (kind == DeltaKind.Reasoning) copy(summary = summary + delta) else this
        is CodexItem.CommandExecution -> if (kind == DeltaKind.CommandOutput) copy(output = output + delta) else this
        is CodexItem.FileChange -> if (kind == DeltaKind.FileChangeOutput) copy(
            changes = changes.appendOutput(delta),
        ) else this
        is CodexItem.UserMessage, is CodexItem.Unknown -> this
    }
}

private fun List<FileUpdateChange>.appendOutput(delta: String): List<FileUpdateChange> = when {
    isEmpty() -> listOf(FileUpdateChange(path = "", kind = FileUpdateKind.Update, diff = delta))
    else -> dropLast(1) + last().copy(diff = last().diff + delta)
}

private fun <T> List<T>.replaceById(id: String, value: T, idOf: (T) -> String): List<T> {
    val index = indexOfFirst { idOf(it) == id }
    return if (index < 0) this + value else toMutableList().also { it[index] = value }
}

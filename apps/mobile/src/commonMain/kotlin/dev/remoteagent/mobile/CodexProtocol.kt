package dev.remoteagent.mobile

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject

private val rawCodexJson = Json { ignoreUnknownKeys = false; isLenient = false }

/**
 * The wire protocol exposed by Host is Codex JSON-RPC.  Keeping these values
 * untyped at this boundary means a newer Codex can add methods and payload
 * fields without requiring a mobile release just to carry them.
 */
sealed interface RawCodexMessage {
    val method: String
    val params: JsonElement

    data class Notification(
        override val method: String,
        override val params: JsonElement,
        val extensions: JsonObject = JsonObject(emptyMap()),
    ) : RawCodexMessage

    data class ServerRequest(
        val id: JsonElement,
        override val method: String,
        override val params: JsonElement,
        val extensions: JsonObject = JsonObject(emptyMap()),
    ) : RawCodexMessage
}

internal fun emptyJsonObject(): JsonObject = JsonObject(emptyMap())

internal fun JsonElement.asObjectOrNull(): JsonObject? = this as? JsonObject

internal fun JsonObject.string(name: String): String? = this[name]?.stringOrNull()

internal fun JsonObject.long(name: String): Long? = this[name]?.longOrNull()

internal fun JsonObject.int(name: String): Int? = long(name)?.toInt()

internal fun JsonObject.boolean(name: String): Boolean? = this[name]?.stringOrNull()?.toBooleanStrictOrNull()

internal fun JsonObject.childObject(name: String): JsonObject? = this[name]?.asObjectOrNull()

internal fun JsonObject.array(name: String): JsonArray? = this[name] as? JsonArray

internal fun JsonElement.stringOrNull(): String? = when (this) {
    JsonNull -> null
    is JsonPrimitive -> contentOrNull
    else -> null
}

internal fun JsonElement.longOrNull(): Long? = when (this) {
    is JsonPrimitive -> contentOrNull?.toLongOrNull()
    else -> null
}

internal fun JsonElement.doubleOrNull(): Double? = when (this) {
    is JsonPrimitive -> contentOrNull?.toDoubleOrNull()
    else -> null
}

internal fun JsonElement.jsonPrimitiveOrNull(): JsonPrimitive? = this as? JsonPrimitive

internal fun JsonElement.messageOrString(): String = when (this) {
    is JsonObject -> string("message") ?: toString()
    else -> stringOrNull() ?: toString()
}

internal fun JsonObject.textLike(): String {
    string("text")?.let { return it }
    string("summary")?.let { return it }
    string("message")?.let { return it }
    val content = array("content") ?: return ""
    return content.joinToString("") { part ->
        (part as? JsonObject)?.string("text") ?: part.stringOrNull().orEmpty()
    }
}

internal fun JsonObject.without(vararg names: String): JsonObject {
    val excluded = names.toSet()
    return JsonObject(filterKeys { it !in excluded })
}

/** Parse a native notification/request without interpreting its method name. */
internal fun parseRawCodexMessage(raw: String): RawCodexMessage? = runCatching {
    rawCodexJson.parseToJsonElement(raw).toRawCodexMessage()
}.getOrNull()

internal fun JsonElement.toRawCodexMessage(): RawCodexMessage? {
    val objectValue = asObjectOrNull() ?: return null
    val method = objectValue.string("method") ?: return null
    val params = objectValue["params"] ?: emptyJsonObject()
    val extensions = objectValue.without("id", "method", "params")
    val id = objectValue["id"]
    return if (id == null) {
        RawCodexMessage.Notification(method, params, extensions)
    } else {
        RawCodexMessage.ServerRequest(id, method, params, extensions)
    }
}

/** Convert a Codex thread object into the existing screen-facing projection. */
internal fun codexThreadSummary(value: JsonElement): ThreadSummary {
    val raw = value.asObjectOrNull() ?: emptyJsonObject()
    val id = raw.string("id").orEmpty()
    return ThreadSummary(
        id = id,
        name = raw.string("name"),
        preview = raw.string("preview").orEmpty(),
        workingDirectory = WorkingDirectory(raw.string("cwd").orEmpty()),
        projectId = raw.string("projectId"),
        createdAtMs = raw.long("createdAt") ?: 0L,
        updatedAtMs = raw.long("updatedAt") ?: 0L,
        status = codexThreadStatus(raw["status"]),
        raw = raw,
    )
}

internal fun codexProject(value: JsonElement): CodexProject {
    val raw = value.asObjectOrNull() ?: emptyJsonObject()
    return CodexProject(
        id = raw.string("id").orEmpty(),
        name = raw.string("name").orEmpty(),
        roots = raw.array("roots").orEmpty().mapNotNull { root ->
            root.asObjectOrNull()?.string("path")?.let(::WorkingDirectory)
        },
        position = raw.long("position") ?: 0L,
        createdAtMs = raw.long("createdAt") ?: 0L,
        updatedAtMs = raw.long("updatedAt") ?: 0L,
        raw = raw,
    )
}

internal fun codexThreadSnapshot(value: JsonElement): ThreadSnapshot {
    val raw = value.asObjectOrNull() ?: emptyJsonObject()
    val turns = raw.array("turns").orEmpty().map(::codexTurn)
    return ThreadSnapshot(codexThreadSummary(raw), turns, raw)
}

internal fun codexThreadFromResponse(value: JsonElement): ThreadSnapshot =
    codexThreadSnapshot(value.asObjectOrNull()?.get("thread") ?: JsonNull)

internal fun codexTurn(value: JsonElement): CodexTurn {
    val raw = value.asObjectOrNull() ?: emptyJsonObject()
    return CodexTurn(
        id = raw.string("id").orEmpty(),
        status = codexTurnStatus(raw.string("status")),
        items = raw.array("items").orEmpty().map(::codexItem),
        raw = raw,
        startedAtMs = unixSecondsToMilliseconds(raw.long("startedAt")),
        completedAtMs = unixSecondsToMilliseconds(raw.long("completedAt")),
        durationMs = nonNegative(raw.long("durationMs")),
        error = raw.childObject("error")?.let(::codexTurnError),
    )
}

internal fun codexItem(value: JsonElement): CodexItem {
    val raw = value.asObjectOrNull() ?: JsonObject(mapOf("value" to value))
    val id = raw.string("id").orEmpty()
    return when (raw.string("type")) {
        "userMessage" -> CodexItem.UserMessage(id, raw.userMessageText(), raw.string("clientId"), raw.userMessageImages())
        "agentMessage" -> CodexItem.AgentMessage(id, raw.textLike(), agentMessagePhase(raw.string("phase")))
        "reasoning" -> CodexItem.Reasoning(id, raw.textLike())
        "commandExecution" -> CodexItem.CommandExecution(
            id = id,
            command = raw.string("command").orEmpty(),
            cwd = raw.string("cwd"),
            output = raw.string("aggregatedOutput").orEmpty(),
            status = codexCommandStatus(raw.string("status")),
            exitCode = raw.int("exitCode"),
        )
        "fileChange" -> CodexItem.FileChange(
            id = id,
            changes = raw.array("changes").orEmpty().mapNotNull(::codexFileChange),
            status = codexFileChangeStatus(raw.string("status")),
        )
        else -> CodexItem.Unknown(
            id = id,
            codexType = raw.string("type") ?: "unknown",
            raw = raw,
        )
    }
}

private fun JsonObject.userMessageImages(): List<String> = array("content").orEmpty().mapNotNull { value ->
    val part = value as? JsonObject ?: return@mapNotNull null
    when (part.string("type")) {
        "localImage" -> part.string("path")
        "image" -> part.string("url")
        else -> null
    }?.takeIf { it.isNotBlank() }
}

private fun JsonObject.userMessageText(): String = buildString {
    append(textLike())
    array("content").orEmpty().forEach { value ->
        val part = value as? JsonObject ?: return@forEach
        val label = when (part.string("type")) {
            "mention" -> attachmentMessageLabel(false, part.string("path").orEmpty(), part.string("name").orEmpty())
            else -> return@forEach
        }
        if (isNotEmpty()) append('\n')
        append(label)
    }
}

private fun codexFileChange(value: JsonElement): FileUpdateChange? {
    val raw = value.asObjectOrNull() ?: return null
    val kind = when (raw.childObject("kind")?.string("type")) {
        "add" -> FileUpdateKind.Add
        "delete" -> FileUpdateKind.Delete
        else -> FileUpdateKind.Update
    }
    return FileUpdateChange(raw.string("path").orEmpty(), kind, raw.string("diff").orEmpty())
}

internal fun codexThreadEvent(
    method: String,
    params: JsonElement,
    extensions: JsonObject = emptyJsonObject(),
): ThreadEvent {
    val raw = params.asObjectOrNull() ?: JsonObject(mapOf("value" to params))
    val threadId = raw.string("threadId").orEmpty()
    return when (method) {
        "turn/started" -> ThreadEvent.TurnStarted(
            threadId,
            raw.childObject("turn")?.string("id").orEmpty(),
            TurnStatus.InProgress,
            unixSecondsToMilliseconds(raw.childObject("turn")?.long("startedAt")),
        )
        "turn/completed" -> raw.childObject("turn").let { turn ->
            ThreadEvent.TurnCompleted(
                threadId,
                turn?.string("id").orEmpty(),
                codexTurnStatus(turn?.string("status")),
                unixSecondsToMilliseconds(turn?.long("startedAt")),
                unixSecondsToMilliseconds(turn?.long("completedAt")),
                nonNegative(turn?.long("durationMs")),
                turn?.childObject("error")?.let(::codexTurnError),
            )
        }
        "item/started" -> ThreadEvent.ItemStarted(
            threadId,
            raw.string("turnId").orEmpty(),
            codexItem(raw["item"] ?: emptyJsonObject()),
        )
        "item/completed" -> ThreadEvent.ItemCompleted(
            threadId,
            raw.string("turnId").orEmpty(),
            codexItem(raw["item"] ?: emptyJsonObject()),
        )
        "item/agentMessage/delta" -> ThreadEvent.AgentMessageDelta(
            threadId,
            raw.string("turnId").orEmpty(),
            eventItemId(raw),
            eventDelta(raw),
        )
        "item/reasoning/textDelta" -> ThreadEvent.ReasoningDelta(
            threadId,
            raw.string("turnId").orEmpty(),
            eventItemId(raw),
            eventDelta(raw),
        )
        "item/reasoning/summaryTextDelta" ->
            ThreadEvent.ReasoningSummaryDelta(
                threadId,
                raw.string("turnId").orEmpty(),
                eventItemId(raw),
                eventDelta(raw),
            )
        "item/commandExecution/outputDelta" -> ThreadEvent.CommandOutputDelta(
            threadId,
            raw.string("turnId").orEmpty(),
            eventItemId(raw),
            eventDelta(raw),
        )
        "error" -> ThreadEvent.Error(
            threadId = threadId,
            turnId = raw.string("turnId").orEmpty(),
            error = codexTurnError(raw.childObject("error") ?: emptyJsonObject()).copy(
                willRetry = raw.boolean("willRetry") == true,
            ),
            willRetry = raw.boolean("willRetry") == true,
        )
        "serverRequest/resolved" -> ThreadEvent.RequestResolved(
            threadId = threadId,
            requestId = raw["requestId"]?.stringOrNull() ?: raw["requestId"].toString(),
        )
        "thread/status/changed" -> ThreadEvent.ThreadStatusChanged(
            threadId = threadId,
            status = codexThreadStatus(raw["status"]),
        )
        "item/autoApprovalReview/started", "item/autoApprovalReview/completed" ->
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

internal fun codexThreadEvent(message: RawCodexMessage): ThreadEvent = when (message) {
    is RawCodexMessage.Notification -> codexThreadEvent(message.method, message.params, message.extensions)
    is RawCodexMessage.ServerRequest -> {
        val params = message.params.asObjectOrNull() ?: JsonObject(mapOf("value" to message.params))
        ThreadEvent.RequestStarted(
            threadId = params.string("threadId").orEmpty(),
            turnId = params.string("turnId").orEmpty(),
            request = CodexServerRequest(
                id = message.id.stringOrNull() ?: message.id.toString(),
                method = message.method,
                params = params,
                wireId = message.id,
            ),
        )
    }
}

private fun codexTurnError(raw: JsonObject): CodexTurnError = CodexTurnError(
    message = raw.string("message").orEmpty(),
    additionalDetails = raw.string("additionalDetails"),
    codexErrorInfo = raw["codexErrorInfo"]?.takeUnless { it is JsonNull },
)

private fun eventItemId(raw: JsonObject): String = raw.string("itemId").orEmpty()

private fun eventDelta(raw: JsonObject): String = raw.string("delta").orEmpty()

private fun codexThreadStatus(value: JsonElement?): ThreadStatus {
    val raw = value?.asObjectOrNull()
    return when (raw?.string("type")) {
        "active" -> ThreadStatus.Active(raw.array("activeFlags").orEmpty().mapNotNull(JsonElement::stringOrNull))
        "systemError" -> ThreadStatus.SystemError
        "notLoaded" -> ThreadStatus.NotLoaded
        else -> ThreadStatus.Idle
    }
}

private fun codexTurnStatus(status: String?): TurnStatus = when (status) {
    "inProgress" -> TurnStatus.InProgress
    "interrupted" -> TurnStatus.Interrupted
    "failed" -> TurnStatus.Failed
    else -> TurnStatus.Completed
}

private fun agentMessagePhase(phase: String?): AgentMessagePhase? = when (phase) {
    "commentary" -> AgentMessagePhase.Commentary
    "final_answer" -> AgentMessagePhase.FinalAnswer
    else -> null
}

private fun nonNegative(value: Long?): Long? = value?.takeIf { it >= 0 }

private fun unixSecondsToMilliseconds(value: Long?): Long? {
    val seconds = nonNegative(value) ?: return null
    return if (seconds > Long.MAX_VALUE / 1_000) Long.MAX_VALUE else seconds * 1_000
}

private fun codexCommandStatus(status: String?): CommandExecutionStatus = when (status) {
    "inProgress" -> CommandExecutionStatus.InProgress
    "failed" -> CommandExecutionStatus.Failed
    "declined" -> CommandExecutionStatus.Declined
    else -> CommandExecutionStatus.Completed
}

private fun codexFileChangeStatus(status: String?): FileChangeStatus = when (status) {
    "inProgress" -> FileChangeStatus.InProgress
    "failed" -> FileChangeStatus.Failed
    "declined" -> FileChangeStatus.Declined
    else -> FileChangeStatus.Completed
}

internal fun attachmentMessageLabel(isImage: Boolean, path: String, name: String): String =
    if (isImage) "画像: $path" else "添付: $name ($path)"

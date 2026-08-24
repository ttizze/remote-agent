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

internal fun JsonObject.nestedString(vararg path: String): String? {
    var value: JsonElement = this
    for (name in path) {
        value = (value as? JsonObject)?.get(name) ?: return null
    }
    return value.stringOrNull()
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
        workingDirectory = WorkingDirectory(raw.string("cwd") ?: raw.nestedString("workingDirectory", "path").orEmpty()),
        createdAtMs = raw.long("createdAt") ?: raw.long("createdAtMs") ?: 0L,
        updatedAtMs = raw.long("updatedAt") ?: raw.long("updatedAtMs") ?: 0L,
        status = codexThreadStatus(raw["status"]),
        raw = raw,
    )
}

internal fun codexThreadSnapshot(value: JsonElement): ThreadSnapshot {
    val raw = value.asObjectOrNull() ?: emptyJsonObject()
    val turns = raw.array("turns").orEmpty().map(::codexTurn)
    return ThreadSnapshot(codexThreadSummary(raw), turns, raw)
}

internal fun codexThreadFromResponse(value: JsonElement): ThreadSnapshot =
    codexThreadSnapshot(value.asObjectOrNull()?.get("thread") ?: value)

internal fun codexTurn(value: JsonElement): CodexTurn {
    val raw = value.asObjectOrNull() ?: emptyJsonObject()
    val statusName = raw.string("status") ?: raw.childObject("status")?.string("type")
    return CodexTurn(
        id = raw.string("id") ?: raw.string("turnId").orEmpty(),
        status = codexTurnStatus(statusName),
        items = raw.array("items").orEmpty().map(::codexItem),
        raw = raw,
    )
}

internal fun codexItem(value: JsonElement): CodexItem {
    val raw = value.asObjectOrNull() ?: JsonObject(mapOf("value" to value))
    val id = raw.string("id").orEmpty()
    return when (raw.string("type")) {
        "userMessage" -> CodexItem.UserMessage(id, raw.textLike())
        "agentMessage" -> CodexItem.AgentMessage(id, raw.textLike())
        "reasoning" -> CodexItem.Reasoning(id, raw.textLike())
        "commandExecution" -> CodexItem.CommandExecution(
            id = id,
            command = raw.string("command").orEmpty(),
            cwd = raw.string("cwd"),
            output = raw.string("aggregatedOutput") ?: raw.string("output").orEmpty(),
            status = codexCommandStatus(raw.string("status") ?: raw.childObject("status")?.string("type")),
            exitCode = raw.int("exitCode"),
        )
        "fileChange" -> CodexItem.FileChange(
            id = id,
            changes = raw.array("changes").orEmpty().mapNotNull(::codexFileChange),
            status = codexFileChangeStatus(raw.string("status") ?: raw.childObject("status")?.string("type")),
        )
        else -> CodexItem.Unknown(
            id = id,
            codexType = raw.string("type") ?: "unknown",
            raw = raw,
        )
    }
}

private fun codexFileChange(value: JsonElement): FileUpdateChange? {
    val raw = value.asObjectOrNull() ?: return null
    val kind = when (raw.string("kind") ?: raw.string("type")) {
        "add", "create" -> FileUpdateKind.Add
        "delete", "remove" -> FileUpdateKind.Delete
        else -> FileUpdateKind.Update
    }
    return FileUpdateChange(raw.string("path").orEmpty(), kind, raw.string("diff") ?: raw.string("patch").orEmpty())
}

internal fun codexThreadEvent(
    method: String,
    params: JsonElement,
    extensions: JsonObject = emptyJsonObject(),
): ThreadEvent {
    val raw = params.asObjectOrNull() ?: JsonObject(mapOf("value" to params))
    val threadId = raw.string("threadId") ?: raw.nestedString("thread", "id").orEmpty()
    val turnId = raw.string("turnId") ?: raw.nestedString("turn", "id").orEmpty()
    return when (method) {
        "turn/started" -> ThreadEvent.TurnStarted(threadId, turnId, TurnStatus.InProgress)
        "turn/completed" -> ThreadEvent.TurnCompleted(
            threadId,
            turnId,
            codexTurnStatus(statusName(raw.childObject("turn")?.get("status") ?: raw["status"])),
        )
        "item/started" -> ThreadEvent.ItemStarted(threadId, turnId, codexItem(raw["item"] ?: emptyJsonObject()))
        "item/completed" -> ThreadEvent.ItemCompleted(threadId, turnId, codexItem(raw["item"] ?: emptyJsonObject()))
        "item/agentMessage/delta" -> ThreadEvent.AgentMessageDelta(threadId, turnId, eventItemId(raw), eventDelta(raw))
        "item/reasoning/textDelta" -> ThreadEvent.ReasoningDelta(threadId, turnId, eventItemId(raw), eventDelta(raw))
        "item/reasoning/summaryDelta", "item/reasoning/summaryTextDelta" ->
            ThreadEvent.ReasoningSummaryDelta(threadId, turnId, eventItemId(raw), eventDelta(raw))
        "item/commandExecution/outputDelta" -> ThreadEvent.CommandOutputDelta(threadId, turnId, eventItemId(raw), eventDelta(raw))
        "item/fileChange/outputDelta", "item/fileChange/patchUpdated" ->
            ThreadEvent.FileChangeOutputDelta(threadId, turnId, eventItemId(raw), eventDelta(raw))
        else -> ThreadEvent.Unknown(threadId, turnId, method, raw, extensions)
    }
}

private fun eventItemId(raw: JsonObject): String = raw.string("itemId") ?: raw.nestedString("item", "id").orEmpty()

private fun eventDelta(raw: JsonObject): String = raw.string("delta") ?: raw.string("text") ?: raw.string("output").orEmpty()

private fun codexThreadStatus(value: JsonElement?): ThreadStatus {
    val raw = value?.asObjectOrNull()
    return when (statusName(value)) {
        "active", "inProgress", "running" -> ThreadStatus.Active(raw?.array("activeFlags").orEmpty().mapNotNull(JsonElement::stringOrNull))
        "systemError", "error" -> ThreadStatus.SystemError
        "notLoaded" -> ThreadStatus.NotLoaded
        else -> ThreadStatus.Idle
    }
}

private fun statusName(value: JsonElement?): String? = when (value) {
    is JsonPrimitive -> value.contentOrNull
    is JsonObject -> value.string("type") ?: value.string("status")
    else -> null
}

private fun codexTurnStatus(status: String?): TurnStatus = when (status) {
    "inProgress", "started", "active", "running" -> TurnStatus.InProgress
    "interrupted", "cancelled", "canceled" -> TurnStatus.Interrupted
    "failed", "error" -> TurnStatus.Failed
    else -> TurnStatus.Completed
}

private fun codexCommandStatus(status: String?): CommandExecutionStatus = when (status) {
    "inProgress", "started", "active", "running" -> CommandExecutionStatus.InProgress
    "failed", "error" -> CommandExecutionStatus.Failed
    "declined", "rejected" -> CommandExecutionStatus.Declined
    else -> CommandExecutionStatus.Completed
}

private fun codexFileChangeStatus(status: String?): FileChangeStatus = when (status) {
    "inProgress", "started", "active", "running" -> FileChangeStatus.InProgress
    "failed", "error" -> FileChangeStatus.Failed
    "declined", "rejected" -> FileChangeStatus.Declined
    else -> FileChangeStatus.Completed
}

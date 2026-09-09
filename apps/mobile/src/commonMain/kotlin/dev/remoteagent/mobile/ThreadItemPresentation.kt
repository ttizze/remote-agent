package dev.remoteagent.mobile

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put

internal data class ThreadItemPresentation(
    val id: String,
    val kind: String,
    val title: String,
    val collapsedBody: String,
    val isCollapsible: Boolean,
    val isVisible: Boolean = true,
)

internal fun CodexItem.toThreadItemPresentation(): ThreadItemPresentation {
    val request = buildJsonObject {
        put("operation", "item")
        put("item", presentationMetadata(title = true))
    }
    val native =
        presentationJson.decodeFromString<NativeItemPresentation>(nativeConversationPresentation(request.toString()))
    val body =
        when (this) {
            is CodexItem.UserMessage -> text
            is CodexItem.AgentMessage -> text
            is CodexItem.Reasoning -> "詳細を表示"
            is CodexItem.CommandExecution -> status.name
            is CodexItem.FileChange -> status.name
            is CodexItem.Unknown -> if (codexType == "imageGeneration") native.title else raw.text("status") ?: "詳細を表示"
        }
    return ThreadItemPresentation(id, native.kind, native.title, body, native.collapsible, native.visible)
}

internal fun CodexItem.expandedThreadItemBody(): String =
    when (this) {
        is CodexItem.UserMessage -> text
        is CodexItem.AgentMessage -> text
        is CodexItem.Reasoning -> summary
        is CodexItem.CommandExecution -> buildString {
                cwd?.takeIf(String::isNotBlank)?.let { append("cwd: ").append(it).append('\n') }
                append(output)
            }
                .trim()
        is CodexItem.FileChange ->
            changes.joinToString("\n\n") { change -> "${change.kind.name}: ${change.path}\n${change.diff}".trim() }
        is CodexItem.Unknown -> raw.toString()
    }

internal fun CodexItem.threadItemContentVersion(): String =
    when (this) {
        is CodexItem.UserMessage -> "user:${text.length}:${imageSources.hashCode()}"
        is CodexItem.AgentMessage -> "agent:${text.length}"
        is CodexItem.Reasoning -> "reasoning:${summary.length}"
        is CodexItem.CommandExecution -> "command:${status.name}:${command.length}:${output.length}"
        is CodexItem.FileChange -> "files:${status.name}:${changes.sumOf { it.path.length + it.diff.length }}"
        is CodexItem.Unknown -> "unknown:${raw.hashCode()}"
    }

internal fun JsonObject.text(name: String): String? = get(name)?.jsonPrimitive?.contentOrNull

internal val presentationJson = Json { ignoreUnknownKeys = true }

@Serializable
private data class NativeItemPresentation(
    val kind: String,
    val title: String,
    val collapsible: Boolean,
    val visible: Boolean,
)

/** Only fields consumed by Rust presentation policy cross the language boundary. */
internal fun CodexItem.presentationMetadata(title: Boolean = false): JsonObject = buildJsonObject {
    put("id", id)
    when (val item = this@presentationMetadata) {
        is CodexItem.UserMessage -> {
            put("type", "userMessage")
            item.clientId?.let { put("clientId", it) }
        }
        is CodexItem.AgentMessage -> {
            put("type", "agentMessage")
            item.phase?.let { put("phase", presentationJson.encodeToJsonElement(AgentMessagePhase.serializer(), it)) }
        }
        is CodexItem.Reasoning -> put("type", "reasoning")
        is CodexItem.CommandExecution -> {
            put("type", "commandExecution")
            if (title) put("command", item.command)
        }
        is CodexItem.FileChange -> {
            put("type", "fileChange")
            put("fileCount", item.changes.size)
        }
        is CodexItem.Unknown -> {
            put("type", item.codexType)
            if (title) {
                for (key in listOf("server", "tool", "query", "path", "status")) item.raw[key]?.let { put(key, it) }
                (item.raw["review"] as? JsonObject)?.get("status")?.let { status ->
                    put("review", buildJsonObject { put("status", status) })
                }
            }
        }
    }
}

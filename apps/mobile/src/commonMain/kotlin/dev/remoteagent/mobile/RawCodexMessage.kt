package dev.remoteagent.mobile

import kotlinx.serialization.Serializable
import kotlinx.serialization.Transient
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject

/**
 * The wire protocol exposed by Host is Codex JSON-RPC. Keeping these values untyped at this boundary means a newer
 * Codex can add methods and payload fields without requiring a mobile release just to carry them.
 */
@Serializable
data class RawCodexMessage(val raw: JsonObject) {
    @Transient val method: String = raw.string("method").orEmpty()
    @Transient val params: JsonElement = raw["params"] ?: emptyJsonObject()
    @Transient val id: String = raw["id"]?.let { it.stringOrNull() ?: it.toString() }.orEmpty()
    @Transient
    val kind: ConversationEventKind =
        if ("id" in raw) ConversationEventKind.RequestStarted
        else ConversationEventKind.entries[nativeClassifyEvent(method)]

    val threadId: String
        get() = params.asObjectOrNull()?.string("threadId").orEmpty()

    val turnId: String
        get() =
            params
                .asObjectOrNull()
                ?.let { value ->
                    value.childObject("turn")?.string("id")?.takeIf(String::isNotEmpty) ?: value.string("turnId")
                }
                .orEmpty()
}

/** Parse a native notification/request without interpreting its method name. */
internal fun parseRawCodexMessage(raw: String): RawCodexMessage? = runCatching {
    Json.parseToJsonElement(raw).asObjectOrNull()?.takeIf { it.string("method") != null }?.let(::RawCodexMessage)
}
    .getOrNull()

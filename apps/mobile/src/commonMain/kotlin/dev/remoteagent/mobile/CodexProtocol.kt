package dev.remoteagent.mobile

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject

private val rawCodexJson = Json {
    ignoreUnknownKeys = false
    isLenient = false
}

/**
 * The wire protocol exposed by Host is Codex JSON-RPC. Keeping these values untyped at this boundary means a newer
 * Codex can add methods and payload fields without requiring a mobile release just to carry them.
 */
sealed interface RawCodexMessage {
    val method: String
    val params: JsonElement
    val kind: ConversationEventKind

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

    data class Notification(
        override val method: String,
        override val params: JsonElement,
        val extensions: JsonObject = JsonObject(emptyMap()),
    ) : RawCodexMessage {
        override val kind = ConversationEventKind.entries[nativeClassifyEvent(method)]
    }

    data class ServerRequest(
        val id: JsonElement,
        override val method: String,
        override val params: JsonElement,
        val extensions: JsonObject = JsonObject(emptyMap()),
    ) : RawCodexMessage {
        override val kind = ConversationEventKind.RequestStarted
    }
}

/** Parse a native notification/request without interpreting its method name. */
internal fun parseRawCodexMessage(raw: String): RawCodexMessage? = runCatching {
    rawCodexJson.parseToJsonElement(raw).toRawCodexMessage()
}
    .getOrNull()

internal fun JsonElement.toRawCodexMessage(): RawCodexMessage? {
    val objectValue = asObjectOrNull() ?: return null
    return objectValue.string("method")?.let { method ->
        val params = objectValue["params"] ?: emptyJsonObject()
        val extensions = objectValue.without("id", "method", "params")
        val id = objectValue["id"]
        if (id == null) {
            RawCodexMessage.Notification(method, params, extensions)
        } else {
            RawCodexMessage.ServerRequest(id, method, params, extensions)
        }
    }
}

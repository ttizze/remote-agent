package dev.remoteagent.mobile

import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

internal fun CodexServerRequest.toThreadRequestPresentation(): ThreadRequestPresentation {
    val input = buildJsonObject {
        put("operation", "request")
        put("method", method)
        put("params", params)
    }
    return presentationJson
        .decodeFromString<ThreadRequestPresentation>(nativeConversationPresentation(input.toString()))
        .copy(id = id)
}

internal fun CodexTurnError.toThreadErrorPresentation(status: TurnStatus): ThreadErrorPresentation {
    val input = buildJsonObject {
        put("operation", "error")
        put("error", presentationJson.encodeToJsonElement(CodexTurnError.serializer(), this@toThreadErrorPresentation))
        put("status", presentationJson.encodeToJsonElement(TurnStatus.serializer(), status))
    }
    return presentationJson.decodeFromString(nativeConversationPresentation(input.toString()))
}

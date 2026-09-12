package dev.remoteagent.mobile

import dev.remoteagent.core.JsonValue
import dev.remoteagent.core.formatJsonValue

internal val JsonValue.text: String?
    get() = when (this) {
        is JsonValue.String -> value
        is JsonValue.Number -> value
        else -> null
    }
internal val JsonValue.values: List<JsonValue>
    get() = (this as? JsonValue.Array)?.values.orEmpty()
internal operator fun JsonValue.get(key: String): JsonValue? = (this as? JsonValue.Object)?.fields?.get(key)
internal fun JsonValue.pretty(): String = formatJsonValue(this)

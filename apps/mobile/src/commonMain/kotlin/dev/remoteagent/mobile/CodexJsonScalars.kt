package dev.remoteagent.mobile

import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull

internal fun JsonElement.stringOrNull(): String? =
    when (this) {
        JsonNull -> null
        is JsonPrimitive -> contentOrNull
        else -> null
    }

internal fun JsonElement.longOrNull(): Long? =
    when (this) {
        is JsonPrimitive -> contentOrNull?.toLongOrNull()
        else -> null
    }

internal fun JsonElement.doubleOrNull(): Double? =
    when (this) {
        is JsonPrimitive -> contentOrNull?.toDoubleOrNull()
        else -> null
    }

internal fun JsonElement.jsonPrimitiveOrNull(): JsonPrimitive? = this as? JsonPrimitive

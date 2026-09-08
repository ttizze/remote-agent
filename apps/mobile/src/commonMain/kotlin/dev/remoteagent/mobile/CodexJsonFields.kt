package dev.remoteagent.mobile

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject

internal fun emptyJsonObject(): JsonObject = JsonObject(emptyMap())

internal fun JsonElement.asObjectOrNull(): JsonObject? = this as? JsonObject

internal fun JsonObject.string(name: String): String? = this[name]?.stringOrNull()

internal fun JsonObject.long(name: String): Long? = this[name]?.longOrNull()

internal fun JsonObject.int(name: String): Int? = long(name)?.toInt()

internal fun JsonObject.boolean(name: String): Boolean? = this[name]?.stringOrNull()?.toBooleanStrictOrNull()

internal fun JsonObject.childObject(name: String): JsonObject? = this[name]?.asObjectOrNull()

internal fun JsonObject.array(name: String): JsonArray? = this[name] as? JsonArray

internal fun JsonObject.without(vararg names: String): JsonObject {
    val excluded = names.toSet()
    return JsonObject(filterKeys { it !in excluded })
}

/** Keep metadata and unknown extensions without retaining an already typed collection. */
internal fun JsonObject.without(key: String): JsonObject = if (key in this) JsonObject(this - key) else this

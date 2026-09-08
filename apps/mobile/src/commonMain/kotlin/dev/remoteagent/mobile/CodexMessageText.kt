package dev.remoteagent.mobile

import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject

internal fun JsonElement.messageOrString(): String =
    when (this) {
        is JsonObject -> string("message") ?: toString()
        else -> stringOrNull() ?: toString()
    }

internal fun JsonObject.textLike(): String =
    string("text")
        ?: string("summary")
        ?: string("message")
        ?: array("content")
            ?.joinToString("") { part -> (part as? JsonObject)?.string("text") ?: part.stringOrNull().orEmpty() }
            .orEmpty()

internal fun JsonObject.userMessageImages(): List<String> =
    array("content").orEmpty().mapNotNull { value ->
        val part = value as? JsonObject ?: return@mapNotNull null
        when (part.string("type")) {
            "localImage" -> part.string("path")
            "image" -> part.string("url")
            else -> null
        }?.takeIf { it.isNotBlank() }
    }

internal fun JsonObject.userMessageText(): String = buildString {
    append(textLike())
    array("content").orEmpty().forEach { value ->
        val part = value as? JsonObject ?: return@forEach
        val label =
            when (part.string("type")) {
                "mention" -> attachmentMessageLabel(false, part.string("path").orEmpty(), part.string("name").orEmpty())
                else -> return@forEach
            }
        if (isNotEmpty()) append('\n')
        append(label)
    }
}

internal fun attachmentMessageLabel(isImage: Boolean, path: String, name: String): String =
    if (isImage) "画像: $path" else "添付: $name ($path)"

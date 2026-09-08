package dev.remoteagent.mobile

import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject

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
        raw = raw.without("turns"),
    )
}

internal fun codexProject(value: JsonElement): CodexProject {
    val raw = value.asObjectOrNull() ?: emptyJsonObject()
    return CodexProject(
        id = raw.string("id").orEmpty(),
        name = raw.string("name").orEmpty(),
        roots =
            raw.array("roots").orEmpty().mapNotNull { root ->
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
    val metadata = raw.without("turns")
    return ThreadSnapshot(codexThreadSummary(metadata), turns, metadata)
}

internal fun codexThreadFromResponse(value: JsonElement): ThreadSnapshot =
    codexThreadSnapshot(value.asObjectOrNull()?.get("thread") ?: JsonNull)

internal fun codexTurn(value: JsonElement): CodexTurn {
    val raw = value.asObjectOrNull() ?: emptyJsonObject()
    return CodexTurn(
        id = raw.string("id")?.takeIf(String::isNotBlank) ?: raw.string("turnId").orEmpty(),
        status = codexTurnStatus(raw.string("status")),
        items = raw.array("items").orEmpty().map(::codexItem),
        raw = raw.without("items"),
        startedAtMs = unixSecondsToMilliseconds(raw.long("startedAt")),
        completedAtMs = unixSecondsToMilliseconds(raw.long("completedAt")),
        durationMs = nonNegative(raw.long("durationMs")),
        error = raw.childObject("error")?.let(::codexTurnError),
    )
}

internal fun codexTurnError(raw: JsonObject): CodexTurnError =
    CodexTurnError(
        message = raw.string("message").orEmpty(),
        additionalDetails = raw.string("additionalDetails"),
        codexErrorInfo = raw["codexErrorInfo"]?.takeUnless { it is JsonNull },
    )

internal fun codexThreadStatus(value: JsonElement?): ThreadStatus {
    val raw = value?.asObjectOrNull()
    return when (raw?.string("type")) {
        "active" -> ThreadStatus.Active(raw.array("activeFlags").orEmpty().mapNotNull(JsonElement::stringOrNull))
        "systemError" -> ThreadStatus.SystemError
        "notLoaded" -> ThreadStatus.NotLoaded
        else -> ThreadStatus.Idle
    }
}

internal fun codexTurnStatus(status: String?): TurnStatus =
    when (status) {
        "inProgress" -> TurnStatus.InProgress
        "interrupted" -> TurnStatus.Interrupted
        "failed" -> TurnStatus.Failed
        else -> TurnStatus.Completed
    }

internal fun nonNegative(value: Long?): Long? = value?.takeIf { it >= 0 }

internal fun unixSecondsToMilliseconds(value: Long?): Long? {
    val seconds = nonNegative(value) ?: return null
    return if (seconds > Long.MAX_VALUE / MILLISECONDS_PER_SECOND) Long.MAX_VALUE else seconds * MILLISECONDS_PER_SECOND
}

private const val MILLISECONDS_PER_SECOND = 1_000L

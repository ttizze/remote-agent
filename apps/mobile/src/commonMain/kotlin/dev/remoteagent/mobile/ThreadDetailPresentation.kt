package dev.remoteagent.mobile

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

internal data class ThreadTurnPresentation(
    val id: String,
    val turnId: String,
    val isLastSegment: Boolean,
    val status: TurnStatus,
    val userMessages: List<CodexItem.UserMessage>,
    val activityItems: List<CodexItem>,
    val responses: List<CodexItem>,
    val activitySummary: String?,
    val activityInitiallyExpanded: Boolean,
    val activityCanCollapse: Boolean,
    val error: ThreadErrorPresentation?,
    val pendingRequests: List<ThreadRequestPresentation>,
)

internal data class ThreadErrorPresentation(
    val title: String,
    val message: String,
    val details: String?,
    val isReconnecting: Boolean,
    val isRetryable: Boolean,
)

internal data class ThreadRequestPresentation(val id: String, val kind: String, val title: String, val body: String)

/** Rust returns source indices; Kotlin keeps ownership of all message bodies. */
internal fun ThreadSnapshot.conversationSegments(): List<ThreadTurnPresentation> = turns.flatMap { turn ->
    turn.toThreadTurnPresentations(submittedMessages.filter { it.turnId == turn.id })
}

internal fun CodexTurn.toThreadTurnPresentations(
    submissions: List<SubmittedMessage> = emptyList()
): List<ThreadTurnPresentation> {
    val request = presentationRequest(submissions)
    val segments =
        presentationJson.decodeFromString<List<NativeConversationSegment>>(
            nativeConversationPresentation(request.toString())
        )
    return segments.map { segment ->
        val users = mutableListOf<CodexItem.UserMessage>()
        val activities = mutableListOf<CodexItem>()
        val responses = mutableListOf<CodexItem>()
        for (row in segment.rows) {
            val item =
                if (row.source < items.size) items[row.source]
                else
                    submissions[row.source - items.size].let {
                        CodexItem.UserMessage(it.clientId, it.text, it.clientId, it.imageSources)
                    }
            when (row.role) {
                "user" -> users += item as CodexItem.UserMessage
                "activity" -> activities += item
                "response" -> responses += item
                "hidden" -> Unit
                else -> error("Unknown conversation row role: ${row.role}")
            }
        }
        ThreadTurnPresentation(
            segment.id,
            id,
            segment.last,
            status,
            users,
            activities,
            responses,
            segment.label,
            segment.initiallyExpanded,
            segment.collapsible,
            if (segment.last) error?.toThreadErrorPresentation(status) else null,
            if (segment.last) pendingRequests.map(CodexServerRequest::toThreadRequestPresentation) else emptyList(),
        )
    }
}

internal expect fun nativeConversationPresentation(request: String): String

@Serializable private data class NativeConversationRow(val source: Int, val role: String)

@Serializable
private data class NativeConversationSegment(
    val id: String,
    val last: Boolean,
    val collapsible: Boolean,
    val initiallyExpanded: Boolean,
    val label: String?,
    val rows: List<NativeConversationRow>,
)

internal fun retainPendingSubmissions(
    pending: List<SubmittedMessage>,
    echoed: Sequence<String>,
): List<SubmittedMessage> {
    if (pending.isEmpty()) return pending
    val request = buildJsonObject {
        put("operation", "reconcile")
        put("pending", JsonArray(pending.map { JsonPrimitive(it.clientId) }))
        put("echoed", JsonArray(echoed.map { JsonPrimitive(it) }.toList()))
    }
    val retained = presentationJson.decodeFromString<List<Int>>(nativeConversationPresentation(request.toString()))
    return if (retained.size == pending.size) pending else retained.map { pending[it] }
}

private fun CodexTurn.presentationRequest(submissions: List<SubmittedMessage>): JsonObject {
    return buildJsonObject {
        put("operation", "turn")
        put(
            "turn",
            buildJsonObject {
                put("id", id)
                put("status", presentationJson.encodeToJsonElement(TurnStatus.serializer(), status))
                durationMs?.let { put("durationMs", it) }
                startedAtMs?.let { put("startedAtMs", it) }
                completedAtMs?.let { put("completedAtMs", it) }
                put("items", JsonArray(items.map { it.presentationMetadata() }))
            },
        )
        put(
            "pending",
            JsonArray(
                submissions.map { pending ->
                    buildJsonObject {
                        pending.afterItemId?.let { put("afterItemId", it) }
                        put(
                            "item",
                            buildJsonObject {
                                put("id", pending.clientId)
                                put("clientId", pending.clientId)
                                put("type", "userMessage")
                            },
                        )
                    }
                }
            ),
        )
    }
}

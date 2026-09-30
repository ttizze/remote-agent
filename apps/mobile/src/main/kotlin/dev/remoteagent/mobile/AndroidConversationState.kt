package dev.remoteagent.mobile

import dev.remoteagent.core.ConversationRow
import dev.remoteagent.core.JsonValue
import dev.remoteagent.core.RenderedConversation
import dev.remoteagent.core.RenderedItem
import dev.remoteagent.core.RenderedTurn
import dev.remoteagent.core.Snapshot
import dev.remoteagent.core.Thread
import dev.remoteagent.core.projectConversation

internal val JsonValue.text: String?
    get() =
        when (this) {
            is JsonValue.String -> value
            is JsonValue.Number -> value
            else -> null
        }
internal val JsonValue.values: List<JsonValue>
    get() = (this as? JsonValue.Array)?.values.orEmpty()

internal operator fun JsonValue.get(key: String): JsonValue? = (this as? JsonValue.Object)?.fields?.get(key)

/** Previous values are inputs; projecting rows never mutates a published result. */
internal data class ConversationProjection(
    val source: RenderedConversation,
    val turns: Map<String, Pair<RenderedTurn, List<ConversationRow>>>,
    val rows: List<ConversationRow>,
    val queued: List<RenderedItem>,
    val requestRows: List<ConversationRow>,
)

internal fun projectConversationRows(
    snapshot: Snapshot,
    thread: Thread?,
    previous: ConversationProjection?,
): ConversationProjection? {
    if (thread == null) return null
    val next = projectConversation(snapshot, thread, previous?.source)
    return if (previous?.source?.let(next::unchanged) == true) previous
    else {
        val turns = mutableMapOf<String, Pair<RenderedTurn, List<ConversationRow>>>()
        val rows =
            next.turns().flatMap { turn ->
                val old = previous?.turns?.get(turn.id())
                val content = if (old != null && turn.unchanged(old.first)) old.second else turn.conversationRows()
                turns[turn.id()] = turn to content
                content
            }
        ConversationProjection(next, turns, rows, next.queued(), next.unplacedRequests())
    }
}

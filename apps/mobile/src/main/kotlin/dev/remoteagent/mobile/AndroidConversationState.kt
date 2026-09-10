package dev.remoteagent.mobile

import dev.remoteagent.core.JsonValue
import dev.remoteagent.core.RenderedConversation
import dev.remoteagent.core.RenderedItem
import dev.remoteagent.core.RenderedTurn
import dev.remoteagent.core.Snapshot
import dev.remoteagent.core.Thread
import dev.remoteagent.core.TurnPresentationData
import dev.remoteagent.core.formatJsonValue
import dev.remoteagent.core.projectConversation

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

/** Retain already-marshaled native rows; projection and body formatting live in Rust. */
internal class ConversationProjection {
    private var previous: RenderedConversation? = null
    private var turns = emptyMap<String, Pair<RenderedTurn, List<TurnPresentationData>>>()
    private var rows = emptyList<TurnPresentationData>()
    var queued = emptyList<RenderedItem>()
        private set

    fun project(snapshot: Snapshot, thread: Thread?): List<TurnPresentationData> {
        if (thread == null) {
            previous = null; turns = emptyMap(); rows = emptyList(); queued = emptyList()
            return rows
        }
        val next = projectConversation(snapshot, thread, previous)
        if (previous?.let(next::unchanged) != true) {
            val cached = mutableMapOf<String, Pair<RenderedTurn, List<TurnPresentationData>>>()
            rows = next.turns().flatMap { turn ->
                val id = turn.id()
                val old = turns[id]
                val content = if (old != null && turn.unchanged(old.first)) old.second else turn.rows()
                cached[id] = turn to content
                content
            }
            turns = cached; queued = next.queued(); previous = next
        }
        return rows
    }
}

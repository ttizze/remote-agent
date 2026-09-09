package dev.remoteagent.mobile

import dev.remoteagent.core.DisplayItem
import dev.remoteagent.core.Item
import dev.remoteagent.core.ItemPresentation
import dev.remoteagent.core.JsonValue
import dev.remoteagent.core.PendingSubmission
import dev.remoteagent.core.Request
import dev.remoteagent.core.Role
import dev.remoteagent.core.Segment
import dev.remoteagent.core.Turn
import dev.remoteagent.core.formatJsonValue

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

internal fun JsonValue.pretty(): String = formatJsonValue(this)

internal class ConversationItem(
    val source: Item?,
    val id: String,
    private val presentation: ItemPresentation,
    val body: String,
    val images: List<String>,
    val deferred: Boolean = false,
) {
    val kind
        get() = presentation.kind

    val title
        get() = presentation.title

    val collapsible
        get() = presentation.collapsible

    fun expanded(): String {
        val item = source ?: return body
        return when (kind) {
            "user",
            "agent",
            "commentary" -> body
            "reasoning" ->
                item
                    .field("summary")
                    ?.let { value ->
                        value.text ?: value.values.joinToString("\n") { it["text"]?.text ?: it.text.orEmpty() }
                    }
                    .orEmpty()
            "command" ->
                listOfNotNull(item.field("cwd")?.text?.let { "cwd: $it" }, item.aggregatedOutput()).joinToString("\n")
            "fileChange" ->
                item.field("changes")?.values.orEmpty().joinToString("\n\n") {
                    val change = it["kind"]?.text ?: it["kind"]?.get("type")?.text.orEmpty()
                    "$change: ${it["path"]?.text.orEmpty()}\n${it["diff"]?.text.orEmpty()}"
                }
            else -> JsonValue.Object(item.fields()).pretty()
        }
    }

    companion object {
        fun native(item: Item, deferred: Boolean): ConversationItem {
            val presentation = item.presentation()
            val content = item.field("content")?.values.orEmpty()
            val text = item.text() ?: content.joinToString("") { it["text"]?.text ?: it.text.orEmpty() }
            val body =
                when (presentation.kind) {
                    "user",
                    "agent",
                    "commentary" ->
                        (listOf(text) +
                                content
                                    .filter { it["type"]?.text == "mention" }
                                    .map { "添付: ${it["name"]?.text.orEmpty()} (${it["path"]?.text.orEmpty()})" })
                            .filter(String::isNotEmpty)
                            .joinToString("\n")
                    "reasoning" -> "詳細を表示"
                    "imageGeneration" -> presentation.title
                    else -> item.status() ?: "詳細を表示"
                }
            return ConversationItem(
                item,
                item.clientId() ?: item.id(),
                presentation,
                body,
                images(item, presentation.kind, content),
                deferred,
            )
        }

        private fun images(item: Item, kind: String, content: List<JsonValue>): List<String> {
            return if (kind == "imageGeneration")
                listOfNotNull(item.savedPath() ?: item.result()?.text?.let { "data:image/png;base64,$it" })
            else
                content.mapNotNull {
                    when (it["type"]?.text) {
                        "localImage" -> it["path"]?.text
                        "image" -> it["url"]?.text
                        else -> null
                    }
                }
        }

        fun submitted(submission: PendingSubmission) =
            ConversationItem(
                null,
                submission.id,
                ItemPresentation("user", "You", false, true),
                (listOf(submission.draft.text) +
                        submission.draft.attachments.filterNot { it.isImage }.map { "添付: ${it.name} (${it.path})" })
                    .filter(String::isNotEmpty)
                    .joinToString("\n"),
                submission.draft.attachments.filter { it.isImage }.map { it.path },
            )
    }
}

internal data class ConversationTurn(
    val source: Turn,
    val segment: Segment,
    val users: List<ConversationItem>,
    val activity: List<ConversationItem>,
    val responses: List<ConversationItem>,
    val opening: ConversationItem?,
    val requests: List<Request>,
    val error: JsonValue?,
)

/** Render cache keys are Rust immutable identities; native code never reduces wire events. */
internal class ConversationProjection {
    private data class Cached(
        val source: Turn,
        val requests: List<Request>,
        val rows: List<ConversationTurn>,
        val items: Map<String, ConversationItem>,
    )

    private var turns = emptyMap<String, Cached>()
    private var previousThread: dev.remoteagent.core.Thread? = null
    private var previousRequests = emptyList<Request>()
    private var previousRows = emptyList<ConversationTurn>()

    fun project(thread: dev.remoteagent.core.Thread?, requests: List<Request>): List<ConversationTurn> {
        if (thread == null || (previousThread?.let(thread::unchanged) == true && previousRequests == requests)) {
            if (thread == null) {
                turns = emptyMap()
                previousThread = null
                previousRequests = emptyList()
                previousRows = emptyList()
            }
            return previousRows
        }
        val native = thread.turns()
        val next = mutableMapOf<String, Cached>()
        val rows = mutableListOf<ConversationTurn>()
        for (turn in native) {
            val id = turn.id()
            val pending = requests.filter {
                it.params["threadId"]?.text == thread.id() &&
                    (it.params["turnId"]?.text ?: native.lastOrNull()?.id()) == id
            }
            val previous = turns[id]
            if (previous != null && turn.unchanged(previous.source) && previous.requests == pending) {
                next[id] = previous
                rows.addAll(previous.rows)
                continue
            }
            val cached = projectTurn(turn, pending, previous)
            next[id] = cached
            rows.addAll(cached.rows)
        }
        turns = next
        previousThread = thread
        previousRequests = requests
        previousRows = rows
        return rows
    }

    private fun projectTurn(turn: Turn, pending: List<Request>, previous: Cached?): Cached {
        val deferred = turn.deferredItemIds().toSet()
        val items = mutableMapOf<String, ConversationItem>()
        fun item(source: Item): ConversationItem {
            val key = source.id()
            val old = previous?.items?.get(key)
            val cached = old?.takeIf { it.source?.let(source::unchanged) == true && it.deferred == (key in deferred) }
            return (cached ?: ConversationItem.native(source, key in deferred)).also { items[key] = it }
        }
        val content = turn.content()
        val display =
            content.items.map {
                when (it) {
                    is DisplayItem.Native -> item(it.item)
                    is DisplayItem.Submitted -> ConversationItem.submitted(it.submission)
                }
            }
        val opening =
            turn
                .openingUserMessage()
                ?.takeUnless { source -> display.any { it.source?.id() == source.id() } }
                ?.let(::item)
        val projected = content.segments.map { segment -> projectSegment(turn, segment, display, opening, pending) }
        return Cached(turn, pending, projected, items)
    }

    private fun projectSegment(
        turn: Turn,
        segment: Segment,
        display: List<ConversationItem>,
        opening: ConversationItem?,
        pending: List<Request>,
    ): ConversationTurn {
        val users = mutableListOf<ConversationItem>()
        val activity = mutableListOf<ConversationItem>()
        val responses = mutableListOf<ConversationItem>()
        segment.roles.forEachIndexed { offset, role ->
            val value = display[segment.start.toInt() + offset]
            when (role) {
                Role.USER -> users.add(value)
                Role.ACTIVITY -> activity.add(value)
                Role.RESPONSE -> responses.add(value)
                Role.HIDDEN -> Unit
            }
        }
        return ConversationTurn(
            turn,
            segment,
            users,
            activity,
            responses,
            opening.takeIf { segment.start == 0u },
            if (segment.last) pending else emptyList(),
            turn.error().takeIf { segment.last },
        )
    }
}

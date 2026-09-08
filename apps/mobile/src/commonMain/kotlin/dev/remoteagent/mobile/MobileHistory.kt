package dev.remoteagent.mobile

import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject

internal val ThreadSnapshot.olderTurnsCursor: String?
    get() = raw?.string("historyCursor")
internal val CodexTurn.olderItemsCursor: String?
    get() = raw?.string("itemsNextCursor")
internal val CodexTurn.hasOlderItems: Boolean
    get() = raw?.boolean("itemsHasMore") ?: (olderItemsCursor != null)

/** Older pages prepend; already observed live items win overlapping IDs. */
internal fun mergeOlderHistory(current: ThreadSnapshot, page: ThreadSnapshot, turnId: String?): ThreadSnapshot {
    if (turnId != null) {
        val older = page.turns.single { it.id == turnId }
        return current.copy(
            turns =
                current.turns.map { turn ->
                    if (turn.id != turnId) turn
                    else
                        turn.copy(
                            items = prependDistinctItems(older.items, turn.items),
                            raw =
                                JsonObject(
                                    turn.raw.orEmpty() +
                                        older.raw.orEmpty().filterKeys { it == "openingUserMessage" } +
                                        mapOf(
                                            "itemsNextCursor" to (older.raw?.get("itemsNextCursor") ?: JsonNull),
                                            "itemsHasMore" to
                                                kotlinx.serialization.json.JsonPrimitive(older.hasOlderItems),
                                            "deferredItemIds" to mergeDeferredIds(older, turn),
                                        )
                                ),
                        )
                }
        )
    }
    val known = current.turns.mapTo(mutableSetOf()) { it.id }
    return current.copy(
        turns = page.turns.filterNot { it.id in known } + current.turns,
        raw = JsonObject(current.raw.orEmpty() + ("historyCursor" to (page.raw?.get("historyCursor") ?: JsonNull))),
    )
}

private fun prependDistinctItems(older: List<CodexItem>, current: List<CodexItem>): List<CodexItem> {
    val known = current.mapTo(mutableSetOf()) { it.id }
    return older.filter { known.add(it.id) } + current
}

private fun mergeDeferredIds(a: CodexTurn, b: CodexTurn) =
    kotlinx.serialization.json.JsonArray(
        ((a.raw?.get("deferredItemIds") as? kotlinx.serialization.json.JsonArray).orEmpty() +
                (b.raw?.get("deferredItemIds") as? kotlinx.serialization.json.JsonArray).orEmpty())
            .distinct()
    )

/** Keep fetched prefixes only across an overlapping, authoritative tail read. */
internal fun mergeHistoryRefresh(previous: ThreadSnapshot?, fresh: ThreadSnapshot): ThreadSnapshot {
    if (
        previous?.raw?.containsKey("historyCursor") != true ||
            fresh.raw?.containsKey("historyCursor") != true ||
            fresh.turns.isEmpty()
    )
        return fresh
    val boundary = previous.turns.indexOfFirst { it.id == fresh.turns.first().id }
    return if (boundary < 0) fresh
    else {
        val oldTurns = previous.turns.associateBy { it.id }
        val turns =
            fresh.turns.map { turn ->
                val old = oldTurns[turn.id] ?: return@map turn
                val first =
                    turn.items.firstOrNull()?.id
                        ?: return@map if (turn.hasOlderItems) turn.copy(items = old.items, raw = old.raw) else turn
                val itemBoundary = old.items.indexOfFirst { it.id == first }
                if (itemBoundary < 0) return@map turn
                turn.copy(
                    items = old.items.take(itemBoundary) + turn.items,
                    raw =
                        JsonObject(
                            turn.raw.orEmpty() +
                                mapOf(
                                    "itemsNextCursor" to (old.raw?.get("itemsNextCursor") ?: JsonNull),
                                    "itemsHasMore" to kotlinx.serialization.json.JsonPrimitive(old.hasOlderItems),
                                    "deferredItemIds" to mergeDeferredIds(old, turn),
                                )
                        ),
                )
            }
        val historyCursor = previous.raw.get("historyCursor") ?: JsonNull
        fresh.copy(
            turns = previous.turns.take(boundary) + turns,
            raw = JsonObject(fresh.raw.orEmpty() + ("historyCursor" to historyCursor)),
        )
    }
}

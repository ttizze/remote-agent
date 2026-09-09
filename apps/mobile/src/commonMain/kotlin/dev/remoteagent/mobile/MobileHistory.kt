package dev.remoteagent.mobile

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put

internal val ThreadSnapshot.olderTurnsCursor: String?
    get() = raw?.string("historyCursor")
internal val CodexTurn.olderItemsCursor: String?
    get() = raw?.string("itemsNextCursor")
internal val CodexTurn.hasOlderItems: Boolean
    get() = raw?.boolean("itemsHasMore") ?: (olderItemsCursor != null)

// FFI carries source positions and pagination metadata. Bodies stay in Kotlin.
private val historyFields =
    setOf("historyCursor", "itemsNextCursor", "itemsHasMore", "deferredItemIds", "openingUserMessage")

private fun reference(source: String, turn: Int, item: Int? = null, request: Int? = null): JsonObject =
    buildJsonObject {
        put("source", source)
        put("turn", turn)
        item?.let { put("item", it) }
        request?.let { put("request", it) }
    }

private fun ThreadSnapshot.historyMetadata(source: String): JsonObject = buildJsonObject {
    put("id", summary.id)
    raw?.get("historyCursor")?.let { put("historyCursor", it) }
    put(
        "turns",
        JsonArray(
            turns.mapIndexed { turnIndex, turn ->
                buildJsonObject {
                    put("id", turn.id)
                    put("reference", reference(source, turnIndex))
                    if (turn.pendingRequests.isNotEmpty()) {
                        put(
                            "pendingRequests",
                            JsonArray(
                                turn.pendingRequests.mapIndexed { index, request ->
                                    buildJsonObject {
                                        put("id", request.id)
                                        put("reference", reference(source, turnIndex, request = index))
                                    }
                                }
                            ),
                        )
                    }
                    turn.raw?.forEach { (key, value) ->
                        if (key in historyFields)
                            put(
                                key,
                                if (key == "openingUserMessage" && value != JsonNull) reference(source, turnIndex)
                                else value,
                            )
                    }
                    put(
                        "items",
                        JsonArray(
                            turn.items.mapIndexed { itemIndex, item ->
                                buildJsonObject {
                                    put("id", item.id)
                                    put("reference", reference(source, turnIndex, itemIndex))
                                }
                            }
                        ),
                    )
                }
            }
        ),
    )
}

internal fun mergeOlderHistory(
    current: ThreadSnapshot,
    page: ThreadSnapshot,
    turnId: String?,
    cursor: String?,
): ThreadSnapshot = reconcileHistory("historyOlder", current, page, current, turnId, cursor)

internal fun mergeHistoryRefresh(previous: ThreadSnapshot?, fresh: ThreadSnapshot): ThreadSnapshot =
    if (previous == null) fresh else reconcileHistory("historyRefresh", previous, fresh, fresh)

private fun reconcileHistory(
    operation: String,
    previous: ThreadSnapshot,
    incoming: ThreadSnapshot,
    base: ThreadSnapshot,
    turnId: String? = null,
    cursor: String? = null,
): ThreadSnapshot {
    val result =
        Json.parseToJsonElement(
                nativeConversationPresentation(
                    buildJsonObject {
                        put("operation", operation)
                        put("previous", previous.historyMetadata("previous"))
                        put("incoming", incoming.historyMetadata("incoming"))
                        put("turnId", turnId)
                        put("cursor", cursor)
                    }
                        .toString()
                )
            )
            .jsonObject
    fun source(token: JsonObject): ThreadSnapshot = if (token.string("source") == "previous") previous else incoming
    fun turn(token: JsonObject): CodexTurn = source(token).turns[token.getValue("turn").jsonPrimitive.int]
    fun metadata(original: JsonObject?, value: JsonObject): JsonObject? {
        val updates =
            value
                .filterKeys { it in historyFields }
                .mapValues { (key, field) ->
                    if (key == "openingUserMessage" && field is JsonObject) turn(field).raw?.get(key) ?: JsonNull
                    else field
                }
        return if (updates.isEmpty()) original else JsonObject(original.orEmpty() + updates)
    }
    return base.copy(
        summary = base.summary.copy(raw = metadata(base.summary.raw, result)),
        raw = metadata(base.raw, result),
        turns =
            result.getValue("turns").jsonArray.map { value ->
                val projected = value.jsonObject
                val original = turn(projected.getValue("reference").jsonObject)
                original.copy(
                    raw = metadata(original.raw, projected),
                    pendingRequests =
                        projected["pendingRequests"]?.jsonArray.orEmpty().map { request ->
                            val token = request.jsonObject.getValue("reference").jsonObject
                            turn(token).pendingRequests[token.getValue("request").jsonPrimitive.int]
                        },
                    items =
                        projected.getValue("items").jsonArray.map { item ->
                            val token = item.jsonObject.getValue("reference").jsonObject
                            turn(token).items[token.getValue("item").jsonPrimitive.int]
                        },
                )
            },
    )
}

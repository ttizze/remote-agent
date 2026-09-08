package dev.remoteagent.mobile

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.put

internal suspend fun RawCodexGateway.sessionImages(profile: HostProfile, threadId: String): List<String> {
    val sources = linkedSetOf<String>()
    var page =
        readImageHistory(
            profile,
            "host/thread/read",
            buildJsonObject {
                put("threadId", threadId)
                put("includeTurns", true)
                put("paginateHistory", true)
                put("deferItemDetails", true)
            },
        )
    val turnCursors = mutableSetOf<String>()
    while (true) {
        for (turn in (page["turns"] as? JsonArray ?: error("履歴のターンがありません。")).reversed()) {
            collectTurnImages(profile, threadId, turn, sources)
        }
        val cursor = (page["historyCursor"] as? JsonPrimitive)?.contentOrNull?.takeIf { it.isNotEmpty() } ?: break
        check(turnCursors.add(cursor)) { "履歴カーソルが進みませんでした。" }
        page =
            readImageHistory(
                profile,
                "host/thread/turns/list",
                buildJsonObject {
                    put("threadId", threadId)
                    put("cursor", cursor)
                    put("deferItemDetails", true)
                },
            )
    }
    return sources.toList().asReversed()
}

private suspend fun RawCodexGateway.collectTurnImages(
    profile: HostProfile,
    threadId: String,
    turn: JsonElement,
    sources: MutableSet<String>,
) {
    var page = turn as? JsonObject ?: error("履歴のターンが無効です。")
    val turnId = (page["id"] as? JsonPrimitive)?.contentOrNull ?: error("ターンIDがありません。")
    val cursors = mutableSetOf<String?>()
    while (true) {
        for (item in (page["items"] as? JsonArray ?: error("履歴の項目がありません。")).reversed()) {
            item.generatedImageSource()?.let { sources.add(it) }
        }
        if ((page["itemsHasMore"] as? JsonPrimitive)?.contentOrNull != "true") break
        // A turn skipped by the initial item budget starts at a null cursor.
        val cursor = (page["itemsNextCursor"] as? JsonPrimitive)?.contentOrNull
        check(cursors.add(cursor)) { "履歴カーソルが進みませんでした。" }
        val older =
            readImageHistory(
                profile,
                "host/thread/items/list",
                buildJsonObject {
                    put("threadId", threadId)
                    put("turnId", turnId)
                    put("cursor", cursor)
                    put("deferItemDetails", true)
                },
            )
        page = (older["turns"] as? JsonArray)?.singleOrNull() as? JsonObject ?: error("履歴のターンがありません。")
    }
}

private fun JsonElement.generatedImageSource(): String? {
    val item = this as? JsonObject
    if ((item?.get("type") as? JsonPrimitive)?.contentOrNull != "imageGeneration") return null
    val path = (item["savedPath"] as? JsonPrimitive)?.contentOrNull
    val inline = (item["result"] as? JsonPrimitive)?.contentOrNull
    return when {
        !path.isNullOrBlank() -> path
        inline.isNullOrBlank() -> null
        inline.startsWith("data:") -> inline
        else -> "data:image/png;base64,$inline"
    }
}

private suspend fun RawCodexGateway.readImageHistory(
    profile: HostProfile,
    method: String,
    params: JsonObject,
): JsonObject =
    when (val result = rawRequest(profile, method, params)) {
        is GatewayResult.Failure -> error(result.message)
        is GatewayResult.Success ->
            (result.value as? JsonObject)?.get("thread") as? JsonObject ?: error("画像一覧の履歴が無効です。")
    }

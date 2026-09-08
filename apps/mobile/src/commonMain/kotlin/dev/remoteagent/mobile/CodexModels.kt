package dev.remoteagent.mobile

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

private const val MAX_MODEL_PAGES = 64
private const val MODEL_PAGE_SIZE = 100

suspend fun CommonCodexClient.listModels(profile: HostProfile): GatewayResult<List<CodexModel>> {
    val models = linkedMapOf<String, CodexModel>()
    val cursors = mutableSetOf<String>()
    var cursor: String? = null
    var failure: GatewayResult.Failure? = null
    do {
        val page =
            rawGateway
                .rawRequest(
                    profile,
                    "model/list",
                    buildJsonObject {
                        put("limit", MODEL_PAGE_SIZE)
                        cursor?.let { put("cursor", it) }
                    },
                )
                .decode("model/list", ::parseModelPage)
        when (page) {
            is GatewayResult.Failure -> failure = page
            is GatewayResult.Success -> {
                val result = page.value
                result.first.forEach { models[it.id] = it }
                cursor = result.second
                if (cursor != null && (!cursors.add(cursor) || cursors.size >= MAX_MODEL_PAGES)) {
                    failure = GatewayResult.Failure("モデル一覧の続きを取得できませんでした")
                }
            }
        }
    } while (failure == null && cursor != null)
    return failure ?: GatewayResult.Success(models.values.toList())
}

private fun parseModelPage(value: JsonElement): Pair<List<CodexModel>, String?> {
    val page = value as? JsonObject ?: invalid("model page must be an object")
    val data = page["data"] as? JsonArray ?: invalid("model data must be an array")
    return data.map(::parseModel) to page.string("nextCursor")?.takeIf(String::isNotBlank)
}

private fun parseModel(entry: JsonElement): CodexModel {
    val model = entry as? JsonObject ?: invalid("model must be an object")
    val efforts = model["supportedReasoningEfforts"] as? JsonArray ?: invalid("reasoning efforts must be an array")
    return CodexModel(
        model.string("id") ?: invalid("missing model id"),
        model.string("model") ?: invalid("missing model name"),
        model.string("displayName") ?: invalid("missing display name"),
        model.string("defaultReasoningEffort") ?: invalid("missing default reasoning effort"),
        efforts.map { (it as? JsonObject)?.string("reasoningEffort") ?: invalid("missing reasoning effort") },
    )
}

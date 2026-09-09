package dev.remoteagent.mobile

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

@Serializable
data class ThreadListQuery(
    val projectLimit: Int = 5,
    val chatLimit: Int = 5,
    val projectThreadLimits: Map<String, Int> = emptyMap(),
    val searchTerm: String = "",
)

data class ThreadListPage(
    val threads: List<ThreadSummary>,
    val projects: List<CodexProject> = emptyList(),
    val moreProjectIds: Set<String> = emptySet(),
    val hasMoreChats: Boolean = false,
    val hasMoreProjects: Boolean = false,
)

internal fun parseThreadListPage(value: JsonElement): ThreadListPage {
    val root = value as JsonObject
    return ThreadListPage(
        (root.getValue("data") as JsonArray).map { codexThreadSummary(it as JsonObject) },
        (root.getValue("projects") as JsonArray).map(::codexProject),
        (root.getValue("moreProjectIds") as JsonArray).mapTo(mutableSetOf()) { (it as JsonPrimitive).content },
        root.boolean("hasMoreChats")!!,
        root.boolean("hasMoreProjects")!!,
    )
}

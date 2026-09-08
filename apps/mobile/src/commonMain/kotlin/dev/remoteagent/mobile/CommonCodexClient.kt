package dev.remoteagent.mobile

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

data class CodexModel(
    val id: String,
    val model: String,
    val displayName: String,
    val defaultReasoningEffort: String,
    val reasoningEfforts: List<String>,
)

data class CodexTurnOptions(val model: String? = null, val effort: String? = null)

/**
 * Common, typed orchestration for Codex Desktop projects and Codex thread/turn operations.
 *
 * Platform gateways own pairing, transport, subscriptions, and the raw JSON boundary. This class owns the protocol
 * knowledge that would otherwise be duplicated in Android and iOS.
 */
class CommonCodexClient(internal val rawGateway: RawCodexGateway, private val deferItemDetails: Boolean = false) :
    CodexGateway {
    private val turnOptions = mutableMapOf<String, CodexTurnOptions>()

    fun setTurnOptions(hostIdentity: String, options: CodexTurnOptions) {
        turnOptions[hostIdentity] = options
    }

    override suspend fun listThreads(profile: HostProfile, query: ThreadListQuery): GatewayResult<ThreadListPage> =
        rawGateway
            .rawRequest(
                profile,
                "host/thread/list",
                buildJsonObject {
                    put("titleOnly", true)
                    put("projectLimit", query.projectLimit)
                    put("chatLimit", query.chatLimit)
                    put(
                        "projectThreadLimits",
                        buildJsonObject { query.projectThreadLimits.forEach { (id, limit) -> put(id, limit) } },
                    )
                    if (query.searchTerm.isNotBlank()) put("searchTerm", query.searchTerm)
                },
            )
            .decode("host/thread/list", ::parseThreadListPage)

    override suspend fun readThread(profile: HostProfile, threadId: String): GatewayResult<ThreadReadResult> {
        val params = buildJsonObject {
            put("threadId", threadId)
            put("includeTurns", true)
            put("paginateHistory", true)
            if (deferItemDetails) put("deferItemDetails", true)
        }
        val result =
            rawGateway.rawRequest(profile, "host/thread/read", params).decode("host/thread/read") { value ->
                parseThreadReadResult(value, threadId)
            }
        return result
    }

    suspend fun readOlderHistory(
        profile: HostProfile,
        threadId: String,
        cursor: String?,
        turnId: String? = null,
    ): GatewayResult<ThreadSnapshot> =
        rawGateway
            .rawRequest(
                profile,
                if (turnId == null) "host/thread/turns/list" else "host/thread/items/list",
                buildJsonObject {
                    put("threadId", threadId)
                    cursor?.let { put("cursor", it) }
                    turnId?.let { put("turnId", it) }
                    if (deferItemDetails) put("deferItemDetails", true)
                },
            )
            .decode("history page") {
                parseThreadSnapshot(it, threadId).also { page ->
                    if (turnId != null && (page.turns.size != 1 || page.turns.single().id != turnId))
                        invalid("turn ID does not match")
                }
            }

    suspend fun readItemDetails(
        profile: HostProfile,
        threadId: String,
        turnId: String,
        itemId: String,
    ): GatewayResult<String> =
        rawGateway
            .rawRequest(
                profile,
                "host/thread/item/read",
                buildJsonObject {
                    put("threadId", threadId)
                    put("turnId", turnId)
                    put("itemId", itemId)
                },
            )
            .decode("host/thread/item/read") { value ->
                val item = (value as? JsonObject)?.get("item") as? JsonObject ?: invalid("item must be an object")
                if (item.string("id") != itemId) invalid("item ID does not match")
                codexItem(item).expandedThreadItemBody()
            }

    override suspend fun startThread(profile: HostProfile, cwd: String): GatewayResult<ThreadSnapshot> {
        val options = turnOptions[profile.id]
        val params = buildJsonObject {
            if (cwd.isNotBlank()) put("cwd", cwd)
            options?.model?.let { put("model", it) }
        }
        val result =
            rawGateway.rawRequest(profile, "host/thread/start", params).decode("host/thread/start") {
                parseThreadSnapshot(it)
            }
        return result
    }

    /** A new/loaded thread has no history to resume before its first turn. */
    override suspend fun startTurn(
        profile: HostProfile,
        threadId: String,
        cwd: String,
        input: CodexTurnInput,
        resume: Boolean,
    ): GatewayResult<String> {
        val options = turnOptions[profile.id]
        if (resume) {
            val resumed =
                rawGateway.rawRequest(
                    profile,
                    "thread/resume",
                    buildJsonObject {
                        put("threadId", threadId)
                        put("cwd", cwd)
                    },
                )
            if (resumed is GatewayResult.Failure) return resumed
        }

        return rawGateway
            .rawRequest(
                profile,
                "turn/start",
                buildJsonObject {
                    put("threadId", threadId)
                    put("clientUserMessageId", input.clientUserMessageId)
                    put("input", turnInput(input.text, input.attachments))
                    options?.model?.let { put("model", it) }
                    options?.effort?.let { put("effort", it) }
                },
            )
            .decode("turn/start") { value -> parseTurnId(value) }
    }

    override suspend fun steerTurn(
        profile: HostProfile,
        threadId: String,
        turnId: String,
        input: CodexTurnInput,
    ): GatewayResult<Unit> =
        rawGateway
            .rawRequest(
                profile,
                "turn/steer",
                buildJsonObject {
                    put("threadId", threadId)
                    put("expectedTurnId", turnId)
                    put("clientUserMessageId", input.clientUserMessageId)
                    put("input", turnInput(input.text, input.attachments))
                },
            )
            .mapGateway { Unit }

    override suspend fun queueTurn(
        profile: HostProfile,
        threadId: String,
        input: CodexTurnInput,
    ): GatewayResult<String> {
        return rawGateway
            .rawRequest(
                profile,
                "thread/queue/add",
                buildJsonObject {
                    put("threadId", threadId)
                    put("clientUserMessageId", input.clientUserMessageId)
                    put("input", turnInput(input.text, input.attachments))
                },
            )
            .decode("thread/queue/add", ::parseQueuedSubmissionId)
    }

    override suspend fun interrupt(profile: HostProfile, threadId: String, turnId: String): GatewayResult<Unit> =
        rawGateway
            .rawRequest(
                profile,
                "turn/interrupt",
                buildJsonObject {
                    put("threadId", threadId)
                    put("turnId", turnId)
                },
            )
            .mapGateway { Unit }
}

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

private fun parseThreadListPage(value: JsonElement): ThreadListPage {
    val root = value as? JsonObject ?: invalid("expected an object")
    val data = root["data"] as? JsonArray ?: invalid("data must be an array")
    val threads = data.map { element ->
        val objectValue = element as? JsonObject ?: invalid("data entries must be objects")
        codexThreadSummary(objectValue).also { summary ->
            if (summary.id.isBlank()) invalid("thread data entry is missing id")
        }
    }
    val projects =
        (root["projects"] as? JsonArray ?: invalid("projects must be an array")).map { entry ->
            codexProject(entry).also { if (it.id.isBlank()) invalid("project is missing id") }
        }
    val moreProjectIds =
        (root["moreProjectIds"] as? JsonArray ?: invalid("moreProjectIds must be an array")).mapTo(mutableSetOf()) {
            (it as? JsonPrimitive)?.stringOrNull() ?: invalid("project id must be a string")
        }
    val hasMoreChats = root.boolean("hasMoreChats") ?: invalid("hasMoreChats must be a boolean")
    val hasMoreProjects = root.boolean("hasMoreProjects") ?: invalid("hasMoreProjects must be a boolean")
    return ThreadListPage(threads, projects, moreProjectIds, hasMoreChats, hasMoreProjects)
}

private fun parseThreadReadResult(value: JsonElement, expectedThreadId: String): ThreadReadResult =
    ThreadReadResult(parseThreadSnapshot(value, expectedThreadId), emptyList())

private fun parseThreadSnapshot(value: JsonElement, expectedThreadId: String? = null): ThreadSnapshot {
    val root = value as? JsonObject ?: invalid("expected an object")
    val thread = root["thread"] ?: root
    val threadObject = thread as? JsonObject ?: invalid("thread must be an object")
    val turns = threadObject["turns"]
    if (turns != null) {
        val turnArray = turns as? JsonArray ?: invalid("turns must be an array")
        turnArray.forEach { turn ->
            val turnObject = turn as? JsonObject ?: invalid("turn entries must be objects")
            if (turnObject.string("id").isNullOrBlank() && turnObject.string("turnId").isNullOrBlank()) {
                invalid("turn entry is missing id")
            }
        }
    }
    return codexThreadFromResponse(root).also { snapshot ->
        if (snapshot.summary.id.isBlank()) invalid("thread is missing id")
        if (expectedThreadId != null && snapshot.summary.id != expectedThreadId) {
            invalid("thread id does not match the requested thread")
        }
    }
}

private fun parseTurnId(value: JsonElement): String {
    val root = value as? JsonObject ?: invalid("expected an object")
    val id = root.string("turnId") ?: root.childObject("turn")?.string("id")
    return id?.takeIf(String::isNotBlank) ?: invalid("turn/start response is missing turn id")
}

private fun parseQueuedSubmissionId(value: JsonElement): String {
    val root = value as? JsonObject ?: invalid("expected an object")
    val queuedSubmission = root["queuedSubmission"] as? JsonObject ?: invalid("queuedSubmission must be an object")
    return queuedSubmission.string("id")?.takeIf(String::isNotBlank)
        ?: invalid("thread/queue/add response is missing queued submission id")
}

internal fun invalid(message: String): Nothing = throw IllegalArgumentException(message)

internal fun <T> GatewayResult<JsonElement>.decode(method: String, transform: (JsonElement) -> T): GatewayResult<T> =
    when (this) {
        is GatewayResult.Failure -> this
        is GatewayResult.Success ->
            try {
                GatewayResult.Success(transform(value))
            } catch (failure: IllegalArgumentException) {
                GatewayResult.Failure(
                    message = "Invalid $method response: ${failure.message ?: "malformed payload"}",
                    rawError = value,
                )
            }
    }

private fun turnInput(text: String, attachments: List<CodexAttachment>) = buildJsonArray {
    if (text.isNotBlank())
        add(
            buildJsonObject {
                put("type", "text")
                put("text", text)
                put("text_elements", buildJsonArray {})
            }
        )
    attachments.forEach { attachment ->
        add(
            buildJsonObject {
                put("type", if (attachment.isImage) "localImage" else "mention")
                put("path", attachment.path)
                if (!attachment.isImage) put("name", attachment.name)
            }
        )
    }
}

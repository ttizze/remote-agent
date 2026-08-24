package dev.remoteagent.mobile

import kotlinx.coroutines.CancellationException
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

private const val MaxThreadItems = 64
private const val MaxThreadPages = 64

/**
 * Limits applied to the Codex operations owned by [CommonCodexClient].
 *
 * These are client-side safety limits, rather than claims about the limits of
 * Codex itself.  In particular, a continuation is never followed after the
 * configured page budget has been used.  The defaults match the previous
 * Android and iOS gateways' cache bound.
 */
data class CommonCodexClientLimits(
    val maxThreadItems: Int = MaxThreadItems,
    val maxThreadPages: Int = MaxThreadPages,
) {
    init {
        require(maxThreadItems in 1..MaxThreadItems) { "maxThreadItems must be between 1 and $MaxThreadItems" }
        require(maxThreadPages in 1..MaxThreadPages) { "maxThreadPages must be between 1 and $MaxThreadPages" }
    }
}

/**
 * Common, typed orchestration for the baseline Codex thread/turn operations.
 *
 * Platform gateways own pairing, transport, subscriptions, and the raw JSON
 * boundary.  This class owns the small amount of protocol knowledge that was
 * previously duplicated in Android and iOS.  Project APIs intentionally do
 * not belong here.
 */
class CommonCodexClient(
    private val rawGateway: RawCodexGateway,
    private val limits: CommonCodexClientLimits = CommonCodexClientLimits(),
) {
    suspend fun listThreads(
        profile: HostProfile,
        cwd: String,
    ): GatewayResult<List<ThreadSummary>> {
        val threads = mutableListOf<ThreadSummary>()
        val seenThreads = mutableSetOf<ThreadListIdentity>()
        val seenCursors = mutableSetOf<String>()
        var cursor: String? = null
        var pagesRead = 0

        while (threads.size < limits.maxThreadItems && pagesRead < limits.maxThreadPages) {
            val remaining = limits.maxThreadItems - threads.size
            val params = buildJsonObject {
                put("limit", remaining)
                if (cwd.isNotBlank()) put("cwd", cwd)
                cursor?.let { put("cursor", it) }
            }
            val page = request(profile, "thread/list", params).decode("thread/list") { value ->
                parseThreadListPage(value)
            }
            val pageValue = when (page) {
                is GatewayResult.Success -> page.value
                is GatewayResult.Failure -> return page
            }

            pagesRead += 1
            pageValue.threads.forEach { thread ->
                val identity = ThreadListIdentity(thread.id, thread.workingDirectory.path)
                if (threads.size < limits.maxThreadItems && seenThreads.add(identity)) {
                    threads += thread
                }
            }

            val nextCursor = pageValue.nextCursor?.takeIf(String::isNotBlank)
            if (nextCursor == null || threads.size >= limits.maxThreadItems || !seenCursors.add(nextCursor)) break
            cursor = nextCursor
        }

        return GatewayResult.Success(threads)
    }

    suspend fun readThread(
        profile: HostProfile,
        threadId: String,
    ): GatewayResult<ThreadReadResult> {
        val params = buildJsonObject {
            put("threadId", threadId)
            put("includeTurns", true)
        }
        val result = request(profile, "thread/read", params).decode("thread/read") { value ->
            parseThreadReadResult(value, threadId)
        }
        return result
    }

    suspend fun startThread(
        profile: HostProfile,
        cwd: String,
    ): GatewayResult<ThreadSnapshot> {
        val params = buildJsonObject { put("cwd", cwd) }
        val result = request(profile, "thread/start", params).decode("thread/start") {
            parseThreadSnapshot(it)
        }
        return result
    }

    /**
     * Starts a turn using an explicitly supplied working directory.  Codex
     * requires the thread to be resumed before turn/start, and a failed resume
     * never permits turn/start to be sent.
     */
    suspend fun startTurn(
        profile: HostProfile,
        threadId: String,
        cwd: String,
        text: String,
    ): GatewayResult<String> {
        val resumed = request(
            profile,
            "thread/resume",
            buildJsonObject {
                put("threadId", threadId)
                put("cwd", cwd)
            },
        )
        if (resumed is GatewayResult.Failure) return resumed

        return request(
            profile,
            "turn/start",
            buildJsonObject {
                put("threadId", threadId)
                put("input", buildJsonArray {
                    add(buildJsonObject {
                        put("type", "text")
                        put("text", text)
                    })
                })
            },
        ).decode("turn/start") { value -> parseTurnId(value) }
    }

    suspend fun interrupt(
        profile: HostProfile,
        threadId: String,
        turnId: String,
    ): GatewayResult<Unit> = request(
        profile,
        "turn/interrupt",
        buildJsonObject {
            put("threadId", threadId)
            put("turnId", turnId)
        },
    ).mapGateway { Unit }

    private suspend fun request(
        profile: HostProfile,
        method: String,
        params: JsonElement,
    ): GatewayResult<JsonElement> = try {
        rawGateway.rawRequest(profile, method, params)
    } catch (cancelled: CancellationException) {
        throw cancelled
    } catch (failure: Throwable) {
        GatewayResult.Failure(failure.message ?: "Codex request failed.")
    }

}

private data class ThreadListIdentity(val id: String, val workingDirectory: String)

private data class ThreadListPage(
    val threads: List<ThreadSummary>,
    val nextCursor: String?,
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
    val nextCursor = when (val cursor = root["nextCursor"]) {
        null, JsonNull -> null
        is JsonPrimitive -> if (cursor.isString) {
            cursor.stringOrNull() ?: invalid("nextCursor must be a string")
        } else {
            invalid("nextCursor must be a string")
        }
        else -> invalid("nextCursor must be a string or null")
    }
    return ThreadListPage(threads, nextCursor)
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

private fun invalid(message: String): Nothing = throw IllegalArgumentException(message)

private fun <T> GatewayResult<JsonElement>.decode(
    method: String,
    transform: (JsonElement) -> T,
): GatewayResult<T> = when (this) {
    is GatewayResult.Failure -> this
    is GatewayResult.Success -> try {
        GatewayResult.Success(transform(value))
    } catch (failure: Throwable) {
        GatewayResult.Failure(
            message = "Invalid $method response: ${failure.message ?: "malformed payload"}",
            rawError = value,
        )
    }
}

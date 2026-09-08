package dev.remoteagent.mobile

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonPrimitive

internal data class ThreadTurnPresentation(
    val id: String,
    val turnId: String,
    val isLastSegment: Boolean,
    val status: TurnStatus,
    val userMessages: List<CodexItem.UserMessage>,
    val activityItems: List<CodexItem>,
    val responses: List<CodexItem>,
    val activitySummary: String?,
    val activityInitiallyExpanded: Boolean,
    val activityCanCollapse: Boolean,
    val error: ThreadErrorPresentation?,
    val pendingRequests: List<ThreadRequestPresentation>,
)

internal data class ThreadErrorPresentation(
    val title: String,
    val message: String,
    val details: String?,
    val isReconnecting: Boolean,
    val isRetryable: Boolean,
)

internal data class ThreadRequestPresentation(
    val id: String,
    val kind: String,
    val title: String,
    val body: String,
)

/** Rust returns source indices; Kotlin keeps ownership of all message bodies. */
internal fun ThreadSnapshot.conversationSegments(): List<ThreadTurnPresentation> = turns.flatMap { turn ->
    turn.toThreadTurnPresentations(submittedMessages.filter { it.turnId == turn.id })
}

internal fun CodexTurn.toThreadTurnPresentations(submissions: List<SubmittedMessage> = emptyList()): List<ThreadTurnPresentation> {
    val request = buildJsonObject {
        put("operation", "turn")
        put("turn", buildJsonObject {
            put("id", id)
            put("status", presentationJson.encodeToJsonElement(TurnStatus.serializer(), status))
            durationMs?.let { put("durationMs", it) }
            startedAtMs?.let { put("startedAtMs", it) }
            completedAtMs?.let { put("completedAtMs", it) }
            put("items", JsonArray(items.map { it.presentationMetadata() }))
        })
        put("pending", JsonArray(submissions.map { pending -> buildJsonObject {
            pending.afterItemId?.let { put("afterItemId", it) }
            put("item", buildJsonObject { put("id", pending.clientId); put("clientId", pending.clientId); put("type", "userMessage") })
        } }))
    }
    val segments = presentationJson.decodeFromString<List<NativeConversationSegment>>(nativeConversationPresentation(request.toString()))
    return segments.map { segment ->
        val users = mutableListOf<CodexItem.UserMessage>()
        val activities = mutableListOf<CodexItem>()
        val responses = mutableListOf<CodexItem>()
        for (row in segment.rows) {
            val item = if (row.source < items.size) items[row.source] else submissions[row.source - items.size].let {
                CodexItem.UserMessage(it.clientId, it.text, it.clientId, it.imageSources)
            }
            when (row.role) {
                "user" -> users += item as CodexItem.UserMessage
                "activity" -> activities += item
                "response" -> responses += item
                "hidden" -> Unit
                else -> error("Unknown conversation row role: ${row.role}")
            }
        }
        ThreadTurnPresentation(segment.id, id, segment.last, status, users, activities, responses,
            segment.label, segment.initiallyExpanded, segment.collapsible,
            if (segment.last) error?.toThreadErrorPresentation(status) else null,
            if (segment.last) pendingRequests.map(CodexServerRequest::toThreadRequestPresentation) else emptyList())
    }
}

private fun CodexServerRequest.toThreadRequestPresentation(): ThreadRequestPresentation {
    val kindAndTitle = when (method) {
        "item/commandExecution/requestApproval" -> "approval" to "コマンドの承認待ち"
        "item/fileChange/requestApproval" -> "approval" to "ファイル変更の承認待ち"
        "item/permissions/requestApproval" -> "approval" to "権限の承認待ち"
        "item/tool/requestUserInput" -> "userInput" to "回答待ち"
        "mcpServer/elicitation/request" -> "mcpInput" to "MCPからの入力待ち"
        "item/tool/call" -> "toolInput" to "ツールの入力待ち"
        else -> "request" to "Codexからの確認待ち"
    }
    val question = (params["questions"] as? JsonArray)
        ?.firstOrNull()
        ?.let { it as? JsonObject }
        ?.text("question")
    val body = question
        ?: params.text("reason")
        ?: params.text("message")
        ?: params.text("prompt")
        ?: "操作を続けるには応答が必要です"
    return ThreadRequestPresentation(id, kindAndTitle.first, kindAndTitle.second, body)
}

private fun CodexTurnError.toThreadErrorPresentation(status: TurnStatus): ThreadErrorPresentation {
    val kind = when (val info = codexErrorInfo) {
        is JsonPrimitive -> info.contentOrNull
        is JsonObject -> info.keys.firstOrNull()
        else -> null
    }
    val httpStatus = (codexErrorInfo as? JsonObject)
        ?.values?.firstOrNull() as? JsonObject
    val overloaded = kind == "serverOverloaded" || httpStatus?.text("httpStatusCode") == "429"
    val reconnecting = willRetry
    val retryable = status == TurnStatus.Interrupted || kind in setOf(
        "serverOverloaded",
        "internalServerError",
        "httpConnectionFailed",
        "responseStreamConnectionFailed",
        "responseStreamDisconnected",
        "responseTooManyFailedAttempts",
    )
    return ThreadErrorPresentation(
        title = when {
            reconnecting && overloaded -> "サーバーが混み合っています。再接続しています"
            reconnecting -> "再接続しています"
            kind == "contextWindowExceeded" -> "コンテキストの上限に達しました"
            kind == "sessionBudgetExceeded" -> "セッションの上限に達しました"
            kind == "usageLimitExceeded" -> "利用上限に達しました"
            kind == "serverOverloaded" -> "サーバーが混み合っています"
            kind == "cyberPolicy" || kind == "misalignmentPolicyViolation" -> "安全ポリシーにより停止しました"
            kind == "internalServerError" -> "サーバーエラー"
            kind == "unauthorized" -> "認証が必要です"
            kind == "badRequest" -> "リクエストを処理できません"
            kind == "threadRollbackFailed" -> "タスクを元に戻せませんでした"
            kind == "sandboxError" -> "サンドボックスエラー"
            kind in setOf(
                "httpConnectionFailed",
                "responseStreamConnectionFailed",
                "responseStreamDisconnected",
                "responseTooManyFailedAttempts",
            ) -> "接続エラー"
            kind == "activeTurnNotSteerable" -> "この作業中はメッセージを追加できません"
            else -> "エラー"
        },
        message = message,
        details = additionalDetails,
        isReconnecting = reconnecting,
        isRetryable = retryable,
    )
}

internal data class ThreadItemPresentation(
    val id: String,
    val kind: String,
    val title: String,
    val collapsedBody: String,
    val isCollapsible: Boolean,
    val isVisible: Boolean = true,
)

internal fun CodexItem.toThreadItemPresentation(): ThreadItemPresentation {
    val request = buildJsonObject { put("operation", "item"); put("item", presentationMetadata(title = true)) }
    val native = presentationJson.decodeFromString<NativeItemPresentation>(nativeConversationPresentation(request.toString()))
    val body = when (this) {
        is CodexItem.UserMessage -> text
        is CodexItem.AgentMessage -> text
        is CodexItem.Reasoning -> "詳細を表示"
        is CodexItem.CommandExecution -> status.name
        is CodexItem.FileChange -> status.name
        is CodexItem.Unknown -> if (codexType == "imageGeneration") native.title else raw.text("status") ?: "詳細を表示"
    }
    return ThreadItemPresentation(id, native.kind, native.title, body, native.collapsible, native.visible)
}

internal fun CodexItem.expandedThreadItemBody(): String = when (this) {
    is CodexItem.UserMessage -> text
    is CodexItem.AgentMessage -> text
    is CodexItem.Reasoning -> summary
    is CodexItem.CommandExecution -> buildString {
        cwd?.takeIf(String::isNotBlank)?.let { append("cwd: ").append(it).append('\n') }
        append(output)
    }.trim()
    is CodexItem.FileChange -> changes.joinToString("\n\n") { change ->
        "${change.kind.name}: ${change.path}\n${change.diff}".trim()
    }
    is CodexItem.Unknown -> raw.toString()
}

internal fun CodexItem.threadItemContentVersion(): String = when (this) {
    is CodexItem.UserMessage -> "user:${text.length}:${imageSources.hashCode()}"
    is CodexItem.AgentMessage -> "agent:${text.length}"
    is CodexItem.Reasoning -> "reasoning:${summary.length}"
    is CodexItem.CommandExecution -> "command:${status.name}:${command.length}:${output.length}"
    is CodexItem.FileChange -> "files:${status.name}:${changes.sumOf { it.path.length + it.diff.length }}"
    is CodexItem.Unknown -> "unknown:${raw.hashCode()}"
}

private fun JsonObject.text(name: String): String? = get(name)?.jsonPrimitive?.contentOrNull

private val presentationJson = Json { ignoreUnknownKeys = true }
internal expect fun nativeConversationPresentation(request: String): String

@Serializable
private data class NativeConversationRow(val source: Int, val role: String)
@Serializable
private data class NativeConversationSegment(
    val id: String, val last: Boolean, val collapsible: Boolean,
    val initiallyExpanded: Boolean, val label: String?, val rows: List<NativeConversationRow>,
)
@Serializable
private data class NativeItemPresentation(val kind: String, val title: String, val collapsible: Boolean, val visible: Boolean)

/** Only fields consumed by Rust presentation policy cross the language boundary. */
private fun CodexItem.presentationMetadata(title: Boolean = false): JsonObject = buildJsonObject {
    put("id", id)
    when (val item = this@presentationMetadata) {
        is CodexItem.UserMessage -> { put("type", "userMessage"); item.clientId?.let { put("clientId", it) } }
        is CodexItem.AgentMessage -> {
            put("type", "agentMessage")
            item.phase?.let { put("phase", presentationJson.encodeToJsonElement(AgentMessagePhase.serializer(), it)) }
        }
        is CodexItem.Reasoning -> put("type", "reasoning")
        is CodexItem.CommandExecution -> { put("type", "commandExecution"); if (title) put("command", item.command) }
        is CodexItem.FileChange -> { put("type", "fileChange"); put("fileCount", item.changes.size) }
        is CodexItem.Unknown -> {
            put("type", item.codexType)
            if (title) {
                for (key in listOf("server", "tool", "query", "path", "status")) item.raw[key]?.let { put(key, it) }
                (item.raw["review"] as? JsonObject)?.get("status")?.let { status ->
                    put("review", buildJsonObject { put("status", status) })
                }
            }
        }
    }
}

internal fun retainPendingSubmissions(pending: List<SubmittedMessage>, echoed: Sequence<String>): List<SubmittedMessage> {
    if (pending.isEmpty()) return pending
    val request = buildJsonObject {
        put("operation", "reconcile")
        put("pending", JsonArray(pending.map { JsonPrimitive(it.clientId) }))
        put("echoed", JsonArray(echoed.map { JsonPrimitive(it) }.toList()))
    }
    val retained = presentationJson.decodeFromString<List<Int>>(nativeConversationPresentation(request.toString()))
    return if (retained.size == pending.size) pending else retained.map { pending[it] }
}

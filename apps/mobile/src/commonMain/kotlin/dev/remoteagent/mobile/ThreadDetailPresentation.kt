package dev.remoteagent.mobile

import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonPrimitive

internal data class ThreadTurnPresentation(
    val id: String,
    val status: TurnStatus,
    val userMessages: List<CodexItem.UserMessage>,
    val activityItems: List<CodexItem>,
    val responses: List<CodexItem.AgentMessage>,
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

internal fun CodexTurn.toThreadTurnPresentation(): ThreadTurnPresentation {
    val explicitFinalExists = items.any {
        it is CodexItem.AgentMessage && it.phase == AgentMessagePhase.FinalAnswer
    }
    val fallbackFinalIndex = if (explicitFinalExists) {
        -1
    } else {
        items.indexOfLast { it is CodexItem.AgentMessage && it.phase == null }
    }
    val userMessages = mutableListOf<CodexItem.UserMessage>()
    val activityItems = mutableListOf<CodexItem>()
    val responses = mutableListOf<CodexItem.AgentMessage>()
    items.forEachIndexed { index, item ->
        when (item) {
            is CodexItem.UserMessage -> userMessages += item
            is CodexItem.AgentMessage -> when {
                item.phase == AgentMessagePhase.FinalAnswer || index == fallbackFinalIndex -> responses += item
                else -> activityItems += item
            }
            else -> if (item.toThreadItemPresentation().isVisible) activityItems += item
        }
    }
    val activityCanCollapse = status == TurnStatus.Completed &&
        responses.isNotEmpty() && activityItems.isNotEmpty() && pendingRequests.isEmpty()
    return ThreadTurnPresentation(
        id = id,
        status = status,
        userMessages = userMessages,
        activityItems = activityItems,
        responses = responses,
        activitySummary = if (activityItems.isNotEmpty() || status != TurnStatus.Completed) workSummary() else null,
        activityInitiallyExpanded = !activityCanCollapse,
        activityCanCollapse = activityCanCollapse,
        error = error?.toThreadErrorPresentation(status),
        pendingRequests = pendingRequests.map(CodexServerRequest::toThreadRequestPresentation),
    )
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

private fun CodexTurn.workSummary(): String {
    val duration = workDurationMs()?.formatWorkDuration()
    return when (status) {
        TurnStatus.InProgress -> "作業中…"
        TurnStatus.Completed -> duration?.let { "${it}間作業しました" } ?: "作業しました"
        TurnStatus.Interrupted -> duration?.let { "${it}間作業した後に中断しました" } ?: "作業を中断しました"
        TurnStatus.Failed -> duration?.let { "${it}間作業した後に失敗しました" } ?: "作業に失敗しました"
    }
}

private fun CodexTurn.workDurationMs(): Long? = durationMs?.takeIf { it >= 0 }
    ?: startedAtMs?.let { start -> completedAtMs?.takeIf { it >= start }?.minus(start) }

private fun Long.formatWorkDuration(): String {
    if (this < 1_000) return "<1s"
    val totalSeconds = this / 1_000
    val hours = totalSeconds / 3_600
    val minutes = (totalSeconds % 3_600) / 60
    val seconds = totalSeconds % 60
    return buildList {
        if (hours > 0) add("${hours}h")
        if (minutes > 0) add("${minutes}m")
        if (seconds > 0 || isEmpty()) add("${seconds}s")
    }.joinToString(" ")
}

internal data class ThreadItemPresentation(
    val id: String,
    val kind: String,
    val title: String,
    val collapsedBody: String,
    val isCollapsible: Boolean,
    val isVisible: Boolean = true,
)

internal fun CodexItem.toThreadItemPresentation(): ThreadItemPresentation = when (this) {
    is CodexItem.UserMessage -> ThreadItemPresentation(
        id = id,
        kind = "user",
        title = "You",
        collapsedBody = text,
        isCollapsible = false,
    )

    is CodexItem.AgentMessage -> ThreadItemPresentation(
        id = id,
        kind = "agent",
        title = "Codex",
        collapsedBody = text,
        isCollapsible = false,
    )

    is CodexItem.Reasoning -> ThreadItemPresentation(
        id = id,
        kind = "reasoning",
        title = "Reasoning",
        collapsedBody = "Tap to show details",
        isCollapsible = true,
    )

    is CodexItem.CommandExecution -> ThreadItemPresentation(
        id = id,
        kind = "command",
        title = "$ ${command.compactTitle()}",
        collapsedBody = status.name,
        isCollapsible = true,
    )

    is CodexItem.FileChange -> ThreadItemPresentation(
        id = id,
        kind = "fileChange",
        title = "${changes.size} file${if (changes.size == 1) "" else "s"} changed",
        collapsedBody = status.name,
        isCollapsible = true,
    )

    is CodexItem.Unknown -> ThreadItemPresentation(
        id = id,
        kind = "unknown",
        title = unknownItemTitle(),
        collapsedBody = raw.text("status") ?: "詳細を表示",
        isCollapsible = true,
        isVisible = codexType !in setOf("sleep", "enteredReviewMode", "exitedReviewMode"),
    )
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
    is CodexItem.UserMessage -> "user:${text.length}"
    is CodexItem.AgentMessage -> "agent:${text.length}"
    is CodexItem.Reasoning -> "reasoning:${summary.length}"
    is CodexItem.CommandExecution -> "command:${status.name}:${command.length}:${output.length}"
    is CodexItem.FileChange -> "files:${status.name}:${changes.sumOf { it.path.length + it.diff.length }}"
    is CodexItem.Unknown -> "unknown:${raw.hashCode()}"
}

private fun String.compactTitle(maxLength: Int = 120): String {
    val firstLine = lineSequence().firstOrNull().orEmpty().trim()
    return if (firstLine.length <= maxLength) firstLine else firstLine.take(maxLength) + "…"
}

private fun CodexItem.Unknown.unknownItemTitle(): String = when (codexType) {
    "hookPrompt" -> "追加指示"
    "plan" -> "計画を更新しました"
    "mcpToolCall" -> listOfNotNull(raw.text("server"), raw.text("tool")).joinToString(" / ")
        .ifBlank { "MCPツールを実行しました" }
    "dynamicToolCall" -> raw.text("tool")?.let { "${it.compactTitle()}を実行しました" }
        ?: "ツールを実行しました"
    "collabAgentToolCall" -> raw.text("tool")?.let { "サブエージェント: ${it.compactTitle()}" }
        ?: "サブエージェントを操作しました"
    "subAgentActivity" -> "サブエージェントが作業しました"
    "webSearch" -> raw.text("query")?.let { "Webを検索: ${it.compactTitle()}" } ?: "Webを検索しました"
    "imageView" -> raw.text("path")?.let { "画像を確認: ${it.compactTitle()}" } ?: "画像を確認しました"
    "sleep" -> "待機しました"
    "imageGeneration" -> "画像を生成しました"
    "enteredReviewMode" -> "レビューを開始しました"
    "exitedReviewMode" -> "レビューを終了しました"
    "contextCompaction" -> "コンテキストを圧縮しました"
    "automaticApprovalReview" -> when (raw.childObject("review")?.text("status")) {
        "inProgress" -> "承認を自動確認中"
        "denied" -> "自動確認で拒否されました"
        "timedOut" -> "自動確認がタイムアウトしました"
        "aborted" -> "自動確認を中止しました"
        else -> "承認を自動確認しました"
    }
    else -> "Codex item (${codexType.compactTitle()})"
}

private fun JsonObject.text(name: String): String? = get(name)?.jsonPrimitive?.contentOrNull

package dev.remoteagent.mobile

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

/** Accepted inputs remain visible at their send position until the native echo arrives. */
internal fun ThreadSnapshot.conversationSegments(): List<ThreadTurnPresentation> = turns.flatMap { turn ->
    val pending = submittedMessages.filter { it.turnId == turn.id }
    if (pending.isEmpty()) turn.toThreadTurnPresentations() else {
        val anchors = turn.items.mapTo(mutableSetOf()) { it.id }
        val grouped = pending.groupBy { it.afterItemId?.takeIf(anchors::contains) }
        val displayItems = buildList {
            turn.items.forEach { item ->
                add(item)
                grouped[item.id].orEmpty().forEach { add(CodexItem.UserMessage(it.clientId, it.text, it.clientId, it.imageSources)) }
            }
            grouped[null].orEmpty().forEach { add(CodexItem.UserMessage(it.clientId, it.text, it.clientId, it.imageSources)) }
        }
        turn.toThreadTurnPresentations(displayItems)
    }
}

/** Completed work folds within each user exchange, never across a later instruction. */
internal fun CodexTurn.toThreadTurnPresentations(displayItems: List<CodexItem> = items): List<ThreadTurnPresentation> {
    val boundaries = buildList {
        add(0)
        var followsResponse = false
        var exchangeEnd = 0
        var exchangeHasAnswer = false
        displayItems.forEachIndexed { index, item ->
            if (index == exchangeEnd) {
                exchangeEnd = index
                exchangeHasAnswer = false
                while (exchangeEnd < displayItems.size) {
                    val exchangeItem = displayItems[exchangeEnd]
                    if (exchangeEnd > index && exchangeItem is CodexItem.UserMessage) break
                    if (status == TurnStatus.Completed && exchangeItem is CodexItem.AgentMessage &&
                        exchangeItem.phase != AgentMessagePhase.Commentary
                    ) exchangeHasAnswer = true
                    exchangeEnd++
                }
            }
            if (index > 0 && (item is CodexItem.UserMessage ||
                    (!exchangeHasAnswer && followsResponse && item !is CodexItem.AgentMessage && item.isVisibleInConversation()))) {
                add(index)
                followsResponse = false
            }
            if (item is CodexItem.AgentMessage) followsResponse = true
        }
        add(displayItems.size)
    }
    return (0 until boundaries.lastIndex).map { section ->
        val start = boundaries[section]
        val end = boundaries[section + 1]
        var finalAnswer: CodexItem.AgentMessage? = null
        if (status == TurnStatus.Completed) for (index in end - 1 downTo start) {
            val candidate = displayItems[index] as? CodexItem.AgentMessage ?: continue
            if (candidate.phase == AgentMessagePhase.FinalAnswer) {
                finalAnswer = candidate
                break
            }
            if (finalAnswer == null && candidate.phase == null) finalAnswer = candidate
        }
        val last = section == boundaries.lastIndex - 1
        val userMessages = mutableListOf<CodexItem.UserMessage>()
        val activityItems = mutableListOf<CodexItem>()
        val responses = mutableListOf<CodexItem.AgentMessage>()
        for (index in start until end) {
            when (val item = displayItems[index]) {
                is CodexItem.UserMessage -> userMessages += item
                is CodexItem.AgentMessage -> if (finalAnswer != null && item !== finalAnswer) activityItems += item else responses += item
                else -> if (item.isVisibleInConversation()) activityItems += item
            }
        }
        val canCollapse = activityItems.isNotEmpty()
        val firstItem = displayItems.getOrNull(start)
        val sectionId = if (section == 0) id else "$id:${(firstItem as? CodexItem.UserMessage)?.clientId ?: firstItem?.id}"
        ThreadTurnPresentation(
            id = sectionId,
            turnId = id,
            isLastSegment = last,
            status = status,
            userMessages = userMessages,
            activityItems = activityItems,
            responses = responses,
            activitySummary = when {
                canCollapse && finalAnswer != null -> if (last) workSummary() else "作業内容"
                canCollapse -> activityItems.activitySummary().let { summary ->
                    if (last && (status == TurnStatus.Interrupted || status == TurnStatus.Failed)) "${workSummary()}・$summary" else summary
                }
                last && status != TurnStatus.Completed -> workSummary()
                else -> null
            },
            activityInitiallyExpanded = false,
            activityCanCollapse = canCollapse,
            error = if (last) error?.toThreadErrorPresentation(status) else null,
            pendingRequests = if (last) pendingRequests.map(CodexServerRequest::toThreadRequestPresentation) else emptyList(),
        )
    }
}

private fun List<CodexItem>.activitySummary(): String {
    var commands = 0
    var files = 0
    var reasoning = 0
    var tools = 0
    for (item in this) when (item) {
        is CodexItem.CommandExecution -> commands++
        is CodexItem.FileChange -> files += item.changes.size
        is CodexItem.Reasoning -> reasoning++
        else -> tools++
    }
    return buildList {
        if (commands > 0) add("${commands}件のコマンド")
        if (files > 0) add("${files}件のファイル変更")
        if (tools > 0) add("${tools}件のツール操作")
        if (reasoning > 0) add("思考")
        if (isEmpty()) add("作業")
    }.joinToString("、")
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

private fun CodexItem.isVisibleInConversation(): Boolean = this !is CodexItem.Unknown || when (codexType) {
    "sleep", "enteredReviewMode", "exitedReviewMode" -> false
    else -> true
}

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
        isVisible = isVisibleInConversation(),
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
    is CodexItem.UserMessage -> "user:${text.length}:${imageSources.hashCode()}"
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

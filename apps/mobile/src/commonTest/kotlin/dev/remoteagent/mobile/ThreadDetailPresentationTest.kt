package dev.remoteagent.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject

class ThreadDetailPresentationTest {
    @Test
    fun completed_turn_collapses_work_and_keeps_the_final_answer_visible() {
        val turn = CodexTurn(
            id = "turn-1",
            status = TurnStatus.Completed,
            items = listOf(
                CodexItem.UserMessage("user", "fix it"),
                CodexItem.AgentMessage("commentary", "I am checking", AgentMessagePhase.Commentary),
                CodexItem.Reasoning("reasoning", "private work"),
                CodexItem.CommandExecution(
                    id = "command",
                    command = "./gradlew test",
                    output = "ok",
                    status = CommandExecutionStatus.Completed,
                ),
                CodexItem.AgentMessage("final", "Fixed.", AgentMessagePhase.FinalAnswer),
            ),
            startedAtMs = 1_000,
            completedAtMs = 143_000,
            durationMs = 142_000,
        )

        val presentation = turn.toThreadTurnPresentation()

        assertEquals(listOf("user"), presentation.userMessages.map { it.id })
        assertEquals(
            listOf("commentary", "reasoning", "command"),
            presentation.activityItems.map { it.id },
        )
        assertEquals(listOf("final"), presentation.responses.map { it.id })
        assertEquals("2m 22s間作業しました", presentation.activitySummary)
        assertFalse(presentation.activityInitiallyExpanded)
    }

    @Test
    fun active_turn_shows_work_and_terminal_states_use_distinct_summaries() {
        val activity = listOf<CodexItem>(CodexItem.Reasoning("reasoning", "working"))

        val active = CodexTurn("active", TurnStatus.InProgress, activity).toThreadTurnPresentation()
        val interrupted = CodexTurn(
            "interrupted",
            TurnStatus.Interrupted,
            activity,
            durationMs = 12_000,
        ).toThreadTurnPresentation()
        val failed = CodexTurn(
            "failed",
            TurnStatus.Failed,
            activity,
            durationMs = 3_000,
        ).toThreadTurnPresentation()

        assertEquals("作業中…", active.activitySummary)
        assertTrue(active.activityInitiallyExpanded)
        assertFalse(active.activityCanCollapse)
        assertEquals("12s間作業した後に中断しました", interrupted.activitySummary)
        assertTrue(interrupted.activityInitiallyExpanded)
        assertFalse(interrupted.activityCanCollapse)
        assertEquals("3s間作業した後に失敗しました", failed.activitySummary)
        assertTrue(failed.activityInitiallyExpanded)
        assertFalse(failed.activityCanCollapse)
    }

    @Test
    fun the_same_streaming_turn_collapses_automatically_when_it_completes() {
        val streaming = CodexTurn(
            id = "turn-live",
            status = TurnStatus.InProgress,
            items = listOf(
                CodexItem.UserMessage("user", "go"),
                CodexItem.AgentMessage("commentary", "checking…", AgentMessagePhase.Commentary),
                CodexItem.CommandExecution(
                    id = "command",
                    command = "test",
                    output = "running",
                    status = CommandExecutionStatus.InProgress,
                ),
                CodexItem.AgentMessage("answer", "", AgentMessagePhase.FinalAnswer),
            ),
            startedAtMs = 1_000,
        )
        val completed = streaming.copy(
            status = TurnStatus.Completed,
            items = streaming.items.map { item ->
                when (item) {
                    is CodexItem.CommandExecution -> item.copy(
                        output = "passed",
                        status = CommandExecutionStatus.Completed,
                    )
                    is CodexItem.AgentMessage -> if (item.phase == AgentMessagePhase.FinalAnswer) {
                        item.copy(text = "Done.")
                    } else {
                        item.copy(text = "checked")
                    }
                    else -> item
                }
            },
            completedAtMs = 6_000,
            durationMs = 5_000,
        )

        val livePresentation = streaming.toThreadTurnPresentation()
        val completedPresentation = completed.toThreadTurnPresentation()

        assertTrue(livePresentation.activityInitiallyExpanded)
        assertFalse(livePresentation.activityCanCollapse)
        assertEquals("checking…", (livePresentation.activityItems.first() as CodexItem.AgentMessage).text)
        assertFalse(completedPresentation.activityInitiallyExpanded)
        assertTrue(completedPresentation.activityCanCollapse)
        assertEquals("5s間作業しました", completedPresentation.activitySummary)
        assertEquals("Done.", completedPresentation.responses.single().text)
        assertEquals("passed", (completedPresentation.activityItems[1] as CodexItem.CommandExecution).output)
    }

    @Test
    fun duration_falls_back_to_timestamps_and_phase_less_last_agent_message_is_the_response() {
        val turn = CodexTurn(
            id = "turn-legacy",
            status = TurnStatus.Completed,
            items = listOf(
                CodexItem.AgentMessage("progress", "checking"),
                CodexItem.Unknown(
                    id = "tool",
                    codexType = "webSearch",
                    raw = Json.parseToJsonElement("""{"type":"webSearch","query":"Codex"}""").jsonObject,
                ),
                CodexItem.AgentMessage("answer", "done"),
            ),
            startedAtMs = 2_000,
            completedAtMs = 67_000,
        )

        val presentation = turn.toThreadTurnPresentation()

        assertEquals(listOf("progress", "tool"), presentation.activityItems.map { it.id })
        assertEquals(listOf("answer"), presentation.responses.map { it.id })
        assertEquals("1m 5s間作業しました", presentation.activitySummary)
    }

    @Test
    fun messages_are_visible_but_expensive_details_are_collapsed_by_default() {
        val largeBody = "detail".repeat(2_000)
        val items = listOf(
            CodexItem.UserMessage("user", "question"),
            CodexItem.AgentMessage("agent", "answer", AgentMessagePhase.FinalAnswer),
            CodexItem.Reasoning("reasoning", largeBody),
            CodexItem.CommandExecution(
                id = "command",
                command = "rg task",
                output = largeBody,
                status = CommandExecutionStatus.Completed,
            ),
            CodexItem.FileChange(
                id = "files",
                changes = listOf(FileUpdateChange("App.kt", FileUpdateKind.Update, largeBody)),
                status = FileChangeStatus.Completed,
            ),
            CodexItem.Unknown(
                id = "unknown",
                codexType = "futureItem",
                raw = Json.parseToJsonElement("""{"payload":"$largeBody"}""").jsonObject,
            ),
        )

        val rows = items.map(CodexItem::toThreadItemPresentation)

        assertFalse(rows[0].isCollapsible)
        assertEquals("question", rows[0].collapsedBody)
        assertFalse(rows[1].isCollapsible)
        assertEquals("answer", rows[1].collapsedBody)
        rows.drop(2).forEachIndexed { index, row ->
            assertTrue(row.isCollapsible)
            assertFalse(row.collapsedBody.contains(largeBody))
            assertTrue(items[index + 2].expandedThreadItemBody().contains(largeBody))
        }
    }

    @Test
    fun collapsed_titles_are_bounded_even_when_commands_and_paths_are_large() {
        val largeText = "x".repeat(5_000)
        val command = CodexItem.CommandExecution(
            id = "command",
            command = largeText,
            output = "output",
            status = CommandExecutionStatus.Completed,
        ).toThreadItemPresentation()
        val files = CodexItem.FileChange(
            id = "files",
            changes = (1..100).map {
                FileUpdateChange("$largeText/$it", FileUpdateKind.Update, "diff")
            },
            status = FileChangeStatus.Completed,
        ).toThreadItemPresentation()

        assertTrue(command.title.length <= 123)
        assertTrue(files.title.length <= 123)
        assertFalse(files.collapsedBody.contains(largeText))
    }

    @Test
    fun stream_and_terminal_errors_have_distinct_visible_presentations() {
        val retrying = CodexTurn(
            id = "retrying",
            status = TurnStatus.InProgress,
            items = listOf(CodexItem.Reasoning("work", "working")),
            error = CodexTurnError(
                message = "stream disconnected",
                additionalDetails = "attempt 2/5",
                codexErrorInfo = Json.parseToJsonElement(
                    """{"responseStreamDisconnected":{"httpStatusCode":429}}""",
                ),
                willRetry = true,
            ),
        ).toThreadTurnPresentation()
        val failed = CodexTurn(
            id = "failed",
            status = TurnStatus.Failed,
            error = CodexTurnError(
                message = "context is full",
                codexErrorInfo = Json.parseToJsonElement("\"contextWindowExceeded\""),
            ),
        ).toThreadTurnPresentation()

        assertEquals("サーバーが混み合っています。再接続しています", retrying.error?.title)
        assertEquals("attempt 2/5", retrying.error?.details)
        assertTrue(retrying.error?.isReconnecting == true)
        assertEquals("context is full", failed.error?.message)
        assertFalse(failed.error?.isRetryable == true)
        assertTrue(failed.activityInitiallyExpanded)
    }

    @Test
    fun pending_requests_stay_visible_and_prevent_activity_auto_collapse() {
        val methods = listOf(
            "item/commandExecution/requestApproval" to "コマンドの承認待ち",
            "item/fileChange/requestApproval" to "ファイル変更の承認待ち",
            "item/permissions/requestApproval" to "権限の承認待ち",
            "item/tool/requestUserInput" to "回答待ち",
            "mcpServer/elicitation/request" to "MCPからの入力待ち",
            "item/tool/call" to "ツールの入力待ち",
        )
        val turn = CodexTurn(
            id = "turn-requests",
            status = TurnStatus.Completed,
            items = listOf(
                CodexItem.Reasoning("work", "worked"),
                CodexItem.AgentMessage("answer", "done", AgentMessagePhase.FinalAnswer),
            ),
            pendingRequests = methods.mapIndexed { index, (method, _) ->
                CodexServerRequest(
                    id = "request-$index",
                    method = method,
                    params = Json.parseToJsonElement(
                        """{"reason":"needed","questions":[{"question":"Which?"}]}""",
                    ).jsonObject,
                )
            },
        ).toThreadTurnPresentation()

        assertEquals(methods.map { it.second }, turn.pendingRequests.map { it.title })
        assertTrue(turn.pendingRequests.all { it.body.isNotBlank() })
        assertFalse(turn.activityCanCollapse)
        assertTrue(turn.activityInitiallyExpanded)
    }

    @Test
    fun every_official_thread_item_has_an_explicit_visibility_and_presentation_policy() {
        val expected = listOf(
            Triple("hookPrompt", true, "追加指示"),
            Triple("plan", true, "計画を更新しました"),
            Triple("mcpToolCall", true, "server / tool"),
            Triple("dynamicToolCall", true, "toolを実行しました"),
            Triple("collabAgentToolCall", true, "サブエージェント: tool"),
            Triple("subAgentActivity", true, "サブエージェントが作業しました"),
            Triple("webSearch", true, "Webを検索: Codex"),
            Triple("imageView", true, "画像を確認: /tmp/a.png"),
            Triple("sleep", false, "待機しました"),
            Triple("imageGeneration", true, "画像を生成しました"),
            Triple("enteredReviewMode", false, "レビューを開始しました"),
            Triple("exitedReviewMode", false, "レビューを終了しました"),
            Triple("contextCompaction", true, "コンテキストを圧縮しました"),
            Triple("automaticApprovalReview", true, "自動確認で拒否されました"),
        )
        val raw = Json.parseToJsonElement(
            """{"server":"server","tool":"tool","query":"Codex","path":"/tmp/a.png","status":"failed",
                "review":{"status":"denied"}}""",
        ).jsonObject

        expected.forEachIndexed { index, (type, visible, title) ->
            val presentation = CodexItem.Unknown("item-$index", type, raw).toThreadItemPresentation()
            assertEquals(visible, presentation.isVisible, type)
            assertEquals(title, presentation.title, type)
            assertTrue(presentation.collapsedBody.isNotBlank(), type)
        }
    }

    @Test
    fun hidden_state_items_are_removed_before_activity_grouping() {
        val turn = CodexTurn(
            id = "turn-hidden",
            status = TurnStatus.Completed,
            items = listOf(
                CodexItem.Unknown("sleep", "sleep", Json.parseToJsonElement("{}" ).jsonObject),
                CodexItem.Unknown("review-in", "enteredReviewMode", Json.parseToJsonElement("{}" ).jsonObject),
                CodexItem.Unknown("compaction", "contextCompaction", Json.parseToJsonElement("{}" ).jsonObject),
                CodexItem.AgentMessage("answer", "done", AgentMessagePhase.FinalAnswer),
            ),
        ).toThreadTurnPresentation()

        assertEquals(listOf("compaction"), turn.activityItems.map { it.id })
    }

    @Test
    fun every_codex_error_family_has_an_explicit_user_visible_classification() {
        val expected = mapOf(
            "contextWindowExceeded" to "コンテキストの上限に達しました",
            "sessionBudgetExceeded" to "セッションの上限に達しました",
            "usageLimitExceeded" to "利用上限に達しました",
            "serverOverloaded" to "サーバーが混み合っています",
            "cyberPolicy" to "安全ポリシーにより停止しました",
            "misalignmentPolicyViolation" to "安全ポリシーにより停止しました",
            "internalServerError" to "サーバーエラー",
            "unauthorized" to "認証が必要です",
            "badRequest" to "リクエストを処理できません",
            "threadRollbackFailed" to "タスクを元に戻せませんでした",
            "sandboxError" to "サンドボックスエラー",
            "httpConnectionFailed" to "接続エラー",
            "responseStreamConnectionFailed" to "接続エラー",
            "responseStreamDisconnected" to "接続エラー",
            "responseTooManyFailedAttempts" to "接続エラー",
            "activeTurnNotSteerable" to "この作業中はメッセージを追加できません",
            "other" to "エラー",
        )

        expected.forEach { (kind, title) ->
            val error = CodexTurn(
                id = kind,
                status = TurnStatus.Failed,
                error = CodexTurnError(
                    message = "message",
                    codexErrorInfo = Json.parseToJsonElement("\"$kind\""),
                ),
            ).toThreadTurnPresentation().error
            assertEquals(title, error?.title, kind)
        }
    }
}

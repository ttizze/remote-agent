package dev.remoteagent.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject

class ThreadDetailPresentationTest {
    @Test
    fun additional_input_stays_between_the_work_before_and_after_it() {
        val turn = CodexTurn("turn", TurnStatus.InProgress, listOf(
            CodexItem.UserMessage("first", "Start"),
            CodexItem.Reasoning("before", "Existing work"),
            CodexItem.UserMessage("additional", "Change direction"),
            CodexItem.Reasoning("after", "New work"),
        ))
        val order = turn.toThreadTurnPresentations().flatMap { it.userMessages + it.activityItems + it.responses }.map { it.id }
        assertEquals(listOf("first", "before", "additional", "after"), order)
    }

    @Test
    fun commentary_separates_collapsed_command_groups_in_chronological_order() {
        val turn = CodexTurn("turn", TurnStatus.InProgress, listOf(
            CodexItem.UserMessage("user", "fix it"),
            CodexItem.AgentMessage("intro", "Checking", AgentMessagePhase.Commentary),
            CodexItem.CommandExecution("first", "rg task", output = "found", status = CommandExecutionStatus.Completed),
            CodexItem.CommandExecution("second", "cat file", output = "text", status = CommandExecutionStatus.Completed),
            CodexItem.AgentMessage("progress", "Fixing", AgentMessagePhase.Commentary),
            CodexItem.CommandExecution("third", "test", output = "running", status = CommandExecutionStatus.InProgress),
        ))
        val sections = turn.toThreadTurnPresentations()
        assertEquals(listOf("user", "intro", "first", "second", "progress", "third"),
            sections.flatMap { it.userMessages + it.activityItems + it.responses }.map { it.id })
        val groups = sections.filter { it.activityCanCollapse }
        assertEquals(listOf("2件のコマンド", "1件のコマンド"), groups.map { it.activitySummary })
        assertTrue(groups.all { !it.activityInitiallyExpanded })
        assertEquals(listOf("turn:first", "turn:third"), groups.map { it.id })
        assertEquals(listOf("intro", "progress"), sections.flatMap { it.responses }.map { it.id })
    }

    @Test
    fun completion_folds_commentary_and_commands_behind_the_final_answer() {
        val streaming = CodexTurn("turn", TurnStatus.InProgress, listOf(
            CodexItem.AgentMessage("intro", "Checking", AgentMessagePhase.Commentary),
            CodexItem.CommandExecution("command", "test", output = "running", status = CommandExecutionStatus.InProgress),
        ))
        val completed = streaming.copy(status = TurnStatus.Completed, items = listOf(
            streaming.items[0],
            (streaming.items[1] as CodexItem.CommandExecution).copy(output = "passed", status = CommandExecutionStatus.Completed),
            CodexItem.AgentMessage("answer", "Done", AgentMessagePhase.FinalAnswer),
        ))
        val live = streaming.toThreadTurnPresentations().last()
        val done = completed.toThreadTurnPresentations().last()
        assertEquals("turn", done.id)
        assertEquals(listOf("intro", "command"), done.activityItems.map { it.id })
        assertEquals("作業しました", done.activitySummary)
        assertTrue(live.activityCanCollapse && done.activityCanCollapse)
        assertFalse(live.activityInitiallyExpanded)
        assertFalse(done.activityInitiallyExpanded)
        assertEquals("Done", done.responses.single().text)
        assertEquals("passed", (done.activityItems.last() as CodexItem.CommandExecution).output)
    }

    @Test
    fun interrupted_and_failed_groups_are_expandable_and_keep_status_visible() {
        val activity = listOf<CodexItem>(CodexItem.Reasoning("reasoning", "working"))
        for ((status, summary) in listOf(
            TurnStatus.Interrupted to "12s間作業した後に中断しました・思考",
            TurnStatus.Failed to "12s間作業した後に失敗しました・思考",
        )) {
            val group = CodexTurn("turn", status, activity, durationMs = 12_000).toThreadTurnPresentations().single()
            assertEquals(summary, group.activitySummary)
            assertTrue(group.activityCanCollapse)
            assertFalse(group.activityInitiallyExpanded)
        }
    }

    @Test
    fun completed_phase_less_messages_keep_only_the_last_answer_outside_work() {
        val sections = CodexTurn("turn", TurnStatus.Completed, listOf(
            CodexItem.AgentMessage("intro", "Checking"),
            CodexItem.Unknown("sleep", "sleep", Json.parseToJsonElement("{}").jsonObject),
            CodexItem.Reasoning("reasoning", "working"),
            CodexItem.Unknown("review", "enteredReviewMode", Json.parseToJsonElement("{}").jsonObject),
            CodexItem.FileChange("file", listOf(FileUpdateChange("file.kt", FileUpdateKind.Update, "diff")), FileChangeStatus.Completed),
            CodexItem.AgentMessage("answer", "Done"),
        )).toThreadTurnPresentations()
        assertEquals(listOf("intro", "reasoning", "file", "answer"),
            sections.flatMap { it.userMessages + it.activityItems + it.responses }.map { it.id })
        assertEquals("作業しました", sections.single().activitySummary)
        assertEquals(listOf("answer"), sections.single().responses.map { it.id })
    }

    @Test
    fun completed_work_preserves_additional_input_and_duration() {
        val sections = CodexTurn("turn", TurnStatus.Completed, listOf(
            CodexItem.UserMessage("first", "Start"),
            CodexItem.AgentMessage("before", "Checking", AgentMessagePhase.Commentary),
            CodexItem.UserMessage("additional", "Change direction"),
            CodexItem.Reasoning("after", "New work"),
            CodexItem.AgentMessage("answer", "Done", AgentMessagePhase.FinalAnswer),
        ), durationMs = 1_459_000).toThreadTurnPresentations()
        assertEquals(listOf("first", "additional"), sections.flatMap { it.userMessages }.map { it.id })
        assertEquals(listOf("before", "after"), sections.flatMap { it.activityItems }.map { it.id })
        assertEquals(listOf("answer"), sections.flatMap { it.responses }.map { it.id })
        assertEquals("24m 19s間作業しました", sections.last().activitySummary)
    }

    @Test
    fun completed_turn_without_a_final_answer_keeps_commentary_visible() {
        val sections = CodexTurn("turn", TurnStatus.Completed, listOf(
            CodexItem.AgentMessage("commentary", "Still checking", AgentMessagePhase.Commentary),
            CodexItem.Reasoning("reasoning", "Working"),
        )).toThreadTurnPresentations()
        assertEquals(listOf("commentary"), sections.flatMap { it.responses }.map { it.id })
        assertEquals(listOf("reasoning"), sections.flatMap { it.activityItems }.map { it.id })
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
        ).toThreadTurnPresentations().single()
        val failed = CodexTurn(
            id = "failed",
            status = TurnStatus.Failed,
            error = CodexTurnError(
                message = "context is full",
                codexErrorInfo = Json.parseToJsonElement("\"contextWindowExceeded\""),
            ),
        ).toThreadTurnPresentations().single()

        assertEquals("サーバーが混み合っています。再接続しています", retrying.error?.title)
        assertEquals("attempt 2/5", retrying.error?.details)
        assertTrue(retrying.error?.isReconnecting == true)
        assertEquals("context is full", failed.error?.message)
        assertFalse(failed.error?.isRetryable == true)
        assertFalse(failed.activityInitiallyExpanded)
    }

    @Test
    fun pending_requests_stay_visible_outside_collapsed_activity() {
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
        ).toThreadTurnPresentations().single()

        assertEquals(methods.map { it.second }, turn.pendingRequests.map { it.title })
        assertTrue(turn.pendingRequests.all { it.body.isNotBlank() })
        assertTrue(turn.activityCanCollapse)
        assertFalse(turn.activityInitiallyExpanded)
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
        ).toThreadTurnPresentations().single()

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
            ).toThreadTurnPresentations().single().error
            assertEquals(title, error?.title, kind)
        }
    }
}

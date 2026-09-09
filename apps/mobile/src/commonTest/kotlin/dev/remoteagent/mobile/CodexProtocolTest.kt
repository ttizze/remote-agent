package dev.remoteagent.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertTrue
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.jsonObject

class CodexProtocolTest {
    private val json = Json.Default

    @Test
    fun user_message_history_keeps_attachment_names_and_paths() {
        val item =
            assertIs<CodexItem.UserMessage>(
                codexItem(
                    json.parseToJsonElement(
                        """
            {"type":"userMessage","id":"user-1","text":"Review these","content":[
                {"type":"text","text":"Review these"},
                {"type":"localImage","path":"/uploads/photo.png"},
                {"type":"image","url":"data:image/png;base64,aW1hZ2U="},
                {"type":"mention","name":"notes.txt","path":"/uploads/notes.txt"}
            ]}
        """
                    )
                )
            )
        assertEquals(
            json.parseToJsonElement("""["/uploads/photo.png","data:image/png;base64,aW1hZ2U="]"""),
            json.encodeToJsonElement<CodexItem>(item).jsonObject["imageSources"],
        )
        assertEquals("Review these\n添付: notes.txt (/uploads/notes.txt)", item.text)
    }

    @Test
    fun unknown_item_keeps_type_and_complete_raw_payload() {
        val item =
            codexItem(
                json.parseToJsonElement(
                    """
            {"type":"futureItem","id":"item-7","newField":{"nested":true}}
        """
                )
            )

        val unknown = assertIs<CodexItem.Unknown>(item)
        assertEquals("item-7", unknown.id)
        assertEquals("futureItem", unknown.codexType)
        assertEquals(true, unknown.raw["newField"]?.jsonObject?.get("nested")?.toString()?.toBoolean())
    }

    @Test
    fun unknown_notification_does_not_change_display_cache() {
        val event =
            RawCodexMessage.Notification(
                "item/futureThing",
                json.parseToJsonElement(
                    """
                {"threadId":"thread-1","turnId":"turn-1","extra":{"value":42}}
            """
                ),
                JsonObject(mapOf("vendor" to json.parseToJsonElement("true"))),
            )
        val unknown = assertEventKind(ConversationEventKind.Unknown, event)
        val cache = MobileCache()
        val updated = applyLiveEvent(cache, "host-1", unknown, MobileCacheLimits())

        assertEquals("item/futureThing", unknown.method)
        assertEquals(42, unknown.paramsObject["extra"]?.jsonObject?.get("value")?.toString()?.toInt())
        assertEquals("true", unknown.extensions["vendor"]?.toString())
        kotlin.test.assertSame(cache, updated)
    }

    @Test
    fun raw_parser_keeps_server_request_id_and_extensions() {
        val message =
            assertIs<RawCodexMessage.ServerRequest>(
                parseRawCodexMessage(
                    """
            {"id":"approval-1","method":"item/commandExecution/requestApproval","params":{"command":"ls"},"vendor":true}
        """
                )
            )

        assertEquals("approval-1", message.id.toString().trim('"'))
        assertEquals("item/commandExecution/requestApproval", message.method)
        assertEquals("true", message.extensions["vendor"]?.toString())
        assertTrue(message.params.toString().contains("ls"))
    }

    @Test
    fun thread_status_uses_the_current_status_object_and_completed_status_uses_nested_turn_status() {
        val summary =
            codexThreadSummary(
                json.parseToJsonElement(
                    """
            {"id":"thread-1","status":{"type":"active","activeFlags":[]},"cwd":"/workspace","projectId":"project-1"}
        """
                )
            )
        assertIs<ThreadStatus.Active>(summary.status)
        assertEquals("project-1", summary.projectId)

        val event =
            RawCodexMessage.Notification(
                "turn/completed",
                json.parseToJsonElement(
                    """
                {"threadId":"thread-1","turn":{"id":"turn-1","status":"interrupted"}}
            """
                ),
            )
        assertEquals(
            TurnStatus.Interrupted,
            assertEventKind(ConversationEventKind.TurnCompleted, event)
                .let { codexTurn(it.paramsObject.getValue("turn")) }
                .status,
        )
    }

    @Test
    fun current_wrappers_fields_and_file_change_kind_are_projected() {
        val snapshot =
            codexThreadFromResponse(
                json.parseToJsonElement(
                    """
            {"thread":{"id":"thread-1","cwd":"/workspace","createdAt":11,"updatedAt":13,
             "turns":[{"id":"turn-1","status":"inProgress","startedAt":10,"completedAt":12,"durationMs":2345,"items":[
                 {"type":"agentMessage","id":"agent-1","text":"final","phase":"final_answer"},
                 {"type":"commandExecution","id":"command-1","aggregatedOutput":"build output","status":"declined"},
                 {"type":"fileChange","id":"file-1","status":"completed","changes":[
                     {"path":"Main.swift","kind":{"type":"delete","move_path":null},"diff":"-old"}
                 ]}
             ]}]}}
        """
                )
            )

        assertEquals("/workspace", snapshot.summary.workingDirectory.path)
        assertEquals(11, snapshot.summary.createdAtMs)
        assertEquals(13, snapshot.summary.updatedAtMs)
        assertEquals("turn-1", snapshot.turns.single().id)
        assertEquals(TurnStatus.InProgress, snapshot.turns.single().status)
        assertEquals(10_000, snapshot.turns.single().startedAtMs)
        assertEquals(12_000, snapshot.turns.single().completedAtMs)
        assertEquals(2_345, snapshot.turns.single().durationMs)
        val agent = assertIs<CodexItem.AgentMessage>(snapshot.turns.single().items[0])
        assertEquals(AgentMessagePhase.FinalAnswer, agent.phase)
        val command = assertIs<CodexItem.CommandExecution>(snapshot.turns.single().items[1])
        assertEquals("build output", command.output)
        assertEquals(CommandExecutionStatus.Declined, command.status)
        val file = assertIs<CodexItem.FileChange>(snapshot.turns.single().items[2])
        assertEquals(FileUpdateKind.Delete, file.changes.single().kind)
        assertEquals("-old", file.changes.single().diff)
    }

    @Test
    fun turn_id_alias_is_preserved_when_history_uses_turn_id() {
        val snapshot =
            codexThreadSnapshot(
                json.parseToJsonElement(
                    """
            {"id":"thread-1","turns":[{"turnId":"turn-1","status":"completed","items":[]}]}
        """
                )
            )

        assertEquals("turn-1", snapshot.turns.single().id)
    }

    @Test
    fun turn_events_keep_server_timing() {
        val started =
            assertEventKind(
                ConversationEventKind.TurnStarted,
                RawCodexMessage.Notification(
                    "turn/started",
                    json.parseToJsonElement(
                        """
                {"threadId":"thread-1","turn":{"id":"turn-1","status":"inProgress","startedAt":10}}
            """
                    ),
                ),
            )
        val completed =
            assertEventKind(
                ConversationEventKind.TurnCompleted,
                RawCodexMessage.Notification(
                    "turn/completed",
                    json.parseToJsonElement(
                        """
                {"threadId":"thread-1","turn":{"id":"turn-1","status":"completed",
                 "startedAt":10,"completedAt":12,"durationMs":2345}}
            """
                    ),
                ),
            )

        assertEquals(10_000, codexTurn(started.paramsObject.getValue("turn")).startedAtMs)
        assertEquals(10_000, codexTurn(completed.paramsObject.getValue("turn")).startedAtMs)
        assertEquals(12_000, codexTurn(completed.paramsObject.getValue("turn")).completedAtMs)
        assertEquals(2_345, codexTurn(completed.paramsObject.getValue("turn")).durationMs)
    }

    @Test
    fun project_projection_uses_official_roots_and_position_fields() {
        val project =
            codexProject(
                json.parseToJsonElement(
                    """
            {"id":"project-1","name":"remote-agent","roots":[{"path":"/workspace/remote-agent"}],
             "metadata":{},"position":7,"createdAt":11,"updatedAt":13}
        """
                )
            )

        assertEquals("project-1", project.id)
        assertEquals("remote-agent", project.name)
        assertEquals(listOf(WorkingDirectory("/workspace/remote-agent")), project.roots)
        assertEquals(7, project.position)
    }

    @Test
    fun reasoning_summary_text_delta_is_a_known_live_event() {
        val event =
            RawCodexMessage.Notification(
                "item/reasoning/summaryTextDelta",
                json.parseToJsonElement(
                    """
                {"threadId":"thread-1","turnId":"turn-1","itemId":"item-1","delta":"summary"}
            """
                ),
            )
        val summary = assertEventKind(ConversationEventKind.ReasoningSummaryDelta, event)
        assertEquals("thread-1", summary.threadId)
        assertEquals("turn-1", summary.turnId)
        assertEquals("item-1", summary.paramsObject.string("itemId"))
        assertEquals("summary", summary.paramsObject.string("delta"))
    }

    @Test
    fun retrying_and_terminal_error_notifications_keep_the_complete_error_state() {
        val retrying =
            assertEventKind(
                ConversationEventKind.Error,
                RawCodexMessage.Notification(
                    "error",
                    json.parseToJsonElement(
                        """
                {"threadId":"thread-1","turnId":"turn-1","willRetry":true,
                 "error":{"message":"stream disconnected","additionalDetails":"attempt 2 of 5",
                  "codexErrorInfo":{"responseStreamDisconnected":{"httpStatusCode":429}}}}
            """
                    ),
                ),
            )
        val terminal =
            assertEventKind(
                ConversationEventKind.Error,
                RawCodexMessage.Notification(
                    "error",
                    json.parseToJsonElement(
                        """
                {"threadId":"thread-1","turnId":"turn-1","willRetry":false,
                 "error":{"message":"context is full","codexErrorInfo":"contextWindowExceeded"}}
            """
                    ),
                ),
            )

        assertTrue(retrying.paramsObject.boolean("willRetry") == true)
        assertEquals("stream disconnected", codexTurnError(retrying.paramsObject.childObject("error")!!).message)
        assertEquals("attempt 2 of 5", codexTurnError(retrying.paramsObject.childObject("error")!!).additionalDetails)
        assertEquals(
            "429",
            codexTurnError(retrying.paramsObject.childObject("error")!!)
                .codexErrorInfo
                ?.jsonObject
                ?.get("responseStreamDisconnected")
                ?.jsonObject
                ?.get("httpStatusCode")
                ?.toString(),
        )
        assertFalse(terminal.paramsObject.boolean("willRetry") == true)
        assertEquals(
            "contextWindowExceeded",
            codexTurnError(terminal.paramsObject.childObject("error")!!).codexErrorInfo?.toString()?.trim('"'),
        )
    }

    @Test
    fun every_conversation_server_request_retains_its_pending_request_fields() {
        val methods =
            listOf(
                "item/commandExecution/requestApproval",
                "item/fileChange/requestApproval",
                "item/permissions/requestApproval",
                "item/tool/requestUserInput",
                "mcpServer/elicitation/request",
                "item/tool/call",
            )

        methods.forEachIndexed { index, method ->
            val message =
                RawCodexMessage.ServerRequest(
                    id = json.parseToJsonElement("\"request-$index\""),
                    method = method,
                    params = json.parseToJsonElement("""{"threadId":"thread-1","turnId":"turn-1","reason":"needed"}"""),
                )
            val event = assertEventKind(ConversationEventKind.RequestStarted, message)
            assertEquals("request-$index", event.serverRequest().id)
            assertEquals(method, event.serverRequest().method)
            assertEquals("needed", event.serverRequest().params["reason"]?.toString()?.trim('"'))
        }
    }

    @Test
    fun failed_turn_completion_keeps_the_nested_error() {
        val event =
            assertEventKind(
                ConversationEventKind.TurnCompleted,
                RawCodexMessage.Notification(
                    "turn/completed",
                    json.parseToJsonElement(
                        """{"threadId":"thread-1","turn":{"id":"turn-1","status":"failed",
                    "error":{"message":"context full","codexErrorInfo":"contextWindowExceeded"}}}"""
                    ),
                ),
            )

        assertEquals(TurnStatus.Failed, codexTurn(event.paramsObject.getValue("turn")).status)
        assertEquals("context full", codexTurn(event.paramsObject.getValue("turn")).error?.message)
        assertEquals(
            "contextWindowExceeded",
            codexTurn(event.paramsObject.getValue("turn")).error?.codexErrorInfo?.toString()?.trim('"'),
        )
    }

    @Test
    fun thread_status_and_every_auto_approval_review_state_are_typed() {
        val status =
            assertEventKind(
                ConversationEventKind.ThreadStatusChanged,
                RawCodexMessage.Notification(
                    "thread/status/changed",
                    json.parseToJsonElement(
                        """{"threadId":"thread-1","status":{"type":"active","activeFlags":["waitingOnApproval"]}}"""
                    ),
                ),
            )
        assertEquals(ThreadStatus.Active(listOf("waitingOnApproval")), codexThreadStatus(status.paramsObject["status"]))

        listOf("inProgress", "approved", "denied", "timedOut", "aborted").forEach { reviewStatus ->
            val method =
                if (reviewStatus == "inProgress") {
                    "item/autoApprovalReview/started"
                } else {
                    "item/autoApprovalReview/completed"
                }
            val review =
                assertEventKind(
                    ConversationEventKind.GuardianReviewChanged,
                    RawCodexMessage.Notification(
                        method,
                        json.parseToJsonElement(
                            """{"threadId":"thread-1","turnId":"turn-1","reviewId":"review-1",
                        "review":{"status":"$reviewStatus","rationale":"checked"},
                        "action":{"type":"command","command":"git status"}}"""
                        ),
                    ),
                )
            assertEquals("review-1", review.paramsObject.string("reviewId"))
            assertEquals(reviewStatus, review.paramsObject.childObject("review")?.string("status"))
        }
    }

    private fun <T : RawCodexMessage> assertEventKind(kind: ConversationEventKind, event: T): T {
        assertEquals(kind, event.kind)
        return event
    }
}

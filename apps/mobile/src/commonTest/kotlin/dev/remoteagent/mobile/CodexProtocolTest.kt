package dev.remoteagent.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertIs
import kotlin.test.assertTrue
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject

class CodexProtocolTest {
    private val json = Json.Default

    @Test
    fun unknown_item_keeps_type_and_complete_raw_payload() {
        val item = codexItem(json.parseToJsonElement("""
            {"type":"futureItem","id":"item-7","newField":{"nested":true}}
        """))

        val unknown = assertIs<CodexItem.Unknown>(item)
        assertEquals("item-7", unknown.id)
        assertEquals("futureItem", unknown.codexType)
        assertEquals(true, unknown.raw["newField"]?.jsonObject?.get("nested")?.toString()?.toBoolean())
    }

    @Test
    fun unknown_notification_is_retained_in_thread_event_and_cache() {
        val event = codexThreadEvent(
            "item/futureThing",
            json.parseToJsonElement("""
                {"threadId":"thread-1","turnId":"turn-1","extra":{"value":42}}
            """),
            JsonObject(mapOf("vendor" to json.parseToJsonElement("true"))),
        )
        val unknown = assertIs<ThreadEvent.Unknown>(event)
        val cache = applyLiveEvent(MobileCache(), "host-1", unknown, MobileCacheLimits())

        assertEquals("item/futureThing", unknown.method)
        assertEquals(42, unknown.raw["extra"]?.jsonObject?.get("value")?.toString()?.toInt())
        assertEquals("true", unknown.extensions["vendor"]?.toString())
        assertEquals(listOf(unknown), cache.profile("host-1").unknownEvents)
    }

    @Test
    fun raw_parser_keeps_server_request_id_and_extensions() {
        val message = assertIs<RawCodexMessage.ServerRequest>(parseRawCodexMessage("""
            {"id":"approval-1","method":"item/commandExecution/requestApproval","params":{"command":"ls"},"vendor":true}
        """))

        assertEquals("approval-1", message.id.toString().trim('"'))
        assertEquals("item/commandExecution/requestApproval", message.method)
        assertEquals("true", message.extensions["vendor"]?.toString())
        assertTrue(message.params.toString().contains("ls"))
    }

    @Test
    fun thread_status_accepts_string_and_completed_status_prefers_nested_turn_status() {
        val summary = codexThreadSummary(json.parseToJsonElement("""
            {"id":"thread-1","status":"active","cwd":"/workspace","projectId":"project-1"}
        """))
        assertIs<ThreadStatus.Active>(summary.status)
        assertEquals("project-1", summary.projectId)

        val event = codexThreadEvent(
            "turn/completed",
            json.parseToJsonElement("""
                {"threadId":"thread-1","turnId":"turn-1","status":"completed",
                 "turn":{"status":"interrupted"}}
            """),
        )
        assertEquals(TurnStatus.Interrupted, assertIs<ThreadEvent.TurnCompleted>(event).status)
    }

    @Test
    fun project_projection_uses_official_roots_and_position_fields() {
        val project = codexProject(json.parseToJsonElement("""
            {"id":"project-1","name":"remote-agent","roots":[{"path":"/workspace/remote-agent"}],
             "metadata":{},"position":7,"createdAt":11,"updatedAt":13}
        """))

        assertEquals("project-1", project.id)
        assertEquals("remote-agent", project.name)
        assertEquals(listOf(WorkingDirectory("/workspace/remote-agent")), project.roots)
        assertEquals(7, project.position)
    }

    @Test
    fun reasoning_summary_text_delta_is_a_known_live_event() {
        val event = codexThreadEvent(
            "item/reasoning/summaryTextDelta",
            json.parseToJsonElement("""
                {"threadId":"thread-1","turnId":"turn-1","itemId":"item-1","delta":"summary"}
            """),
        )
        assertIs<ThreadEvent.ReasoningSummaryDelta>(event)
    }
}

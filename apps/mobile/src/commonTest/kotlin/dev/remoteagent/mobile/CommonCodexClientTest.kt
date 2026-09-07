package dev.remoteagent.mobile

import kotlin.coroutines.Continuation
import kotlin.coroutines.EmptyCoroutineContext
import kotlin.coroutines.startCoroutine
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertIs
import kotlin.test.assertTrue
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.jsonObject

class CommonCodexClientTest {
    private val json = Json.Default
    private val profile = HostProfile("runner-1", "Host", "wss://relay.example.test/socket/websocket", "runner-1", "device-key-ref")

    @Test
    fun initial_list_request_returns_only_the_requested_title_window_in_one_response() {
        val gateway = FakeRawGateway()
        gateway.enqueue("host/thread/list", success("""{"data":[{"id":"recent","name":"Recent title","projectId":"project","cwd":"/workspace"}],"projects":[{"id":"project","name":"Project","roots":[]}],"moreProjectIds":["project"],"hasMoreChats":true,"hasMoreProjects":true}"""))
        val page = assertIs<GatewayResult.Success<ThreadListPage>>(runSuspend { CommonCodexClient(gateway).listThreads(profile) }).value
        assertEquals(1, gateway.calls.size, "Initial display must not wait for older pages or read thread bodies")
        assertEquals(json.parseToJsonElement("""{"titleOnly":true,"projectLimit":5,"chatLimit":5,"projectThreadLimits":{}}"""), gateway.calls.single().params)
        assertEquals("Recent title", page.threads.single().name)
        assertEquals("", page.threads.single().preview)
        assertEquals(listOf("project"), page.projects.map { it.id })
        assertEquals(setOf("project"), page.moreProjectIds)
        assertTrue(page.hasMoreChats)
        assertTrue(page.hasMoreProjects)
    }

    @Test
    fun model_catalog_follows_pages_and_exposes_supported_efforts() {
        val gateway = FakeRawGateway()
        gateway.enqueue("model/list",
            success("""{"data":[{"id":"a","model":"model-a","displayName":"Model A","defaultReasoningEffort":"medium","supportedReasoningEfforts":[{"reasoningEffort":"medium"},{"reasoningEffort":"high"}]}],"nextCursor":"page-2"}"""),
            success("""{"data":[{"id":"b","model":"model-b","displayName":"Model B","defaultReasoningEffort":"low","supportedReasoningEfforts":[{"reasoningEffort":"low"}]}]}"""),
        )
        val result = assertIs<GatewayResult.Success<List<CodexModel>>>(runSuspend { CommonCodexClient(gateway).listModels(profile) }).value
        assertEquals(listOf("model-a", "model-b"), result.map { it.model })
        assertEquals(listOf("medium", "high"), result.first().reasoningEfforts)
        assertEquals(json.parseToJsonElement("\"page-2\""), gateway.calls.last().params.jsonObject["cursor"])
    }

    @Test
    fun selected_model_and_effort_apply_to_new_tasks_and_existing_turns_per_host() {
        val gateway = FakeRawGateway()
        gateway.enqueue("host/thread/start", success("""{"thread":{"id":"thread-1","cwd":"/workspace","turns":[]}}"""))
        gateway.enqueue("thread/resume", success("{}"))
        gateway.enqueue("turn/start", success("""{"turn":{"id":"turn-1"}}"""), success("""{"turn":{"id":"turn-2"}}"""), success("""{"turn":{"id":"turn-3"}}"""))
        val client = CommonCodexClient(gateway)
        client.setTurnOptions(profile.id, CodexTurnOptions("model-b", "high"))
        assertIs<GatewayResult.Success<ThreadSnapshot>>(runSuspend { client.startThread(profile, "/workspace") })
        assertIs<GatewayResult.Success<String>>(runSuspend { client.startTurn(profile, "thread-1", "/workspace", "first", resume = false, clientUserMessageId = "client-1") })
        assertIs<GatewayResult.Success<String>>(runSuspend { client.startTurn(profile, "thread-1", "/workspace", "next", resume = false, clientUserMessageId = "client-1") })
        val other = profile.copy(hostIdentity = "other-host")
        assertIs<GatewayResult.Success<String>>(runSuspend { client.startTurn(other, "thread-2", "/other", "hello", resume = true, clientUserMessageId = "client-1") })
        val starts = gateway.calls.filter { it.method == "turn/start" }
        starts.take(2).forEach {
            assertEquals(json.parseToJsonElement("\"model-b\""), it.params.jsonObject["model"])
            assertEquals(json.parseToJsonElement("\"high\""), it.params.jsonObject["effort"])
        }
        assertEquals(null, starts.last().params.jsonObject["model"])
        assertEquals(null, starts.last().params.jsonObject["effort"])
        assertEquals(json.parseToJsonElement("\"model-b\""), gateway.calls.first().params.jsonObject["model"])
    }

    @Test
    fun first_turn_does_not_try_to_resume_an_unmaterialized_thread() {
        val gateway = FakeRawGateway()
        gateway.enqueue("thread/resume", GatewayResult.Failure("no rollout found for thread id"))
        gateway.enqueue("turn/start", success("""{"turn":{"id":"first-turn"}}"""))
        val result = runSuspend { CommonCodexClient(gateway).startTurn(profile, "new", "/workspace", "First message", resume = false, clientUserMessageId = "client-1") }
        assertEquals("first-turn", assertIs<GatewayResult.Success<String>>(result).value)
        assertEquals(listOf("turn/start"), gateway.calls.map { it.method })
    }

    @Test
    fun deferred_history_preserves_markers_and_expansion_returns_full_output() {
        val gateway = FakeRawGateway()
        gateway.enqueue("host/thread/read", success("""{"thread":{"id":"thread-1","cwd":"/workspace","turns":[{"id":"turn-1","status":"completed","deferredItemIds":["item-1"],"items":[{"id":"item-1","type":"commandExecution","command":"cat log","status":"completed"}]}]}}"""))
        gateway.enqueue("host/thread/item/read", success("""{"item":{"id":"item-1","type":"commandExecution","command":"cat log","status":"completed","aggregatedOutput":"first\nlast"}}"""))
        val client = CommonCodexClient(gateway, deferItemDetails = true)
        val history = assertIs<GatewayResult.Success<ThreadReadResult>>(runSuspend { client.readThread(profile, "thread-1") }).value
        assertEquals(json.parseToJsonElement("""["item-1"]"""), history.thread.turns.single().raw?.get("deferredItemIds"))
        assertEquals(json.parseToJsonElement("""{"threadId":"thread-1","includeTurns":true,"paginateHistory":true,"deferItemDetails":true}"""), gateway.calls.single().params)
        val body = runSuspend { client.readItemDetails(profile, "thread-1", "turn-1", "item-1") }
        assertEquals("first\nlast", assertIs<GatewayResult.Success<String>>(body).value)
        assertEquals(json.parseToJsonElement("""{"threadId":"thread-1","turnId":"turn-1","itemId":"item-1"}"""), gateway.calls.last().params)
    }

    @Test
    fun deferred_detail_rejects_another_item_and_reports_remote_failure() {
        val gateway = FakeRawGateway()
        gateway.enqueue("host/thread/item/read", success("""{"item":{"id":"other","type":"agentMessage","text":"wrong"}}"""), GatewayResult.Failure("offline"))
        val client = CommonCodexClient(gateway)
        assertIs<GatewayResult.Failure>(runSuspend { client.readItemDetails(profile, "thread-1", "turn-1", "item-1") })
        assertEquals(GatewayResult.Failure("offline"), runSuspend { client.readItemDetails(profile, "thread-1", "turn-1", "item-1") })
    }

    @Test
    fun new_conversation_without_a_directory_uses_the_host_default() {
        val gateway = FakeRawGateway()
        gateway.enqueue("host/thread/start", success("""{"thread":{"id":"thread-new","cwd":"/default","turns":[]}}"""))
        val started = assertIs<GatewayResult.Success<ThreadSnapshot>>(runSuspend {
            CommonCodexClient(gateway).startThread(profile, "")
        }).value
        assertEquals("thread-new", started.summary.id)
        assertEquals("/default", started.summary.workingDirectory.path)
        assertEquals(json.parseToJsonElement("{}"), gateway.calls.single().params)
    }

    @Test
    fun title_list_expansion_and_search_keep_independent_section_limits() {
        val gateway = FakeRawGateway()
        gateway.enqueue("host/thread/list", success("""{"data":[],"projects":[],"moreProjectIds":[],"hasMoreChats":false,"hasMoreProjects":false}"""))
        val result = runSuspend { CommonCodexClient(gateway).listThreads(profile, ThreadListQuery(15, 25, mapOf("project" to 15), "needle")) }
        assertIs<GatewayResult.Success<ThreadListPage>>(result)
        assertEquals(json.parseToJsonElement("""{"titleOnly":true,"projectLimit":15,"chatLimit":25,"projectThreadLimits":{"project":15},"searchTerm":"needle"}"""), gateway.calls.single().params)
    }

    @Test
    fun malformed_title_list_and_remote_errors_are_not_presented_as_an_empty_list() {
        val gateway = FakeRawGateway()
        gateway.enqueue("host/thread/list", success("""{"data":[],"projects":[],"hasMoreChats":false,"hasMoreProjects":false}"""), GatewayResult.Failure("offline"))
        val client = CommonCodexClient(gateway)
        assertIs<GatewayResult.Failure>(runSuspend { client.listThreads(profile) })
        assertEquals(GatewayResult.Failure("offline"), runSuspend { client.listThreads(profile) })
    }

    @Test
    fun baseline_operations_emit_native_payloads_and_decode_results() {
        val gateway = FakeRawGateway()
        gateway.enqueue(
            "host/thread/read",
            success("""{"thread":{"id":"thread-1","cwd":"/workspace","turns":[]}}"""),
        )
        gateway.enqueue(
            "host/thread/start",
            success("""{"thread":{"id":"thread-2","cwd":"/workspace","turns":[]}}"""),
        )
        gateway.enqueue("thread/resume", success("{}"))
        gateway.enqueue("turn/start", success("""{"turn":{"id":"turn-1"}}"""))
        gateway.enqueue("turn/interrupt", success("{}"))
        val client = CommonCodexClient(gateway)

        val read = runSuspend { client.readThread(profile, "thread-1") }
        val started = runSuspend { client.startThread(profile, "/workspace") }
        val turn = runSuspend { client.startTurn(profile, "thread-1", "/workspace", "hello", resume = true, clientUserMessageId = "client-1") }
        val interrupted = runSuspend { client.interrupt(profile, "thread-1", "turn-1") }

        assertEquals("thread-1", assertIs<GatewayResult.Success<ThreadReadResult>>(read).value.thread.summary.id)
        assertEquals("thread-2", assertIs<GatewayResult.Success<ThreadSnapshot>>(started).value.summary.id)
        assertEquals("turn-1", assertIs<GatewayResult.Success<String>>(turn).value)
        assertIs<GatewayResult.Success<Unit>>(interrupted)
        assertEquals(
            listOf("host/thread/read", "host/thread/start", "thread/resume", "turn/start", "turn/interrupt"),
            gateway.calls.map(RequestCall::method),
        )
        assertEquals(
            json.parseToJsonElement("""{"threadId":"thread-1","includeTurns":true,"paginateHistory":true}"""),
            gateway.calls[0].params,
        )
        assertEquals(
            json.parseToJsonElement("""{"cwd":"/workspace"}"""),
            gateway.calls[1].params,
        )
        assertEquals(
            json.parseToJsonElement("""{"threadId":"thread-1","cwd":"/workspace"}"""),
            gateway.calls[2].params,
        )
        assertEquals(
            json.parseToJsonElement("""
                {"threadId":"thread-1","clientUserMessageId":"client-1","input":[{"type":"text","text":"hello","text_elements":[]}]}
            """),
            gateway.calls[3].params,
        )
        assertEquals(
            json.parseToJsonElement("""{"threadId":"thread-1","turnId":"turn-1"}"""),
            gateway.calls[4].params,
        )
    }

    @Test
    fun failed_resume_does_not_emit_turn_start() {
        val gateway = FakeRawGateway()
        val remoteFailure = GatewayResult.Failure("resume rejected")
        gateway.enqueue("thread/resume", remoteFailure)

        val result = runSuspend {
            CommonCodexClient(gateway).startTurn(profile, "thread-1", "/workspace", "hello", resume = true, clientUserMessageId = "client-1")
        }

        assertEquals(remoteFailure, result)
        assertEquals(listOf("thread/resume"), gateway.calls.map(RequestCall::method))
    }

    @Test
    fun start_turn_uses_the_explicit_cwd_without_relying_on_previous_thread_operations() {
        val gateway = FakeRawGateway()
        gateway.enqueue("host/thread/read", success("""{"thread":{"id":"thread-1","cwd":"/old","turns":[]}}"""))
        gateway.enqueue("thread/resume", success("{}"), success("{}"))
        gateway.enqueue("turn/start", success("""{"turn":{"id":"turn-1"}}"""), success("""{"turn":{"id":"turn-2"}}"""))
        val client = CommonCodexClient(gateway)

        runSuspend { client.readThread(profile, "thread-1") }
        val first = runSuspend { client.startTurn(profile, "thread-1", "/first", "one", resume = true, clientUserMessageId = "client-1") }
        val second = runSuspend { client.startTurn(profile, "thread-1", "/second", "two", resume = true, clientUserMessageId = "client-1") }

        assertEquals("turn-1", assertIs<GatewayResult.Success<String>>(first).value)
        assertEquals("turn-2", assertIs<GatewayResult.Success<String>>(second).value)
        assertEquals(
            listOf(
                json.parseToJsonElement("""{"threadId":"thread-1","cwd":"/first"}"""),
                json.parseToJsonElement("""{"threadId":"thread-1","cwd":"/second"}"""),
            ),
            gateway.calls.filter { it.method == "thread/resume" }.map(RequestCall::params),
        )
    }

    @Test
    fun steer_turn_forwards_the_expected_turn_and_text_input() {
        val gateway = FakeRawGateway()
        gateway.enqueue("turn/steer", success("{}"))

        val result = runSuspend {
            CommonCodexClient(gateway).steerTurn(profile, "thread-1", "turn-1", "keep going", clientUserMessageId = "client-1")
        }

        assertIs<GatewayResult.Success<Unit>>(result)
        assertEquals(listOf("turn/steer"), gateway.calls.map(RequestCall::method))
        assertEquals(
            json.parseToJsonElement(
                """{"threadId":"thread-1","expectedTurnId":"turn-1","clientUserMessageId":"client-1","input":[{"type":"text","text":"keep going","text_elements":[]}]}""",
            ),
            gateway.calls.single().params,
        )
    }

    @Test
    fun queue_turn_forwards_a_fresh_client_message_id_and_decodes_the_server_submission() {
        val gateway = FakeRawGateway()
        gateway.enqueue(
            "thread/queue/add",
            success("""
                {"queuedSubmission":{"id":"queued-1","clientUserMessageId":"client-1",
                "input":[{"type":"text","text":"hello","text_elements":[]}]}}
            """),
            success("""
                {"queuedSubmission":{"id":"queued-2","clientUserMessageId":"client-2",
                "input":[{"type":"text","text":"again","text_elements":[]}]}}
            """),
        )
        val client = CommonCodexClient(gateway)

        val first = runSuspend { client.queueTurn(profile, "thread-1", "hello", clientUserMessageId = "client-1") }
        val second = runSuspend { client.queueTurn(profile, "thread-1", "again", clientUserMessageId = "client-2") }

        assertEquals("queued-1", assertIs<GatewayResult.Success<String>>(first).value)
        assertEquals("queued-2", assertIs<GatewayResult.Success<String>>(second).value)
        assertEquals(listOf("thread/queue/add", "thread/queue/add"), gateway.calls.map(RequestCall::method))
        assertEquals(
            json.parseToJsonElement(
                """{"threadId":"thread-1","clientUserMessageId":"client-1","input":[{"type":"text","text":"hello","text_elements":[]}]}""",
            ),
            gateway.calls[0].params,
        )
        assertEquals(
            json.parseToJsonElement(
                """{"threadId":"thread-1","clientUserMessageId":"client-2","input":[{"type":"text","text":"again","text_elements":[]}]}""",
            ),
            gateway.calls[1].params,
        )
    }

    @Test
    fun malformed_queue_submission_response_is_a_failure_with_the_raw_payload() {
        val gateway = FakeRawGateway()
        val malformed = json.parseToJsonElement("""{"queuedSubmission":{"clientUserMessageId":"client-1","input":[]}}""")
        gateway.enqueue("thread/queue/add", GatewayResult.Success(malformed))

        val result = runSuspend {
            CommonCodexClient(gateway)
                .queueTurn(profile, "thread-1", "hello", clientUserMessageId = "client-1")
        }

        val failure = assertIs<GatewayResult.Failure>(result)
        assertTrue(failure.message.contains("Invalid thread/queue/add response"))
        assertEquals(malformed, failure.rawError)
    }

    @Test
    fun malformed_success_responses_become_failures_with_the_raw_payload() {
        val gateway = FakeRawGateway()
        val malformed = json.parseToJsonElement("""{"thread":{"cwd":"/workspace"}}""")
        gateway.enqueue("host/thread/read", GatewayResult.Success(malformed))

        val result = runSuspend { CommonCodexClient(gateway).readThread(profile, "thread-1") }

        val failure = assertIs<GatewayResult.Failure>(result)
        assertTrue(failure.message.contains("Invalid host/thread/read response"))
        assertEquals(malformed, failure.rawError)
    }

    @Test
    fun attachments_use_uploaded_host_paths_in_start_steer_and_queue() {
        val gateway = FakeRawGateway()
        gateway.enqueue("thread/resume", success("{}"))
        gateway.enqueue("turn/start", success("""{"turn":{"id":"turn-1"}}"""))
        gateway.enqueue("turn/steer", success("{}"))
        gateway.enqueue("thread/queue/add", success("""{"queuedSubmission":{"id":"queue-1"}}"""))
        val client = CommonCodexClient(gateway)
        val attachments = listOf(CodexAttachment("/uploads/image.png", "image.png", true), CodexAttachment("/uploads/notes.txt", "notes.txt", false))
        runSuspend { client.startTurn(profile, "thread-1", "/workspace", "", attachments, resume = true, clientUserMessageId = "client-1") }
        runSuspend { client.steerTurn(profile, "thread-1", "turn-1", "", attachments, clientUserMessageId = "client-1") }
        runSuspend { client.queueTurn(profile, "thread-1", "", attachments, clientUserMessageId = "client-1") }
        val input = json.parseToJsonElement("""[{"type":"localImage","path":"/uploads/image.png"},{"type":"mention","path":"/uploads/notes.txt","name":"notes.txt"}]""")
        gateway.calls.filter { it.method != "thread/resume" }.forEach {
            assertEquals(input, (it.params as kotlinx.serialization.json.JsonObject)["input"])
        }
    }

    private fun success(raw: String): GatewayResult<JsonElement> =
        GatewayResult.Success(json.parseToJsonElement(raw))

    private data class RequestCall(
        val profile: HostProfile,
        val method: String,
        val params: JsonElement,
    )

    private class FakeRawGateway : RawCodexGateway {
        val calls = mutableListOf<RequestCall>()
        private val responses = mutableMapOf<String, MutableList<GatewayResult<JsonElement>>>()

        fun enqueue(method: String, vararg results: GatewayResult<JsonElement>) {
            responses.getOrPut(method) { mutableListOf() }.addAll(results)
        }

        override suspend fun rawRequest(
            profile: HostProfile,
            method: String,
            params: JsonElement,
        ): GatewayResult<JsonElement> {
            calls += RequestCall(profile, method, params)
            return responses[method]?.let { queued ->
                if (queued.isEmpty()) null else queued.removeAt(0)
            }
                ?: GatewayResult.Failure("unexpected raw request: $method")
        }

        override fun subscribeRaw(
            profile: HostProfile,
            onMessage: (RawCodexMessage) -> Unit,
            onClosed: (String) -> Unit,
        ): HostEventSubscription = HostEventSubscription {}

        override suspend fun respondResult(
            profile: HostProfile,
            requestId: JsonElement,
            result: JsonElement,
        ): GatewayResult<Unit> = GatewayResult.Success(Unit)

        override suspend fun respondError(
            profile: HostProfile,
            requestId: JsonElement,
            error: JsonElement,
        ): GatewayResult<Unit> = GatewayResult.Success(Unit)
    }

    private fun <T> runSuspend(block: suspend () -> T): T {
        var completion: Result<T>? = null
        block.startCoroutine(object : Continuation<T> {
            override val context = EmptyCoroutineContext

            override fun resumeWith(result: Result<T>) {
                completion = result
            }
        })
        return requireNotNull(completion).getOrThrow()
    }
}

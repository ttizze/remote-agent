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

class CommonCodexClientTest {
    private val json = Json.Default
    private val profile = HostProfile("host-1", "Host", listOf("127.0.0.1:49152"), "device-1")

    @Test
    fun list_threads_uses_cwd_paginates_and_deduplicates_by_id_and_working_directory() {
        val gateway = FakeRawGateway()
        gateway.enqueue(
            "thread/list",
            success("""
                {"data":[
                    {"id":"thread-1","cwd":"/workspace","preview":"one"},
                    {"id":"thread-1","cwd":"/other","preview":"same id, other cwd"},
                    {"id":"thread-2","cwd":"/workspace","preview":"two"}
                ],"nextCursor":"page-2"}
            """),
            success("""
                {"data":[
                    {"id":"thread-2","cwd":"/workspace","preview":"duplicate"},
                    {"id":"thread-3","cwd":"/workspace","preview":"three"}
                ],"nextCursor":null}
            """),
        )

        val result = runSuspend {
            CommonCodexClient(gateway, CommonCodexClientLimits(maxThreadItems = 4, maxThreadPages = 3))
                .listThreads(profile, "/workspace")
        }

        val success = assertIs<GatewayResult.Success<List<ThreadSummary>>>(result)
        assertEquals(listOf("thread-1", "thread-1", "thread-2", "thread-3"), success.value.map(ThreadSummary::id))
        assertEquals(listOf("thread/list", "thread/list"), gateway.calls.map(RequestCall::method))
        assertEquals(
            json.parseToJsonElement("""{"limit":4,"cwd":"/workspace"}"""),
            gateway.calls[0].params,
        )
        assertEquals(
            json.parseToJsonElement("""{"limit":1,"cwd":"/workspace","cursor":"page-2"}"""),
            gateway.calls[1].params,
        )
    }

    @Test
    fun list_threads_rejects_a_repeated_cursor() {
        val gateway = FakeRawGateway()
        gateway.enqueue(
            "thread/list",
            success("""{"data":[],"nextCursor":"same"}"""),
            success("""{"data":[],"nextCursor":"same"}"""),
        )

        val result = runSuspend {
            CommonCodexClient(gateway).listThreads(profile, "")
        }

        val failure = assertIs<GatewayResult.Failure>(result)
        assertTrue(failure.message.contains("non-progressing cursor"))
        assertEquals(2, gateway.calls.size)
        assertEquals(json.parseToJsonElement("""{"limit":64}"""), gateway.calls[0].params)
    }

    @Test
    fun list_threads_fails_when_the_page_budget_would_be_exceeded() {
        val gateway = FakeRawGateway()
        gateway.enqueue(
            "thread/list",
            success("""{"data":[],"nextCursor":"page-2"}"""),
        )

        val result = runSuspend {
            CommonCodexClient(gateway, CommonCodexClientLimits(maxThreadPages = 1))
                .listThreads(profile, "/workspace")
        }

        val failure = assertIs<GatewayResult.Failure>(result)
        assertTrue(failure.message.contains("page limit"))
        assertEquals(1, gateway.calls.size)
    }

    @Test
    fun baseline_operations_emit_native_payloads_and_decode_results() {
        val gateway = FakeRawGateway()
        gateway.enqueue(
            "thread/read",
            success("""{"thread":{"id":"thread-1","cwd":"/workspace","turns":[]}}"""),
        )
        gateway.enqueue(
            "thread/start",
            success("""{"thread":{"id":"thread-2","cwd":"/workspace","turns":[]}}"""),
        )
        gateway.enqueue("thread/resume", success("{}"))
        gateway.enqueue("turn/start", success("""{"turn":{"id":"turn-1"}}"""))
        gateway.enqueue("turn/interrupt", success("{}"))
        val client = CommonCodexClient(gateway)

        val read = runSuspend { client.readThread(profile, "thread-1") }
        val started = runSuspend { client.startThread(profile, "/workspace") }
        // This overload proves that a read/start/list result supplies the cwd
        // required by the existing HostGateway.startTurn signature.
        val turn = runSuspend { client.startTurn(profile, "thread-1", "hello") }
        val interrupted = runSuspend { client.interrupt(profile, "thread-1", "turn-1") }

        assertEquals("thread-1", assertIs<GatewayResult.Success<ThreadReadResult>>(read).value.thread.summary.id)
        assertEquals("thread-2", assertIs<GatewayResult.Success<ThreadSnapshot>>(started).value.summary.id)
        assertEquals("turn-1", assertIs<GatewayResult.Success<String>>(turn).value)
        assertIs<GatewayResult.Success<Unit>>(interrupted)
        assertEquals(
            listOf("thread/read", "thread/start", "thread/resume", "turn/start", "turn/interrupt"),
            gateway.calls.map(RequestCall::method),
        )
        assertEquals(
            json.parseToJsonElement("""{"threadId":"thread-1","includeTurns":true}"""),
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
                {"threadId":"thread-1","input":[{"type":"text","text":"hello"}]}
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
            CommonCodexClient(gateway).startTurn(profile, "thread-1", "/workspace", "hello")
        }

        assertEquals(remoteFailure, result)
        assertEquals(listOf("thread/resume"), gateway.calls.map(RequestCall::method))
    }

    @Test
    fun malformed_success_responses_become_failures_with_the_raw_payload() {
        val gateway = FakeRawGateway()
        val malformed = json.parseToJsonElement("""{"thread":{"cwd":"/workspace"}}""")
        gateway.enqueue("thread/read", GatewayResult.Success(malformed))

        val result = runSuspend { CommonCodexClient(gateway).readThread(profile, "thread-1") }

        val failure = assertIs<GatewayResult.Failure>(result)
        assertTrue(failure.message.contains("Invalid thread/read response"))
        assertEquals(malformed, failure.rawError)
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
        var result: Result<T>? = null
        block.startCoroutine(object : Continuation<T> {
            override val context = EmptyCoroutineContext

            override fun resumeWith(value: Result<T>) {
                result = value
            }
        })
        return requireNotNull(result).getOrThrow()
    }
}

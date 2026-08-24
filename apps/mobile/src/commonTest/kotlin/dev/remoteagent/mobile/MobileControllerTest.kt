package dev.remoteagent.mobile

import kotlin.coroutines.Continuation
import kotlin.coroutines.EmptyCoroutineContext
import kotlin.coroutines.startCoroutine
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertIs
import kotlin.test.assertTrue
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject

class MobileControllerTest {
    private val profile = HostProfile("host-1", "Host", listOf("127.0.0.1:49152"), "device-1")
    private val thread = snapshot("thread-1")

    @Test
    fun connect_subscribes_lists_and_refreshes_the_selected_thread() {
        val gateway = FakeHostGateway().apply {
            readResult = ThreadReadResult(thread, emptyList())
        }
        val controller = controller(gateway, selectedThreadId = "thread-1")

        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }

        assertEquals(listOf("/workspace"), gateway.listCwds)
        assertEquals(listOf("thread-1"), gateway.readIds)
        assertIs<LoadPhase.Ready>(controller.state.selectedView.threadList)
        assertIs<LoadPhase.Ready>(controller.state.selectedView.threadDetail)
        assertEquals(thread, controller.state.cache.snapshot(profile.hostIdentity, "thread-1"))
    }

    @Test
    fun events_during_read_are_applied_after_snapshot_and_other_threads_do_not_mix() {
        val gateway = FakeHostGateway().apply {
            readResult = ThreadReadResult(thread, emptyList())
            listResult = GatewayResult.Success(listOf(thread.summary))
        }
        val controller = controller(gateway)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        val transitions = mutableListOf<AppState>()
        controller.observe { transitions += it }
        gateway.readHook = {
            gateway.emit(notification("item/agentMessage/delta", """
                {"threadId":"thread-1","turnId":"turn-1","itemId":"item-1","delta":" new"}
            """))
            gateway.emit(notification("turn/completed", """
                {"threadId":"thread-1","turnId":"turn-1","status":"completed"}
            """))
            gateway.emit(notification("item/agentMessage/delta", """
                {"threadId":"thread-2","turnId":"turn-2","itemId":"item-2","delta":"wrong"}
            """))
        }

        runSuspend { controller.readThread(profile, "thread-1") }

        val result = requireNotNull(controller.state.cache.snapshot(profile.hostIdentity, "thread-1"))
        val resultTurn = result.turns.single()
        assertEquals(TurnStatus.Completed, resultTurn.status)
        assertEquals("old new", (resultTurn.items.single() as CodexItem.AgentMessage).text)
        assertEquals(3, controller.state.cache.profile(profile.hostIdentity).rawMessages.size)
        val rawIndex = transitions.indexOfFirst {
            it.cache.profile(profile.hostIdentity).rawMessages.isNotEmpty()
        }
        val snapshotIndex = transitions.indexOfFirst {
            it.cache.snapshot(profile.hostIdentity, "thread-1")?.turns?.single()?.items?.single() ==
                CodexItem.AgentMessage("item-1", "old new")
        }
        assertTrue(rawIndex >= 0 && snapshotIndex > rawIndex)
    }

    @Test
    fun read_buffer_overflow_surfaces_a_retryable_failure() {
        val gateway = FakeHostGateway().apply {
            readResult = ThreadReadResult(thread, emptyList())
        }
        val controller = controller(
            gateway,
            cacheLimits = MobileCacheLimits(maxApproximateBytes = 64),
        )
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        gateway.readHook = {
            gateway.emit(notification("item/agentMessage/delta", """
                {"threadId":"thread-1","turnId":"turn-1","itemId":"item-1","delta":"${"x".repeat(100)}"}
            """))
        }

        runSuspend { controller.readThread(profile, "thread-1") }

        val phase = controller.state.selectedView.threadDetail
        assertIs<LoadPhase.Failed>(phase)
        assertTrue(phase.message.contains("同期"))
    }

    @Test
    fun starting_without_a_working_directory_keeps_the_loaded_list_visible() {
        val gateway = FakeHostGateway()
        val controller = controller(gateway, workingDirectory = "")
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }

        runSuspend { controller.startThread(profile) }

        assertIs<LoadPhase.Ready>(controller.state.selectedView.threadList)
        assertEquals("作業ディレクトリを指定してください。", controller.state.selectedView.notice)
        assertEquals(0, gateway.startCalls)
    }

    private fun controller(
        gateway: FakeHostGateway,
        selectedThreadId: String? = null,
        workingDirectory: String = "/workspace",
        cacheLimits: MobileCacheLimits = MobileCacheLimits(),
    ): MobileController {
        val initialCache = reconcileThreadRead(
            MobileCache(),
            profile.hostIdentity,
            ThreadReadResult(thread, emptyList()),
            cacheLimits,
        )
        return MobileController(
            gateway = gateway,
            repository = InMemoryMobileRepository(
                AppState(
                    profiles = listOf(profile),
                    selectedProfileId = profile.hostIdentity,
                    profileViews = mapOf(
                        profile.hostIdentity to ProfileViewState(
                            workingDirectoryPath = workingDirectory,
                            selectedThreadId = selectedThreadId,
                        ),
                    ),
                    cache = initialCache,
                ),
            ),
            cacheLimits = cacheLimits,
        )
    }

    private fun notification(method: String, params: String): RawCodexMessage.Notification =
        RawCodexMessage.Notification(method, Json.parseToJsonElement(params))

    private fun snapshot(id: String): ThreadSnapshot = ThreadSnapshot(
        summary = ThreadSummary(
            id = id,
            preview = "preview",
            workingDirectory = WorkingDirectory("/workspace"),
            createdAtMs = 1,
            updatedAtMs = 1,
            status = ThreadStatus.Idle,
        ),
        turns = listOf(
            CodexTurn(
                id = "turn-1",
                status = TurnStatus.InProgress,
                items = listOf(CodexItem.AgentMessage("item-1", "old")),
            ),
        ),
    )

    private class FakeHostGateway : HostGateway {
        var connectResult: GatewayResult<Unit> = GatewayResult.Success(Unit)
        var listResult: GatewayResult<List<ThreadSummary>> = GatewayResult.Success(emptyList())
        var readResult: GatewayResult<ThreadReadResult> = GatewayResult.Success(
            ThreadReadResult(ThreadSnapshot(summary("default")), emptyList()),
        )
        var startResult: GatewayResult<ThreadSnapshot> = GatewayResult.Failure("not configured")
        var turnResult: GatewayResult<String> = GatewayResult.Success("turn-1")
        var interruptResult: GatewayResult<Unit> = GatewayResult.Success(Unit)
        var readHook: (() -> Unit)? = null
        var callback: ((RawCodexMessage) -> Unit)? = null
        var listCwds = mutableListOf<String>()
        var readIds = mutableListOf<String>()
        var startCalls = 0

        override suspend fun pair(payload: PairingQrPayload): GatewayResult<HostProfile> = GatewayResult.Failure("unused")
        override suspend fun discover(profile: HostProfile): GatewayResult<List<String>> = GatewayResult.Success(profile.addresses)
        override suspend fun connect(profile: HostProfile): GatewayResult<Unit> = connectResult
        override suspend fun listThreads(profile: HostProfile, cwd: String): GatewayResult<List<ThreadSummary>> {
            listCwds += cwd
            return listResult
        }
        override suspend fun readThread(profile: HostProfile, threadId: String): GatewayResult<ThreadReadResult> {
            readIds += threadId
            readHook?.invoke()
            return readResult
        }
        override suspend fun startThread(profile: HostProfile, cwd: String): GatewayResult<ThreadSnapshot> {
            startCalls += 1
            return startResult
        }
        override suspend fun startTurn(profile: HostProfile, threadId: String, text: String): GatewayResult<String> = turnResult
        override suspend fun interrupt(profile: HostProfile, threadId: String, turnId: String): GatewayResult<Unit> = interruptResult
        override suspend fun rawRequest(profile: HostProfile, method: String, params: JsonElement): GatewayResult<JsonElement> =
            GatewayResult.Success(JsonObject(emptyMap()))
        override fun subscribeRaw(profile: HostProfile, onMessage: (RawCodexMessage) -> Unit): HostEventSubscription {
            callback = onMessage
            return HostEventSubscription { if (callback === onMessage) callback = null }
        }
        override suspend fun respondResult(profile: HostProfile, requestId: JsonElement, result: JsonElement): GatewayResult<Unit> =
            GatewayResult.Success(Unit)
        override suspend fun respondError(profile: HostProfile, requestId: JsonElement, error: JsonElement): GatewayResult<Unit> =
            GatewayResult.Success(Unit)

        fun emit(message: RawCodexMessage) {
            callback?.invoke(message)
        }
    }

    private fun summary(id: String) = ThreadSummary(
        id = id,
        preview = "preview",
        workingDirectory = WorkingDirectory("/workspace"),
        createdAtMs = 1,
        updatedAtMs = 1,
        status = ThreadStatus.Idle,
    )

    private fun runSuspend(block: suspend () -> Unit) {
        var result: Result<Unit>? = null
        block.startCoroutine(object : Continuation<Unit> {
            override val context = EmptyCoroutineContext
            override fun resumeWith(value: Result<Unit>) {
                result = value
            }
        })
        (result ?: error("test coroutine suspended unexpectedly")).getOrThrow()
    }
}

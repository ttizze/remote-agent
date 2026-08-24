package dev.remoteagent.mobile

import kotlin.coroutines.Continuation
import kotlin.coroutines.EmptyCoroutineContext
import kotlin.coroutines.startCoroutine
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertIs
import kotlin.test.assertNull
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
            readResult = GatewayResult.Success(ThreadReadResult(thread, emptyList()))
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
            readResult = GatewayResult.Success(ThreadReadResult(thread, emptyList()))
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
            readResult = GatewayResult.Success(ThreadReadResult(thread, emptyList()))
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

    @Test
    fun starting_turn_passes_the_cached_snapshot_working_directory() {
        val gateway = FakeHostGateway().apply {
            listResult = GatewayResult.Success(listOf(summary("thread-1", "/cached/workspace")))
        }
        val controller = controller(gateway, selectedThreadId = null)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }

        runSuspend { controller.startTurn(profile, "thread-1", "hello") }

        assertEquals(listOf("/workspace"), gateway.turnCwds)
        assertEquals(listOf("hello"), gateway.turnTexts)
    }

    @Test
    fun starting_turn_without_a_cached_working_directory_sends_no_request_and_asks_for_retry() {
        val gateway = FakeHostGateway().apply {
            listResult = GatewayResult.Success(listOf(summary("thread-1", "")))
        }
        val controller = controller(gateway, selectedThreadId = null, cachedThread = null)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }

        runSuspend { controller.startTurn(profile, "thread-1", "hello") }

        assertTrue(gateway.turnCwds.isEmpty())
        assertEquals("タスクの作業ディレクトリが不明です。タスク一覧を更新してください", controller.state.selectedView.notice)
    }

    @Test
    fun connect_completion_after_disconnect_is_ignored_and_reconnect_starts_a_new_generation() {
        val gateway = FakeHostGateway()
        val controller = controller(gateway, selectedThreadId = null)
        gateway.connectHook = { controller.disconnect(profile.hostIdentity) }

        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }

        assertIs<ConnectionPhase.Disconnected>(controller.state.selectedView.connection)
        assertTrue(gateway.callback == null)
        assertTrue(gateway.listCwds.isEmpty())
        assertEquals(1, gateway.disconnectCalls)

        gateway.connectHook = null
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }

        assertIs<ConnectionPhase.Connected>(controller.state.selectedView.connection)
        assertEquals(1, gateway.listCwds.size)
    }

    @Test
    fun list_completion_after_disconnect_does_not_replace_the_newly_disconnected_view() {
        val gateway = FakeHostGateway()
        val controller = controller(gateway, selectedThreadId = null)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        gateway.listResult = GatewayResult.Success(listOf(summary("stale-thread", "/stale")))
        gateway.listHook = { controller.disconnect(profile.hostIdentity) }

        runSuspend { controller.listThreads(profile) }

        assertIs<ConnectionPhase.Disconnected>(controller.state.selectedView.connection)
        assertIs<LoadPhase.Idle>(controller.state.selectedView.threadList)
        assertNull(controller.state.cache.profile(profile.hostIdentity).threadList.firstOrNull())

        gateway.listHook = null
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        assertIs<ConnectionPhase.Connected>(controller.state.selectedView.connection)
    }

    @Test
    fun read_completion_after_disconnect_does_not_apply_snapshot_or_notice() {
        val gateway = FakeHostGateway().apply {
            readResult = GatewayResult.Success(ThreadReadResult(thread, emptyList()))
        }
        val controller = controller(gateway, selectedThreadId = null, cachedThread = null)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        gateway.readHook = { controller.disconnect(profile.hostIdentity) }

        runSuspend { controller.readThread(profile, "thread-1") }

        assertIs<ConnectionPhase.Disconnected>(controller.state.selectedView.connection)
        assertIs<LoadPhase.Idle>(controller.state.selectedView.threadDetail)
        assertNull(controller.state.cache.snapshot(profile.hostIdentity, "thread-1"))
        assertNull(controller.state.selectedView.notice)
    }

    @Test
    fun turn_completion_after_disconnect_does_not_publish_a_stale_failure() {
        val gateway = FakeHostGateway().apply {
            listResult = GatewayResult.Success(listOf(summary("thread-1", "/workspace")))
            turnResult = GatewayResult.Failure("stale turn failure")
        }
        val controller = controller(gateway, selectedThreadId = null, cachedThread = null)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        gateway.turnHook = { controller.disconnect(profile.hostIdentity) }

        runSuspend { controller.startTurn(profile, "thread-1", "hello") }

        assertIs<ConnectionPhase.Disconnected>(controller.state.selectedView.connection)
        assertNull(controller.state.selectedView.notice)
    }

    @Test
    fun interrupt_completion_after_disconnect_does_not_clear_or_publish_stale_state() {
        val gateway = FakeHostGateway().apply {
            interruptResult = GatewayResult.Failure("stale interrupt failure")
        }
        val controller = controller(gateway, selectedThreadId = null)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        gateway.interruptHook = { controller.disconnect(profile.hostIdentity) }

        runSuspend { controller.interrupt(profile, "thread-1", "turn-1") }

        assertIs<ConnectionPhase.Disconnected>(controller.state.selectedView.connection)
        assertNull(controller.state.selectedView.interruptingTurnId)
        assertNull(controller.state.selectedView.notice)
    }

    @Test
    fun disconnect_cancels_the_subscription_before_reducing_disconnected() {
        val gateway = FakeHostGateway()
        val controller = controller(gateway)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        var cancelledBeforeObserver = false
        controller.observe {
            if (it.profileViews[profile.hostIdentity]?.connection == ConnectionPhase.Disconnected) {
                cancelledBeforeObserver = gateway.subscriptionCancelCount > 0
            }
        }

        controller.disconnect(profile.hostIdentity)

        assertTrue(cancelledBeforeObserver)
        assertEquals(1, gateway.subscriptionCancelCount)
        assertNull(gateway.callback)
    }

    @Test
    fun explicit_disconnect_retires_subscription_closes_gateway_and_ignores_close_failure() {
        val gateway = FakeHostGateway().apply {
            disconnectResult = GatewayResult.Failure("close failed")
        }
        val controller = controller(gateway)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }

        runSuspend { controller.disconnect(profile) }

        assertEquals(1, gateway.disconnectCalls)
        assertTrue(gateway.subscriptionWasRetiredAtDisconnect)
        assertIs<ConnectionPhase.Disconnected>(controller.state.selectedView.connection)
        assertNull(controller.state.selectedView.notice)
    }

    private fun controller(
        gateway: FakeHostGateway,
        selectedThreadId: String? = null,
        workingDirectory: String = "/workspace",
        cachedThread: ThreadSnapshot? = thread,
        cacheLimits: MobileCacheLimits = MobileCacheLimits(),
    ): MobileController {
        val initialCache = cachedThread?.let {
            reconcileThreadRead(
                MobileCache(),
                profile.hostIdentity,
                ThreadReadResult(it, emptyList()),
                cacheLimits,
            )
        } ?: MobileCache()
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
        var readResult: GatewayResult<ThreadReadResult> = GatewayResult.Failure("not configured")
        var startResult: GatewayResult<ThreadSnapshot> = GatewayResult.Failure("not configured")
        var turnResult: GatewayResult<String> = GatewayResult.Success("turn-1")
        var interruptResult: GatewayResult<Unit> = GatewayResult.Success(Unit)
        var disconnectResult: GatewayResult<Unit> = GatewayResult.Success(Unit)
        var readHook: (() -> Unit)? = null
        var callback: ((RawCodexMessage) -> Unit)? = null
        var listCwds = mutableListOf<String>()
        var readIds = mutableListOf<String>()
        var startCalls = 0
        var turnCwds = mutableListOf<String>()
        var turnTexts = mutableListOf<String>()
        var connectHook: (() -> Unit)? = null
        var listHook: (() -> Unit)? = null
        var turnHook: (() -> Unit)? = null
        var interruptHook: (() -> Unit)? = null
        var subscriptionCancelCount = 0
        var disconnectCalls = 0
        var subscriptionWasRetiredAtDisconnect = false

        override suspend fun pair(payload: PairingQrPayload): GatewayResult<HostProfile> = GatewayResult.Failure("unused")
        override suspend fun discover(profile: HostProfile): GatewayResult<List<String>> = GatewayResult.Success(profile.addresses)
        override suspend fun connect(profile: HostProfile): GatewayResult<Unit> {
            connectHook?.invoke()
            return connectResult
        }
        override suspend fun disconnect(profile: HostProfile): GatewayResult<Unit> {
            disconnectCalls += 1
            subscriptionWasRetiredAtDisconnect = callback == null
            return disconnectResult
        }
        override suspend fun listThreads(profile: HostProfile, cwd: String): GatewayResult<List<ThreadSummary>> {
            listCwds += cwd
            listHook?.invoke()
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
        override suspend fun startTurn(profile: HostProfile, threadId: String, cwd: String, text: String): GatewayResult<String> {
            turnCwds += cwd
            turnTexts += text
            turnHook?.invoke()
            return turnResult
        }
        override suspend fun interrupt(profile: HostProfile, threadId: String, turnId: String): GatewayResult<Unit> {
            interruptHook?.invoke()
            return interruptResult
        }
        override suspend fun rawRequest(profile: HostProfile, method: String, params: JsonElement): GatewayResult<JsonElement> =
            GatewayResult.Success(JsonObject(emptyMap()))
        override fun subscribeRaw(profile: HostProfile, onMessage: (RawCodexMessage) -> Unit): HostEventSubscription {
            callback = onMessage
            return HostEventSubscription {
                subscriptionCancelCount += 1
                if (callback === onMessage) callback = null
            }
        }
        override suspend fun respondResult(profile: HostProfile, requestId: JsonElement, result: JsonElement): GatewayResult<Unit> =
            GatewayResult.Success(Unit)
        override suspend fun respondError(profile: HostProfile, requestId: JsonElement, error: JsonElement): GatewayResult<Unit> =
            GatewayResult.Success(Unit)

        fun emit(message: RawCodexMessage) {
            callback?.invoke(message)
        }
    }

    private fun summary(id: String, cwd: String = "/workspace") = ThreadSummary(
        id = id,
        preview = "preview",
        workingDirectory = WorkingDirectory(cwd),
        createdAtMs = 1,
        updatedAtMs = 1,
        status = ThreadStatus.Idle,
    )

    private fun runSuspend(block: suspend () -> Unit) {
        var completion: Result<Unit>? = null
        block.startCoroutine(object : Continuation<Unit> {
            override val context = EmptyCoroutineContext
            override fun resumeWith(result: Result<Unit>) {
                completion = result
            }
        })
        (completion ?: error("test coroutine suspended unexpectedly")).getOrThrow()
    }
}

package dev.remoteagent.mobile

import kotlin.coroutines.Continuation
import kotlin.coroutines.CoroutineContext
import kotlin.coroutines.EmptyCoroutineContext
import kotlin.coroutines.startCoroutine
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Runnable
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.joinAll
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject

class MobileControllerTest {
    private val profile = HostProfile("host-1", "Host", listOf("127.0.0.1:49152"), "device-1")
    private val thread = snapshot("thread-1")

    @Test
    fun connect_loads_lists_and_returns_to_task_list_when_selection_was_persisted() {
        val gateway = FakeHostGateway()
        val controller = controller(gateway, selectedThreadId = "thread-1")

        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }

        assertEquals(1, gateway.projectListCalls)
        assertEquals(listOf(""), gateway.listCwds)
        assertEquals(emptyList(), gateway.readIds)
        assertIs<LoadPhase.Ready>(controller.state.selectedView.threadList)
        assertNull(controller.state.selectedView.selectedThreadId)
        assertIs<LoadPhase.Idle>(controller.state.selectedView.threadDetail)
    }

    @Test
    fun connect_rediscovers_and_persists_the_current_host_endpoint_before_dialing() {
        val currentAddress = "127.0.0.1:51833"
        val gateway = FakeHostGateway().apply {
            discoveryResult = GatewayResult.Success(listOf(currentAddress))
        }
        val controller = controller(gateway)

        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }

        assertEquals(listOf(currentAddress), gateway.connectedProfiles.single().addresses)
        assertEquals(listOf(currentAddress), controller.state.selectedProfile?.addresses)
    }

    @Test
    fun connect_uses_saved_addresses_when_rediscovery_is_unavailable() {
        val gateway = FakeHostGateway().apply {
            discoveryResult = GatewayResult.Failure("mDNS unavailable")
        }
        val controller = controller(gateway)

        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }

        assertEquals(listOf(profile), gateway.connectedProfiles)
        assertIs<ConnectionPhase.Connected>(controller.state.selectedView.connection)
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
                {"threadId":"thread-1","turn":{"id":"turn-1","status":"completed"}}
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
    fun notifications_from_another_client_update_the_open_thread_immediately() {
        val initial = thread.copy(turns = emptyList())
        val gateway = FakeHostGateway().apply {
            readResult = GatewayResult.Success(ThreadReadResult(initial, emptyList()))
        }
        val controller = controller(gateway, selectedThreadId = "thread-1", cachedThread = initial)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        runSuspend { controller.readThread(profile, "thread-1") }

        gateway.emit(notification("turn/started", """
            {"threadId":"thread-1","turn":{"id":"turn-external","status":"inProgress"}}
        """))
        gateway.emit(notification("item/started", """
            {"threadId":"thread-1","turnId":"turn-external",
             "item":{"id":"user-external","type":"userMessage","content":[{"type":"text","text":"from another phone"}]}}
        """))
        gateway.emit(notification("item/started", """
            {"threadId":"thread-1","turnId":"turn-external",
             "item":{"id":"agent-external","type":"agentMessage","text":""}}
        """))
        gateway.emit(notification("item/agentMessage/delta", """
            {"threadId":"thread-1","turnId":"turn-external",
             "itemId":"agent-external","delta":"live reply"}
        """))
        gateway.emit(notification("turn/completed", """
            {"threadId":"thread-1","turn":{"id":"turn-external","status":"completed"}}
        """))

        val externalTurn = controller.state.cache.snapshot(profile.hostIdentity, "thread-1")!!.turns.single()
        assertEquals(TurnStatus.Completed, externalTurn.status)
        assertEquals(
            listOf(
                CodexItem.UserMessage("user-external", "from another phone"),
                CodexItem.AgentMessage("agent-external", "live reply"),
            ),
            externalTurn.items,
        )
        assertEquals(listOf("thread-1"), gateway.readIds)
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
    fun starting_a_task_forwards_only_the_working_directory_and_first_prompt() {
        val project = CodexProject(
            id = "project-1",
            name = "remote-agent",
            roots = listOf(WorkingDirectory("/workspace/remote-agent")),
            position = 0,
            createdAtMs = 1,
            updatedAtMs = 1,
        )
        val started = snapshot("thread-new").let { snapshot ->
            snapshot.copy(summary = snapshot.summary.copy(
                workingDirectory = WorkingDirectory("/workspace/remote-agent"),
                projectId = project.id,
            ))
        }
        val gateway = FakeHostGateway().apply {
            projectResult = GatewayResult.Success(listOf(project))
            projectStartResult = GatewayResult.Success(ThreadStartResult(started, "turn-new"))
        }
        val controller = controller(gateway)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }

        runSuspend {
            controller.startThread(profile, "/workspace/remote-agent", "  Build it  ")
        }

        assertEquals(StartArguments("/workspace/remote-agent", "Build it"), gateway.startArguments)
        assertEquals("thread-new", controller.state.selectedView.selectedThreadId)
    }

    @Test
    fun transport_closure_retires_the_connected_host() {
        val gateway = FakeHostGateway()
        val controller = controller(gateway)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }

        gateway.closeStream("channel closed")

        assertIs<ConnectionPhase.Disconnected>(controller.state.selectedView.connection)
        assertEquals(1, gateway.disconnectCalls)
    }

    @Test
    fun starting_turn_passes_the_cached_snapshot_working_directory() {
        val gateway = FakeHostGateway().apply {
            listResult = GatewayResult.Success(listOf(summary("thread-1", "/cached/workspace")))
        }
        val controller = controller(gateway, selectedThreadId = null, cachedThread = thread.copy(turns = emptyList()))
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }

        runSuspend { controller.startTurn(profile, "thread-1", "hello") }

        assertEquals(listOf("/workspace"), gateway.turnCwds)
        assertEquals(listOf("hello"), gateway.turnTexts)
    }

    @Test
    fun successful_turn_start_refreshes_the_detail_so_the_sent_message_is_visible() {
        val idleThread = thread.copy(turns = emptyList())
        val refreshed = idleThread.copy(
            turns = listOf(CodexTurn(
                id = "turn-2",
                status = TurnStatus.InProgress,
                items = listOf(CodexItem.UserMessage("user-2", "hello from task detail")),
            )),
        )
        val gateway = FakeHostGateway().apply {
            turnResult = GatewayResult.Success("turn-2")
            readResult = GatewayResult.Success(ThreadReadResult(idleThread, emptyList()))
        }
        val controller = controller(gateway, selectedThreadId = "thread-1", cachedThread = idleThread)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        runSuspend { controller.readThread(profile, "thread-1") }
        gateway.readResult = GatewayResult.Success(ThreadReadResult(refreshed, emptyList()))
        gateway.readIds.clear()

        runSuspend { controller.startTurn(profile, "thread-1", "hello from task detail") }

        assertEquals(listOf("thread-1"), gateway.readIds)
        assertEquals(
            CodexItem.UserMessage("user-2", "hello from task detail"),
            controller.state.cache.snapshot(profile.hostIdentity, "thread-1")
                ?.turns
                ?.last()
                ?.items
                ?.single(),
        )
    }

    @Test
    fun successful_turn_start_does_not_reopen_detail_after_the_user_returns_to_the_list() {
        val idleThread = thread.copy(turns = emptyList())
        val gateway = FakeHostGateway().apply {
            turnResult = GatewayResult.Success("turn-2")
            readResult = GatewayResult.Success(ThreadReadResult(idleThread, emptyList()))
        }
        val controller = controller(gateway, selectedThreadId = "thread-1", cachedThread = idleThread)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        runSuspend { controller.readThread(profile, "thread-1") }
        gateway.readIds.clear()
        gateway.turnHook = {
            controller.dispatch(AppAction.ThreadListOpened(profile.hostIdentity))
        }

        runSuspend { controller.startTurn(profile, "thread-1", "hello") }

        assertNull(controller.state.selectedView.selectedThreadId)
        assertTrue(gateway.readIds.isEmpty())
    }

    @Test
    fun active_turn_steers_existing_turn_and_refreshes_selected_detail() {
        val gateway = FakeHostGateway().apply {
            readResult = GatewayResult.Success(ThreadReadResult(thread, emptyList()))
            turnResult = GatewayResult.Failure("active turns must use steer")
            steerResult = GatewayResult.Success(Unit)
        }
        val controller = controller(gateway, selectedThreadId = "thread-1")
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        runSuspend { controller.readThread(profile, "thread-1") }
        gateway.readIds.clear()

        runSuspend { controller.startTurn(profile, "thread-1", "keep going") }

        assertEquals(listOf("turn-1"), gateway.steerTurnIds)
        assertEquals(listOf("keep going"), gateway.steerTexts)
        assertTrue(gateway.turnCwds.isEmpty())
        assertEquals(listOf("thread-1"), gateway.readIds)
    }

    @Test
    fun active_turn_steer_failure_surfaces_notice_without_falling_back_to_start() {
        val gateway = FakeHostGateway().apply {
            readResult = GatewayResult.Success(ThreadReadResult(thread, emptyList()))
            turnResult = GatewayResult.Failure("active turns must use steer")
            steerResult = GatewayResult.Failure("steer failed")
        }
        val controller = controller(gateway, selectedThreadId = "thread-1")
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        runSuspend { controller.readThread(profile, "thread-1") }
        gateway.readIds.clear()

        runSuspend { controller.startTurn(profile, "thread-1", "keep going") }

        assertEquals("steer failed", controller.state.selectedView.notice)
        assertEquals(listOf("turn-1"), gateway.steerTurnIds)
        assertEquals(listOf("keep going"), gateway.steerTexts)
        assertTrue(gateway.turnCwds.isEmpty())
        assertTrue(gateway.turnTexts.isEmpty())
        assertTrue(gateway.readIds.isEmpty())
    }

    @Test
    fun active_thread_without_a_visible_turn_queues_message_and_shows_delivery_notice() {
        val activeThread = thread.copy(
            summary = thread.summary.copy(status = ThreadStatus.Active(emptyList())),
            turns = emptyList(),
        )
        val gateway = FakeHostGateway().apply {
            listResult = GatewayResult.Success(listOf(activeThread.summary))
            queueResult = GatewayResult.Success("queued-1")
        }
        val controller = controller(
            gateway,
            cachedThread = activeThread,
        )
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }

        runSuspend { controller.startTurn(profile, "thread-1", "queued hello") }

        assertEquals(listOf("thread-1"), gateway.queueThreadIds)
        assertEquals(listOf("queued hello"), gateway.queueTexts)
        assertTrue(gateway.steerTurnIds.isEmpty())
        assertTrue(gateway.turnTexts.isEmpty())
        assertEquals("現在の処理が完了した後にメッセージを送信します。", controller.state.selectedView.notice)
        assertTrue(controller.state.cache.snapshot(profile.hostIdentity, "thread-1")?.turns.orEmpty().isEmpty())
    }

    @Test
    fun active_thread_list_status_queues_even_when_cached_detail_status_is_stale() {
        val idleThread = thread.copy(
            summary = thread.summary.copy(status = ThreadStatus.Idle),
            turns = emptyList(),
        )
        val activeSummary = idleThread.summary.copy(status = ThreadStatus.Active(emptyList()))
        val gateway = FakeHostGateway().apply {
            listResult = GatewayResult.Success(listOf(activeSummary))
            queueResult = GatewayResult.Success("queued-1")
        }
        val controller = controller(gateway, cachedThread = idleThread)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }

        runSuspend { controller.startTurn(profile, "thread-1", "queued hello") }

        assertEquals(listOf("thread-1"), gateway.queueThreadIds)
        assertTrue(gateway.turnTexts.isEmpty())
    }

    @Test
    fun active_thread_queue_failure_surfaces_notice_without_falling_back_to_start() {
        val activeThread = thread.copy(
            summary = thread.summary.copy(status = ThreadStatus.Active(emptyList())),
            turns = emptyList(),
        )
        val gateway = FakeHostGateway().apply {
            listResult = GatewayResult.Success(listOf(activeThread.summary))
            queueResult = GatewayResult.Failure("queue failed")
            turnResult = GatewayResult.Success("must not start")
        }
        val controller = controller(gateway, cachedThread = activeThread)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }

        runSuspend { controller.startTurn(profile, "thread-1", "queued hello") }

        assertEquals("queue failed", controller.state.selectedView.notice)
        assertEquals(listOf("thread-1"), gateway.queueThreadIds)
        assertTrue(gateway.queueTexts.isNotEmpty())
        assertTrue(gateway.steerTurnIds.isEmpty())
        assertTrue(gateway.turnTexts.isEmpty())
    }

    @Test
    fun turn_start_acknowledgement_upserts_an_in_progress_placeholder_when_started_notification_is_missing() {
        val idleThread = thread.copy(turns = emptyList())
        val gateway = FakeHostGateway().apply {
            listResult = GatewayResult.Success(listOf(idleThread.summary))
        }
        val controller = controller(
            gateway,
            selectedThreadId = "thread-1",
            cachedThread = idleThread,
        )
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }

        controller.dispatch(
            AppAction.TurnStartAcknowledged(profile.hostIdentity, "thread-1", "turn-ack"),
        )

        assertEquals(
            listOf(CodexTurn("turn-ack", TurnStatus.InProgress)),
            controller.state.cache.snapshot(profile.hostIdentity, "thread-1")?.turns,
        )
    }

    @Test
    fun late_turn_start_acknowledgement_preserves_a_terminal_status() {
        val completedTurn = CodexTurn("turn-ack", TurnStatus.Completed)
        val completedThread = thread.copy(turns = listOf(completedTurn))
        val gateway = FakeHostGateway().apply {
            listResult = GatewayResult.Success(listOf(completedThread.summary))
        }
        val controller = controller(
            gateway,
            selectedThreadId = "thread-1",
            cachedThread = completedThread,
        )
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }

        controller.dispatch(
            AppAction.TurnStartAcknowledged(profile.hostIdentity, "thread-1", "turn-ack"),
        )

        assertEquals(
            listOf(completedTurn),
            controller.state.cache.snapshot(profile.hostIdentity, "thread-1")?.turns,
        )
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

    @Test
    fun cancelled_connect_retires_generation_closes_transport_and_shows_no_failure() = runBlocking {
        val started = CompletableDeferred<Unit>()
        val gateway = FakeHostGateway().apply {
            connectBlock = {
                started.complete(Unit)
                awaitCancellation()
            }
        }
        val controller = controller(gateway)
        val connection = launch {
            controller.connect(profile, CoroutineScope(Dispatchers.Unconfined))
        }
        started.await()

        connection.cancelAndJoin()

        assertEquals(1, gateway.disconnectCalls)
        assertIs<ConnectionPhase.Disconnected>(controller.state.selectedView.connection)
        assertNull(controller.state.selectedView.notice)
    }

    @Test
    fun disconnect_invalidates_inflight_connect_then_closes_only_after_connect_returns() = runBlocking {
        val connectStarted = CompletableDeferred<Unit>()
        val releaseConnect = CompletableDeferred<Unit>()
        val gateway = FakeHostGateway().apply {
            connectBlock = {
                connectInFlight = true
                connectStarted.complete(Unit)
                releaseConnect.await()
                connectInFlight = false
                GatewayResult.Success(Unit)
            }
        }
        val controller = controller(gateway)
        val connecting = launch(start = CoroutineStart.UNDISPATCHED) {
            controller.connect(profile, CoroutineScope(Dispatchers.Unconfined))
        }
        connectStarted.await()
        val disconnecting = launch(start = CoroutineStart.UNDISPATCHED) {
            controller.disconnect(profile)
        }

        releaseConnect.complete(Unit)
        joinAll(connecting, disconnecting)

        assertTrue(gateway.disconnectCalls >= 1)
        assertFalse(gateway.disconnectObservedConnectInFlight)
        assertTrue(gateway.listCwds.isEmpty())
        assertNull(gateway.callback)
        assertIs<ConnectionPhase.Disconnected>(controller.state.selectedView.connection)
        Unit
    }

    @Test
    fun cancelled_explicit_disconnect_still_closes_transport_and_publishes_disconnected() = runBlocking {
        val disconnectStarted = CompletableDeferred<Unit>()
        val releaseDisconnect = CompletableDeferred<Unit>()
        val gateway = FakeHostGateway().apply {
            disconnectBlock = {
                disconnectStarted.complete(Unit)
                releaseDisconnect.await()
                GatewayResult.Success(Unit)
            }
        }
        val controller = controller(gateway)
        controller.connect(profile, CoroutineScope(Dispatchers.Unconfined))
        val disconnecting = launch { controller.disconnect(profile) }
        disconnectStarted.await()

        disconnecting.cancel()
        assertFalse(disconnecting.isCompleted)
        releaseDisconnect.complete(Unit)
        disconnecting.join()

        assertEquals(1, gateway.disconnectCalls)
        assertIs<ConnectionPhase.Disconnected>(controller.state.selectedView.connection)
        Unit
    }

    @Test
    fun native_callback_is_dispatched_to_application_scope_and_dropped_after_disconnect() = runBlocking {
        val dispatcher = QueuedDispatcher()
        val gateway = FakeHostGateway()
        val controller = controller(gateway)
        controller.connect(profile, CoroutineScope(dispatcher))

        gateway.emit(notification("future/notification", "{\"value\":true}"))
        assertEquals(emptyList(), controller.state.cache.profile(profile.hostIdentity).rawMessages)

        controller.disconnect(profile)
        dispatcher.runAll()

        assertEquals(emptyList(), controller.state.cache.profile(profile.hostIdentity).rawMessages)
        assertIs<ConnectionPhase.Disconnected>(controller.state.selectedView.connection)
        Unit
    }

    @Test
    fun persistence_failure_keeps_memory_and_observers_current_then_retries_on_next_transition() {
        val repository = FailingOnceMobileRepository(AppState(profiles = listOf(profile)))
        val controller = MobileController(FakeHostGateway(), repository)
        var observed: AppState? = null
        controller.observe { observed = it }

        controller.dispatch(AppAction.PairingOpened)

        assertTrue(controller.state.showingPairing)
        assertTrue(requireNotNull(observed).showingPairing)
        assertEquals("IllegalStateException", controller.lastPersistenceFailureType)

        controller.dispatch(AppAction.PairingDismissed)

        assertEquals(2, repository.saveCalls)
        assertNull(controller.lastPersistenceFailureType)
        assertEquals(controller.state, repository.savedState)
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
        var discoveryResult: GatewayResult<List<String>> = GatewayResult.Success(emptyList())
        var connectResult: GatewayResult<Unit> = GatewayResult.Success(Unit)
        var connectBlock: (suspend () -> GatewayResult<Unit>)? = null
        var listResult: GatewayResult<List<ThreadSummary>> = GatewayResult.Success(emptyList())
        var projectResult: GatewayResult<List<CodexProject>> = GatewayResult.Success(emptyList())
        var readResult: GatewayResult<ThreadReadResult> = GatewayResult.Failure("not configured")
        var startResult: GatewayResult<ThreadSnapshot> = GatewayResult.Failure("not configured")
        var projectStartResult: GatewayResult<ThreadStartResult> = GatewayResult.Failure("not configured")
        var turnResult: GatewayResult<String> = GatewayResult.Success("turn-1")
        var steerResult: GatewayResult<Unit> = GatewayResult.Success(Unit)
        var queueResult: GatewayResult<String> = GatewayResult.Success("queue-1")
        var interruptResult: GatewayResult<Unit> = GatewayResult.Success(Unit)
        var disconnectResult: GatewayResult<Unit> = GatewayResult.Success(Unit)
        var disconnectBlock: (suspend () -> GatewayResult<Unit>)? = null
        var readHook: (() -> Unit)? = null
        var callback: ((RawCodexMessage) -> Unit)? = null
        var closeCallback: ((String) -> Unit)? = null
        var listCwds = mutableListOf<String>()
        var readIds = mutableListOf<String>()
        var startCalls = 0
        var projectListCalls = 0
        var startArguments: StartArguments? = null
        var turnCwds = mutableListOf<String>()
        var turnTexts = mutableListOf<String>()
        var steerTurnIds = mutableListOf<String>()
        var steerTexts = mutableListOf<String>()
        var queueThreadIds = mutableListOf<String>()
        var queueTexts = mutableListOf<String>()
        var connectHook: (() -> Unit)? = null
        var listHook: (() -> Unit)? = null
        var turnHook: (() -> Unit)? = null
        var interruptHook: (() -> Unit)? = null
        var subscriptionCancelCount = 0
        var disconnectCalls = 0
        var subscriptionWasRetiredAtDisconnect = false
        var connectInFlight = false
        var disconnectObservedConnectInFlight = false
        val connectedProfiles = mutableListOf<HostProfile>()

        override suspend fun pair(payload: PairingQrPayload): GatewayResult<HostProfile> = GatewayResult.Failure("unused")
        override suspend fun discover(profile: HostProfile): GatewayResult<List<String>> = when (val result = discoveryResult) {
            is GatewayResult.Success -> GatewayResult.Success(result.value.ifEmpty { profile.addresses })
            is GatewayResult.Failure -> result
        }
        override suspend fun connect(profile: HostProfile): GatewayResult<Unit> {
            connectedProfiles += profile
            connectHook?.invoke()
            return connectBlock?.invoke() ?: connectResult
        }
        override suspend fun disconnect(profile: HostProfile): GatewayResult<Unit> {
            disconnectCalls += 1
            disconnectObservedConnectInFlight = disconnectObservedConnectInFlight || connectInFlight
            subscriptionWasRetiredAtDisconnect = callback == null
            return disconnectBlock?.invoke() ?: disconnectResult
        }
        override suspend fun listProjects(profile: HostProfile): GatewayResult<List<CodexProject>> {
            projectListCalls += 1
            return projectResult
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
        override suspend fun startThread(
            profile: HostProfile,
            cwd: String,
            firstPrompt: String,
        ): GatewayResult<ThreadStartResult> {
            startArguments = StartArguments(cwd, firstPrompt)
            return projectStartResult
        }
        override suspend fun startTurn(profile: HostProfile, threadId: String, cwd: String, text: String): GatewayResult<String> {
            turnCwds += cwd
            turnTexts += text
            turnHook?.invoke()
            return turnResult
        }
        override suspend fun steerTurn(profile: HostProfile, threadId: String, turnId: String, text: String): GatewayResult<Unit> {
            steerTurnIds += turnId
            steerTexts += text
            return steerResult
        }
        override suspend fun queueTurn(profile: HostProfile, threadId: String, text: String): GatewayResult<String> {
            queueThreadIds += threadId
            queueTexts += text
            return queueResult
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
        override fun subscribeRaw(
            profile: HostProfile,
            onMessage: (RawCodexMessage) -> Unit,
            onClosed: (String) -> Unit,
        ): HostEventSubscription {
            closeCallback = onClosed
            return subscribeRaw(profile, onMessage)
        }
        override suspend fun respondResult(profile: HostProfile, requestId: JsonElement, result: JsonElement): GatewayResult<Unit> =
            GatewayResult.Success(Unit)
        override suspend fun respondError(profile: HostProfile, requestId: JsonElement, error: JsonElement): GatewayResult<Unit> =
            GatewayResult.Success(Unit)

        fun emit(message: RawCodexMessage) {
            callback?.invoke(message)
        }

        fun closeStream(message: String) {
            closeCallback?.invoke(message)
        }
    }

    private data class StartArguments(val cwd: String, val firstPrompt: String)

    private class FailingOnceMobileRepository(
        private val initialState: AppState,
    ) : MobileRepository {
        var saveCalls = 0
        var savedState: AppState? = null

        override fun load(): AppState = initialState

        override fun save(state: AppState) {
            saveCalls += 1
            if (saveCalls == 1) error("sanitized test failure")
            savedState = state
        }
    }

    private class QueuedDispatcher : CoroutineDispatcher() {
        private val queued = ArrayDeque<Runnable>()

        override fun dispatch(context: CoroutineContext, block: Runnable) {
            queued.addLast(block)
        }

        fun runAll() {
            while (queued.isNotEmpty()) queued.removeFirst().run()
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

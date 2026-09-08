package dev.remoteagent.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.cancel
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.joinAll
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking

internal class MobileConnectionControllerTest : MobileControllerTestFixture() {
    @Test
    fun connect_completion_after_disconnect_is_ignored_and_reconnect_starts_a_new_generation() {
        val gateway = FakeHostGateway()
        val controller = controller(gateway, selectedThreadId = null)
        gateway.connectHook = { controller.disconnect(profile.id) }

        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }

        assertIs<ConnectionPhase.Disconnected>(controller.state.selectedView.connection)
        assertTrue(gateway.callback == null)
        assertTrue(gateway.listQueries.isEmpty())
        assertEquals(1, gateway.disconnectCalls)

        gateway.connectHook = null
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }

        assertIs<ConnectionPhase.Connected>(controller.state.selectedView.connection)
        assertEquals(1, gateway.listQueries.size)
    }

    @Test
    fun list_completion_after_disconnect_does_not_replace_the_newly_disconnected_view() {
        val gateway = FakeHostGateway()
        val controller = controller(gateway, selectedThreadId = null)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        gateway.listResult = GatewayResult.Success(listOf(summary("stale-thread", "/stale")))
        gateway.listHook = { controller.disconnect(profile.id) }

        runSuspend { controller.listThreads(profile) }

        assertIs<ConnectionPhase.Disconnected>(controller.state.selectedView.connection)
        assertIs<LoadPhase.Idle>(controller.state.selectedView.threadList)
        assertNull(controller.state.cache.profile(profile.id).threadList.firstOrNull())

        gateway.listHook = null
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        assertIs<ConnectionPhase.Connected>(controller.state.selectedView.connection)
    }

    @Test
    fun read_completion_after_disconnect_does_not_apply_snapshot_or_notice() {
        val gateway =
            FakeHostGateway().apply { readResult = GatewayResult.Success(ThreadReadResult(thread, emptyList())) }
        val controller = controller(gateway, selectedThreadId = null, cachedThread = null)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        gateway.readHook = { controller.disconnect(profile.id) }

        runSuspend { controller.readThread(profile, "thread-1") }

        assertIs<ConnectionPhase.Disconnected>(controller.state.selectedView.connection)
        assertIs<LoadPhase.Idle>(controller.state.selectedView.threadDetail)
        assertNull(controller.state.cache.snapshot(profile.id, "thread-1"))
        assertNull(controller.state.selectedView.notice)
    }

    @Test
    fun turn_completion_after_disconnect_does_not_publish_a_stale_failure() {
        val gateway =
            FakeHostGateway().apply {
                listResult = GatewayResult.Success(listOf(summary("thread-1", "/workspace")))
                turnResult = GatewayResult.Failure("stale turn failure")
            }
        val controller = controller(gateway, selectedThreadId = null, cachedThread = null)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        gateway.turnHook = { controller.disconnect(profile.id) }

        runSuspend { controller.startTurn(profile, "thread-1", "hello") }

        assertIs<ConnectionPhase.Disconnected>(controller.state.selectedView.connection)
        assertNull(controller.state.selectedView.notice)
    }

    @Test
    fun interrupt_completion_after_disconnect_does_not_clear_or_publish_stale_state() {
        val gateway = FakeHostGateway().apply { interruptResult = GatewayResult.Failure("stale interrupt failure") }
        val controller = controller(gateway, selectedThreadId = null)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        gateway.interruptHook = { controller.disconnect(profile.id) }

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
            if (it.profileViews[profile.id]?.connection == ConnectionPhase.Disconnected) {
                cancelledBeforeObserver = gateway.subscriptionCancelCount > 0
            }
        }

        controller.disconnect(profile.id)

        assertTrue(cancelledBeforeObserver)
        assertEquals(1, gateway.subscriptionCancelCount)
        assertNull(gateway.callback)
    }

    @Test
    fun explicit_disconnect_retires_subscription_closes_gateway_and_ignores_close_failure() {
        val gateway = FakeHostGateway().apply { disconnectResult = GatewayResult.Failure("close failed") }
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
        val gateway =
            FakeHostGateway().apply {
                connectBlock = {
                    started.complete(Unit)
                    awaitCancellation()
                }
            }
        val controller = controller(gateway)
        val connection = launch { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
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
        val gateway =
            FakeHostGateway().apply {
                connectBlock = {
                    connectInFlight = true
                    connectStarted.complete(Unit)
                    releaseConnect.await()
                    connectInFlight = false
                    GatewayResult.Success(Unit)
                }
            }
        val controller = controller(gateway)
        val connecting =
            launch(start = CoroutineStart.UNDISPATCHED) {
                controller.connect(profile, CoroutineScope(Dispatchers.Unconfined))
            }
        connectStarted.await()
        val disconnecting = launch(start = CoroutineStart.UNDISPATCHED) { controller.disconnect(profile) }

        releaseConnect.complete(Unit)
        joinAll(connecting, disconnecting)

        assertTrue(gateway.disconnectCalls >= 1)
        assertFalse(gateway.disconnectObservedConnectInFlight)
        assertTrue(gateway.listQueries.isEmpty())
        assertNull(gateway.callback)
        assertIs<ConnectionPhase.Disconnected>(controller.state.selectedView.connection)
        Unit
    }

    @Test
    fun cancelled_explicit_disconnect_still_closes_transport_and_publishes_disconnected() = runBlocking {
        val disconnectStarted = CompletableDeferred<Unit>()
        val releaseDisconnect = CompletableDeferred<Unit>()
        val gateway =
            FakeHostGateway().apply {
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

        val before = controller.state.cache.snapshot(profile.id, "thread-1")!!.turns.single().items.single()
        gateway.emit(
            notification(
                "item/agentMessage/delta",
                """{"threadId":"thread-1","turnId":"turn-1","itemId":"item-1","delta":" stale"}""",
            )
        )
        assertEquals(before, controller.state.cache.snapshot(profile.id, "thread-1")!!.turns.single().items.single())

        controller.disconnect(profile)
        dispatcher.runAll()

        assertEquals(before, controller.state.cache.snapshot(profile.id, "thread-1")!!.turns.single().items.single())
        assertIs<ConnectionPhase.Disconnected>(controller.state.selectedView.connection)
        Unit
    }

    @Test
    fun one_native_message_publishes_typed_state_once() {
        val gateway = FakeHostGateway()
        val controller = controller(gateway)
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined)
        try {
            runSuspend { controller.connect(profile, scope) }
            val observed = mutableListOf<AppState>()
            controller.observe { observed += it }
            gateway.emit(
                notification(
                    "item/agentMessage/delta",
                    """{"threadId":"thread-1","turnId":"turn-1","itemId":"item-1","delta":" new"}""",
                )
            )
            assertEquals(2, observed.size)
            val cache = observed.last().cache.profile(profile.id)
            val reply = cache.snapshots["thread-1"]!!.turns.single().items.single() as CodexItem.AgentMessage
            assertEquals("old new", reply.text)
        } finally {
            scope.cancel()
        }
    }
}

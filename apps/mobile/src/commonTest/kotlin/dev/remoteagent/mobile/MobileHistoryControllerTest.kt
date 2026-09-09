package dev.remoteagent.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject

internal class MobileHistoryControllerTest : MobileControllerTestFixture() {
    @Test
    fun older_history_coalesces_loads_and_discards_a_page_after_navigation() = runBlocking {
        val gateway = FakeHostGateway()
        val current = thread.copy(raw = Json.parseToJsonElement("""{"historyCursor":"opaque"}""") as JsonObject)
        gateway.readResult = GatewayResult.Success(ThreadReadResult(current, emptyList()))
        val controller = controller(gateway)
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined)
        try {
            controller.openApp(scope)
            controller.readThread(profile, "thread-1")
            val blocked = CompletableDeferred<GatewayResult<String>>()
            var calls = 0
            gateway.agentBlock = { command ->
                assertEquals(AgentCommand.ReadOlder("thread-1", "opaque", null, false), command)
                calls++
                blocked.await()
            }
            val load = scope.launch { controller.loadOlderHistory(profile) }
            controller.loadOlderHistory(profile)
            assertEquals(1, calls)
            controller.dispatch(AppAction.ThreadSelected(profile.id, "thread-2"))
            blocked.complete(
                GatewayResult.Success(
                    """{"thread":{"id":"thread-1","historyCursor":null,"turns":[{"id":"older","items":[]}]}}"""
                )
            )
            load.join()
            assertEquals("thread-2", controller.state.selectedView.selectedThreadId)
            assertEquals(
                listOf("turn-1"),
                controller.state.cache.snapshot(profile.id, "thread-1")!!.turns.map { it.id },
            )
            assertFalse(controller.state.selectedView.loadingHistory)
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun events_during_read_are_applied_after_snapshot_and_other_threads_do_not_mix() {
        val gateway =
            FakeHostGateway().apply {
                readResult = GatewayResult.Success(ThreadReadResult(thread, emptyList()))
                listResult = GatewayResult.Success(listOf(thread.summary))
            }
        val controller = controller(gateway)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        val transitions = mutableListOf<AppState>()
        controller.observe { transitions += it }
        gateway.readHook = {
            gateway.emit(
                notification(
                    "item/agentMessage/delta",
                    """
                {"threadId":"thread-1","turnId":"turn-1","itemId":"item-1","delta":" new"}
            """,
                )
            )
            gateway.emit(
                notification(
                    "turn/completed",
                    """
                {"threadId":"thread-1","turn":{"id":"turn-1","status":"completed"}}
            """,
                )
            )
            gateway.emit(
                notification(
                    "item/agentMessage/delta",
                    """
                {"threadId":"thread-2","turnId":"turn-2","itemId":"item-2","delta":"wrong"}
            """,
                )
            )
        }

        runSuspend { controller.readThread(profile, "thread-1") }

        val result = requireNotNull(controller.state.cache.snapshot(profile.id, "thread-1"))
        val resultTurn = result.turns.single()
        assertEquals(TurnStatus.Completed, resultTurn.status)
        assertEquals("old new", (resultTurn.items.single() as CodexItem.AgentMessage).text)
        val loaded =
            transitions
                .mapNotNull { it.cache.snapshot(profile.id, "thread-1")?.turns?.single() }
                .filter { (it.items.single() as CodexItem.AgentMessage).text == "old new" }
        assertTrue(loaded.isNotEmpty())
        assertTrue(loaded.all { it.status == TurnStatus.Completed })
    }

    @Test
    fun external_history_changes_refresh_the_open_body_without_loading_or_sending() = runBlocking {
        val initial =
            thread.copy(
                summary = thread.summary.copy(status = ThreadStatus.NotLoaded),
                turns = thread.turns.map { it.copy(status = TurnStatus.Completed) },
                raw = Json.parseToJsonElement("""{"path":"/fixture/rollout.jsonl"}""") as JsonObject,
            )
        val gateway =
            FakeHostGateway().apply { readResult = GatewayResult.Success(ThreadReadResult(initial, emptyList())) }
        val controller = controller(gateway, selectedThreadId = "thread-1", cachedThread = initial)
        val scope = CoroutineScope(coroutineContext + SupervisorJob())
        val registered = CompletableDeferred<Long>()
        val unregistered = CompletableDeferred<Unit>()
        gateway.agentBlock = { command ->
            when (command) {
                is AgentCommand.WatchThread -> registered.complete(command.watchId)
                is AgentCommand.UnwatchThread -> unregistered.complete(Unit)
                else -> error("Unexpected intent: $command")
            }
            GatewayResult.Success("null")
        }
        var enteredLoading = false
        val initialRefresh = CompletableDeferred<Unit>()
        val updated = CompletableDeferred<Unit>()
        var maintenance: HostEventSubscription? = null
        var observation: HostEventSubscription? = null
        try {
            controller.connect(profile, scope)
            observation = controller.observe { state ->
                enteredLoading = enteredLoading || state.selectedView.threadDetail is LoadPhase.Loading
                if (gateway.readIds.size >= 2) initialRefresh.complete(Unit)
                val answer = state.cache.snapshot(profile.id, "thread-1")?.turns?.last()?.items?.last()
                if (answer == CodexItem.AgentMessage("item-1", "Persisted external answer")) updated.complete(Unit)
            }
            maintenance = controller.maintainConnection(scope)
            val revision = withTimeout(5_000) { registered.await() }
            withTimeout(5_000) { initialRefresh.await() }
            replaceWatchedAnswer(gateway, initial, revision)
            withTimeout(5_000) { updated.await() }
            assertEquals(3, gateway.readIds.size, "a burst must coalesce into one quiet history refresh")
            assertFalse(enteredLoading)
            assertEquals("thread-1", controller.state.selectedView.selectedThreadId)
            assertNull(controller.state.selectedView.notice)
            assertTrue(gateway.turnTexts.isEmpty())
            controller.showThreadList(profile)
            withTimeout(5_000) { unregistered.await() }
            val staleRead = CompletableDeferred<Unit>()
            gateway.readHook = { staleRead.complete(Unit) }
            gateway.emit(
                notification("host/thread/changed", """{"watchKey":1,"watchId":$revision,"threadId":"thread-1"}""")
            )
            assertNull(withTimeoutOrNull(250) { staleRead.await() })
            assertNull(controller.state.selectedView.selectedThreadId)
        } finally {
            observation?.cancel()
            maintenance?.cancel()
            scope.cancel()
        }
    }

    @Test
    fun notifications_from_another_client_update_the_open_thread_immediately() {
        val initial = thread.copy(turns = emptyList())
        val gateway =
            FakeHostGateway().apply { readResult = GatewayResult.Success(ThreadReadResult(initial, emptyList())) }
        val controller = controller(gateway, selectedThreadId = "thread-1", cachedThread = initial)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }

        emitExternalConversation(gateway)

        val externalTurn = controller.state.cache.snapshot(profile.id, "thread-1")!!.turns.single()
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
        val gateway =
            FakeHostGateway().apply { readResult = GatewayResult.Success(ThreadReadResult(thread, emptyList())) }
        val controller = controller(gateway, cacheLimits = MobileCacheLimits(maxApproximateBytes = 64))
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        gateway.readHook = {
            gateway.emit(
                notification(
                    "item/agentMessage/delta",
                    """
                {"threadId":"thread-1","turnId":"turn-1","itemId":"item-1","delta":"${"x".repeat(100)}"}
            """,
                )
            )
        }

        runSuspend { controller.readThread(profile, "thread-1") }

        val phase = controller.state.selectedView.threadDetail
        assertIs<LoadPhase.Failed>(phase)
        assertTrue(phase.message.contains("同期"))
    }
}

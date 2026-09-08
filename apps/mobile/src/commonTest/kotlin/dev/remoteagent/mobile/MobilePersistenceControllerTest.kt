package dev.remoteagent.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.cancel
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.withTimeoutOrNull

internal class MobilePersistenceControllerTest : MobileControllerTestFixture() {
    @Test
    fun streaming_is_persisted_at_completion_or_explicit_flush_without_losing_deltas() = runBlocking {
        val writes = Channel<AppState>(Channel.UNLIMITED)
        val repository =
            object : MobileRepository {
                override fun load() =
                    AppState(
                        profiles = listOf(profile),
                        selectedProfileId = profile.id,
                        profileViews = mapOf(profile.id to ProfileViewState(selectedThreadId = thread.summary.id)),
                        cache =
                            MobileCache(
                                mapOf(profile.id to ProfileMobileCache(snapshots = mapOf(thread.summary.id to thread)))
                            ),
                    )

                override fun save(state: AppState) {
                    writes.trySend(state)
                }
            }
        val gateway = FakeHostGateway()
        val controller = MobileController(gateway, repository, persistenceScope())
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined)
        try {
            controller.connect(profile, scope)
            controller.flushPersistence()
            var pending: Boolean
            do {
                pending = writes.tryReceive().isSuccess
            } while (pending)
            fun emit() =
                gateway.emit(
                    notification(
                        "item/agentMessage/delta",
                        """{"threadId":"thread-1","turnId":"turn-1","itemId":"item-1","delta":"x"}""",
                    )
                )
            repeat(100) { emit() }
            assertNull(withTimeoutOrNull(300) { writes.receive() })
            controller.flushPersistence()
            val flushed = withTimeout(5_000) { writes.receive() }
            assertEquals(
                "old" + "x".repeat(100),
                (flushed.cache.snapshot(profile.id, "thread-1")!!.turns.single().items.single()
                        as CodexItem.AgentMessage)
                    .text,
            )
            emit()
            gateway.emit(
                notification(
                    "turn/completed",
                    """{"threadId":"thread-1","turn":{"id":"turn-1","status":"completed"}}""",
                )
            )
            val completed =
                withTimeout(5_000) { writes.receive() }.cache.snapshot(profile.id, "thread-1")!!.turns.single()
            assertEquals(TurnStatus.Completed, completed.status)
            assertEquals("old" + "x".repeat(101), (completed.items.single() as CodexItem.AgentMessage).text)
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun slow_storage_coalesces_updates_without_blocking_state_or_reordering_writes() = runBlocking {
        val started = CompletableDeferred<Unit>()
        val release = CompletableDeferred<Unit>()
        val saved = mutableListOf<AppState>()
        val repository =
            object : MobileRepository {
                override fun load() = AppState(profiles = listOf(profile))

                override fun save(state: AppState) {
                    if (saved.isEmpty()) {
                        started.complete(Unit)
                        runBlocking { withTimeout(5_000) { release.await() } }
                    }
                    saved += state
                }
            }
        val controller = MobileController(FakeHostGateway(), repository, persistenceScope())
        try {
            controller.dispatch(AppAction.PairingOpened)
            withTimeout(5_000) { started.await() }
            repeat(100) { controller.dispatch(AppAction.PairingOpened) }
            controller.dispatch(AppAction.PairingDismissed)
            assertFalse(controller.state.showingPairing)
            val flushed = async(start = CoroutineStart.UNDISPATCHED) { controller.flushPersistence() }
            assertFalse(flushed.isCompleted)
            release.complete(Unit)
            withTimeout(5_000) { flushed.await() }
            assertEquals(listOf(true, false), saved.map { it.showingPairing })
            assertEquals(controller.state, saved.last())
        } finally {
            release.complete(Unit)
        }
    }

    @Test
    fun persistence_failure_keeps_memory_and_observers_current_then_retries_on_next_transition() = runBlocking {
        val repository = FailingOnceMobileRepository(AppState(profiles = listOf(profile)))
        val controller = MobileController(FakeHostGateway(), repository, persistenceScope())
        var observed: AppState? = null
        controller.observe { observed = it }

        controller.dispatch(AppAction.PairingOpened)

        assertTrue(controller.state.showingPairing)
        assertTrue(requireNotNull(observed).showingPairing)
        controller.flushPersistence()
        assertEquals("IllegalStateException", controller.lastPersistenceFailureType)

        controller.dispatch(AppAction.PairingDismissed)

        controller.flushPersistence()
        assertEquals(2, repository.saveCalls)
        assertNull(controller.lastPersistenceFailureType)
        assertEquals(controller.state, repository.savedState)
    }
}

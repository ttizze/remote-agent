package dev.remoteagent.mobile

import kotlin.coroutines.Continuation
import kotlin.coroutines.EmptyCoroutineContext
import kotlin.coroutines.startCoroutine
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.joinAll
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject

class HostSessionsTest {
    @Test
    fun generations_replace_subscriptions_and_cancel_after_unlocking() {
        val coordinator = hostSessions()
        val first = coordinator.beginConnection("host-1")
        var cancelled = false
        var wasCurrentWhenCancelled = true
        val oldSubscription = HostEventSubscription {
            cancelled = true
            wasCurrentWhenCancelled = coordinator.isCurrent("host-1", first)
        }

        assertTrue(coordinator.installSubscription("host-1", first, oldSubscription))
        val second = coordinator.beginConnection("host-1")

        assertEquals(1L, first)
        assertEquals(2L, second)
        assertTrue(cancelled)
        assertFalse(wasCurrentWhenCancelled)
        assertFalse(coordinator.isCurrent("host-1", first))
        assertTrue(coordinator.isCurrent("host-1", second))

        val newSubscription = HostEventSubscription { cancelled = true }
        assertTrue(coordinator.installSubscription("host-1", second, newSubscription))
        coordinator.retireHost("host-1")
        assertFalse(coordinator.isCurrent("host-1", second))
    }

    @Test
    fun stale_subscription_install_is_rejected_and_cancelled() {
        val coordinator = hostSessions()
        val first = coordinator.beginConnection("host-1")
        val second = coordinator.beginConnection("host-1")
        var cancelled = false

        assertFalse(coordinator.installSubscription("host-1", first, HostEventSubscription { cancelled = true }))

        assertTrue(cancelled)
        assertTrue(coordinator.isCurrent("host-1", second))
    }

    @Test
    fun read_barriers_are_isolated_by_thread_and_stale_tokens_cannot_finish() {
        val coordinator = hostSessions()
        val generation = coordinator.beginConnection("host-1")
        val first = assertNotNull(coordinator.beginRead("host-1", "thread-1", generation))
        val second = assertNotNull(coordinator.beginRead("host-1", "thread-2", generation))

        val firstEvent =
            codexMessage(
                method = "turn/started",
                params =
                    buildJsonObject {
                        put("threadId", JsonPrimitive("thread-1"))
                        put("turn", (CodexTurn("turn-1", TurnStatus.InProgress)).fixtureJson())
                    },
            )
        val secondEvent =
            codexMessage(
                method = "turn/started",
                params =
                    buildJsonObject {
                        put("threadId", JsonPrimitive("thread-2"))
                        put("turn", (CodexTurn("turn-2", TurnStatus.InProgress)).fixtureJson())
                    },
            )
        assertEquals(HostReadBufferResult.Buffered, coordinator.bufferEvent(first, firstEvent))
        assertEquals(HostReadBufferResult.NotBuffered, coordinator.bufferEvent(first, secondEvent))
        assertEquals(HostReadBufferResult.Buffered, coordinator.bufferEvent(second, secondEvent))

        val firstCompletion = assertNotNull(coordinator.finishRead(first))
        val secondCompletion = assertNotNull(coordinator.finishRead(second))
        assertEquals(listOf(firstEvent), firstCompletion.events)
        assertEquals(listOf(secondEvent), secondCompletion.events)

        val replaced = assertNotNull(coordinator.beginRead("host-1", "thread-1", generation))
        val replacement = assertNotNull(coordinator.beginRead("host-1", "thread-1", generation))
        assertNull(coordinator.finishRead(replaced))
        assertNotNull(coordinator.finishRead(replacement))
    }

    @Test
    fun read_barrier_overflow_returns_retry_signal_for_bytes_and_event_count() {
        val byteLimited = hostSessions(MobileCacheLimits(maxApproximateBytes = 64))
        val generation = byteLimited.beginConnection("host-1")
        val token = assertNotNull(byteLimited.beginRead("host-1", "thread-1", generation))
        val large =
            codexMessage(
                method = "item/agentMessage/delta",
                params =
                    buildJsonObject {
                        put("threadId", JsonPrimitive("thread-1"))
                        put("turnId", JsonPrimitive("turn-1"))
                        put("itemId", JsonPrimitive("item-1"))
                        put("delta", JsonPrimitive("x".repeat(256)))
                    },
            )

        assertEquals(HostReadBufferResult.Overflowed, byteLimited.bufferEvent(token, large))
        assertTrue(byteLimited.bufferEvent(token, large).retryRequired)
        val byteCompletion = assertNotNull(byteLimited.finishRead(token))
        assertTrue(byteCompletion.retryRequired)
        assertTrue(byteCompletion.events.isEmpty())

        val countLimited = hostSessions()
        val countGeneration = countLimited.beginConnection("host-1")
        val countToken = assertNotNull(countLimited.beginRead("host-1", "thread-1", countGeneration))
        repeat(256) {
            assertEquals(
                HostReadBufferResult.Buffered,
                countLimited.bufferEvent(
                    countToken,
                    codexMessage(
                        method = "turn/started",
                        params =
                            buildJsonObject {
                                put("threadId", JsonPrimitive("thread-1"))
                                put("turn", (CodexTurn("turn-$it", TurnStatus.InProgress)).fixtureJson())
                            },
                    ),
                ),
            )
        }
        val overflow =
            codexMessage(
                method = "turn/started",
                params =
                    buildJsonObject {
                        put("threadId", JsonPrimitive("thread-1"))
                        put("turn", (CodexTurn("turn-256", TurnStatus.InProgress)).fixtureJson())
                    },
            )
        assertEquals(HostReadBufferResult.Overflowed, countLimited.bufferEvent(countToken, overflow))
        assertTrue(assertNotNull(countLimited.finishRead(countToken)).retryRequired)
    }

    @Test
    fun read_completion_is_rejected_after_generation_changes() {
        val coordinator = hostSessions()
        val firstGeneration = coordinator.beginConnection("host-1")
        val token = assertNotNull(coordinator.beginRead("host-1", "thread-1", firstGeneration))
        coordinator.beginConnection("host-1")

        assertNull(coordinator.finishRead(token))
        assertNull(coordinator.beginRead("host-1", "thread-1", firstGeneration))
    }

    @Test
    fun connection_blocks_complete_for_each_host() {
        val coordinator = hostSessions()

        var firstResult = 0
        var secondResult = 0
        runSuspend {
            coordinator.withHostConnection("host-1") { firstResult = 1 }
            coordinator.withHostConnection("host-2") { secondResult = 2 }
        }
        assertEquals(1, firstResult)
        assertEquals(2, secondResult)
    }

    @Test
    fun same_host_lifecycle_waits_while_another_host_progresses() = runBlocking {
        val coordinator = hostSessions()
        val releaseFirstHost = CompletableDeferred<Unit>()
        val sameHostEntered = CompletableDeferred<Unit>()
        val otherHostEntered = CompletableDeferred<Unit>()
        val first =
            launch(start = CoroutineStart.UNDISPATCHED) {
                coordinator.withHostConnection("host-1") { releaseFirstHost.await() }
            }
        val sameHost = launch { coordinator.withHostConnection("host-1") { sameHostEntered.complete(Unit) } }
        val otherHost = launch { coordinator.withHostConnection("host-2") { otherHostEntered.complete(Unit) } }

        otherHostEntered.await()
        assertFalse(sameHostEntered.isCompleted)
        releaseFirstHost.complete(Unit)
        joinAll(first, sameHost, otherHost)
        assertTrue(sameHostEntered.isCompleted)
    }

    private fun runSuspend(block: suspend () -> Unit) {
        var completion: Result<Unit>? = null
        block.startCoroutine(
            object : Continuation<Unit> {
                override val context = EmptyCoroutineContext

                override fun resumeWith(result: Result<Unit>) {
                    completion = result
                }
            }
        )
        (completion ?: error("test coroutine suspended unexpectedly")).getOrThrow()
    }
}

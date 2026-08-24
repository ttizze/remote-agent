package dev.remoteagent.mobile

import kotlin.coroutines.Continuation
import kotlin.coroutines.EmptyCoroutineContext
import kotlin.coroutines.startCoroutine
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNotSame
import kotlin.test.assertNull
import kotlin.test.assertSame
import kotlin.test.assertTrue

class HostSessionCoordinatorTest {
    @Test
    fun generations_replace_subscriptions_and_cancel_after_unlocking() {
        val coordinator = HostSessionCoordinator()
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
        val coordinator = HostSessionCoordinator()
        val first = coordinator.beginConnection("host-1")
        val second = coordinator.beginConnection("host-1")
        var cancelled = false

        assertFalse(
            coordinator.installSubscription("host-1", first, HostEventSubscription { cancelled = true }),
        )

        assertTrue(cancelled)
        assertTrue(coordinator.isCurrent("host-1", second))
    }

    @Test
    fun read_barriers_are_isolated_by_thread_and_stale_tokens_cannot_finish() {
        val coordinator = HostSessionCoordinator()
        val generation = coordinator.beginConnection("host-1")
        val first = assertNotNull(coordinator.beginRead("host-1", "thread-1", generation))
        val second = assertNotNull(coordinator.beginRead("host-1", "thread-2", generation))

        val firstEvent = ThreadEvent.TurnStarted("thread-1", "turn-1", TurnStatus.InProgress)
        val secondEvent = ThreadEvent.TurnStarted("thread-2", "turn-2", TurnStatus.InProgress)
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
        val byteLimited = HostSessionCoordinator(MobileCacheLimits(maxApproximateBytes = 64))
        val generation = byteLimited.beginConnection("host-1")
        val token = assertNotNull(byteLimited.beginRead("host-1", "thread-1", generation))
        val large = ThreadEvent.AgentMessageDelta("thread-1", "turn-1", "item-1", "x".repeat(256))

        assertEquals(HostReadBufferResult.Overflowed, byteLimited.bufferEvent(token, large))
        assertTrue(byteLimited.bufferEvent(token, large).retryRequired)
        val byteCompletion = assertNotNull(byteLimited.finishRead(token))
        assertTrue(byteCompletion.retryRequired)
        assertTrue(byteCompletion.events.isEmpty())

        val countLimited = HostSessionCoordinator()
        val countGeneration = countLimited.beginConnection("host-1")
        val countToken = assertNotNull(countLimited.beginRead("host-1", "thread-1", countGeneration))
        repeat(256) {
            assertEquals(
                HostReadBufferResult.Buffered,
                countLimited.bufferEvent(
                    countToken,
                    ThreadEvent.TurnStarted("thread-1", "turn-$it", TurnStatus.InProgress),
                ),
            )
        }
        assertEquals(
            HostReadBufferResult.Overflowed,
            countLimited.bufferEvent(countToken, ThreadEvent.TurnStarted("thread-1", "turn-256", TurnStatus.InProgress)),
        )
        assertTrue(assertNotNull(countLimited.finishRead(countToken)).retryRequired)
    }

    @Test
    fun read_completion_is_rejected_after_generation_changes() {
        val coordinator = HostSessionCoordinator()
        val firstGeneration = coordinator.beginConnection("host-1")
        val token = assertNotNull(coordinator.beginRead("host-1", "thread-1", firstGeneration))
        coordinator.beginConnection("host-1")

        assertNull(coordinator.finishRead(token))
        assertNull(coordinator.beginRead("host-1", "thread-1", firstGeneration))
    }

    @Test
    fun each_host_has_one_connect_mutex_and_hosts_do_not_share_it() {
        val coordinator = HostSessionCoordinator()
        val firstHostMutex = coordinator.hostConnectionMutex("host-1")
        assertSame(firstHostMutex, coordinator.hostConnectionMutex("host-1"))
        assertNotSame(firstHostMutex, coordinator.hostConnectionMutex("host-2"))

        var firstResult = 0
        var secondResult = 0
        runSuspend {
            coordinator.withHostConnection("host-1") { firstResult = 1 }
            coordinator.withHostConnection("host-2") { secondResult = 2 }
        }
        assertEquals(1, firstResult)
        assertEquals(2, secondResult)
    }

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

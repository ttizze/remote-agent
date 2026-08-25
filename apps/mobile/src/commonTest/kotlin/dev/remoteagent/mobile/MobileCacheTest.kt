package dev.remoteagent.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue
import kotlinx.serialization.json.Json

class MobileCacheTest {
    private val limits = MobileCacheLimits(maxThreads = 2, maxItemsPerThread = 2, maxTextCharacters = 20, maxApproximateBytes = 1_024)

    @Test
    fun read_replaces_snapshot_then_applies_buffered_events_in_order() {
        val snapshot = ThreadSnapshot(
            summary = summary("thread-1"),
            turns = listOf(CodexTurn("turn-1", TurnStatus.InProgress, listOf(CodexItem.AgentMessage("item-1", "old")))),
        )
        val result = ThreadReadResult(
            thread = snapshot,
            bufferedEvents = listOf(
                ThreadEvent.AgentMessageDelta("thread-1", "turn-1", "item-1", " new"),
                ThreadEvent.ItemCompleted("thread-1", "turn-1", CodexItem.AgentMessage("item-2", "second")),
                ThreadEvent.TurnCompleted("thread-1", "turn-1", TurnStatus.Completed),
            ),
        )

        val cache = reconcileThreadRead(MobileCache(), "host-1", result, limits)
        val turn = cache.snapshot("host-1", "thread-1")!!.turns.single()

        assertEquals(TurnStatus.Completed, turn.status)
        assertEquals(listOf("item-1", "item-2"), turn.items.map { it.id })
        assertEquals("old new", (turn.items.first() as CodexItem.AgentMessage).text)
    }

    @Test
    fun live_item_events_are_ignored_until_their_snapshot_and_turn_are_known() {
        val event = ThreadEvent.ItemStarted("thread-1", "turn-1", CodexItem.UserMessage("item-1", "hello"))
        val noSnapshot = applyLiveEvent(MobileCache(), "host-1", event, limits)
        val snapshot = ThreadSnapshot(summary("thread-1"), emptyList())
        val noTurn = applyLiveEvent(
            reconcileThreadRead(MobileCache(), "host-1", ThreadReadResult(snapshot, emptyList()), limits),
            "host-1",
            event,
            limits,
        )

        assertTrue(noSnapshot.profile("host-1").snapshots.isEmpty())
        assertTrue(noTurn.snapshot("host-1", "thread-1")!!.turns.isEmpty())
    }

    @Test
    fun cache_is_profile_isolated_and_enforces_thread_item_and_byte_bounds() {
        val large = "x".repeat(500)
        val first = ThreadSnapshot(
            summary("one", 1),
            listOf(CodexTurn("turn", TurnStatus.Completed, listOf(
                CodexItem.AgentMessage("a", large), CodexItem.AgentMessage("b", large), CodexItem.AgentMessage("c", large),
            ))),
        )
        var cache = reconcileThreadRead(MobileCache(), "host-a", ThreadReadResult(first, emptyList()), limits)
        cache = reconcileThreadList(cache, "host-a", listOf(summary("one", 1), summary("two", 2), summary("three", 3)), limits)
        cache = reconcileThreadRead(cache, "host-b", ThreadReadResult(ThreadSnapshot(summary("other"), emptyList()), emptyList()), limits)

        val hostA = cache.profile("host-a")
        assertEquals(listOf("two", "three"), hostA.threadList.map { it.id })
        assertFalse("one" in hostA.snapshots)
        assertEquals(listOf("other"), cache.profile("host-b").threadList.map { it.id })
        assertTrue(approximateCacheBytes(MobileCache(mapOf("host-a" to hostA))) <= limits.maxApproximateBytes)
        assertTrue(
            approximateCacheBytes(MobileCache(mapOf("host-b" to cache.profile("host-b")))) <=
                limits.maxApproximateBytes,
        )
    }

    @Test
    fun read_ignores_buffered_events_for_another_thread() {
        val snapshot = ThreadSnapshot(summary("thread-1"), emptyList())
        val cache = reconcileThreadRead(
            MobileCache(),
            "host-1",
            ThreadReadResult(
                snapshot,
                listOf(ThreadEvent.TurnStarted("thread-2", "turn-2", TurnStatus.InProgress)),
            ),
            limits,
        )

        assertTrue(cache.snapshot("host-1", "thread-1")!!.turns.isEmpty())
        assertTrue(cache.snapshot("host-1", "thread-2") == null)
    }

    @Test
    fun raw_codex_payloads_count_toward_the_cache_bound() {
        val raw = RawCodexMessage.Notification(
            method = "future/notification",
            params = Json.parseToJsonElement("\"${"x".repeat(2_000)}\""),
        )
        val cache = retainRawMessage(MobileCache(), "host-1", raw, limits)

        assertTrue(approximateCacheBytes(cache) <= limits.maxApproximateBytes)
    }

    @Test
    fun projects_keep_codex_position_order_without_deriving_membership_from_cwd() {
        val cache = reconcileProjectList(
            MobileCache(),
            "host-1",
            listOf(project("second", 20), project("first", 10)),
            limits,
        )

        assertEquals(listOf("first", "second"), cache.profile("host-1").projects.map { it.id })
    }

    private fun project(id: String, position: Long) = CodexProject(
        id = id,
        name = id,
        roots = listOf(WorkingDirectory("/workspace/$id")),
        position = position,
        createdAtMs = 1,
        updatedAtMs = 1,
    )

    private fun summary(id: String, updatedAtMs: Long = 1) = ThreadSummary(
        id = id,
        name = id,
        preview = "preview-$id",
        workingDirectory = WorkingDirectory("/workspace/$id"),
        createdAtMs = updatedAtMs,
        updatedAtMs = updatedAtMs,
        status = ThreadStatus.Idle,
    )
}

package dev.remoteagent.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue
import kotlinx.serialization.json.Json

class MobileHistoryTest {
    private val limits = MobileCacheLimits(maxThreads = 2, maxTurnsPerThread = 2, maxApproximateBytes = 1_024)

    @Test
    fun refresh_matches_duplicate_turn_occurrences_and_keeps_fetched_details() {
        fun snapshot(turns: String) =
            codexThreadFromResponse(
                Json.parseToJsonElement("""{"thread":{"id":"thread","historyCursor":null,"turns":[$turns]}}""")
            )
        val previous =
            snapshot(
                """
            {"id":"prefix","items":[]},
            {"id":"repeat","status":"inProgress","items":[{"id":"command","type":"commandExecution","aggregatedOutput":"first-full"}]},
            {"id":"repeat","status":"inProgress","items":[{"id":"command","type":"commandExecution","aggregatedOutput":"second-full"}]}
        """
            )
        val fresh =
            snapshot(
                """
            {"id":"repeat","status":"completed","deferredItemIds":["command"],"items":[{"id":"command","type":"commandExecution"}]},
            {"id":"repeat","status":"completed","deferredItemIds":["command"],"items":[{"id":"command","type":"commandExecution"}]}
        """
            )
        val merged = mergeHistoryRefresh(previous, fresh)
        assertEquals(listOf("prefix", "repeat", "repeat"), merged.turns.map { it.id })
        assertEquals(
            listOf("first-full", "second-full"),
            merged.turns.drop(1).map { (it.items.single() as CodexItem.CommandExecution).output },
        )
        assertTrue(merged.turns.drop(1).all { it.status == TurnStatus.Completed })
        assertTrue(merged.turns.drop(1).all { it.raw?.get("deferredItemIds").toString() == "[]" })
        val olderSummary =
            snapshot(
                """{"id":"repeat","deferredItemIds":["command"],
                "items":[{"id":"command","type":"commandExecution"}],"itemsHasMore":false}"""
            )
        val prepended = mergeOlderHistory(merged, olderSummary, "repeat", null)
        assertEquals("first-full", (prepended.turns[1].items.single() as CodexItem.CommandExecution).output)
        assertEquals("[]", prepended.turns[1].raw?.get("deferredItemIds").toString())
    }

    @Test
    fun summary_only_refresh_keeps_items_but_full_unpaged_read_is_authoritative() {
        fun snapshot(raw: String) = codexThreadFromResponse(Json.parseToJsonElement(raw))
        val previous =
            snapshot(
                """{"thread":{"id":"thread","historyCursor":"older","turns":[
            {"id":"prefix","items":[]},
            {"id":"turn","status":"inProgress","items":[{"id":"answer","type":"agentMessage","text":"complete"}],"itemsHasMore":false}
        ]}}"""
            )
        val fresh =
            snapshot(
                """{"thread":{"id":"thread","historyCursor":"fresh","turns":[
            {"id":"turn","status":"completed","items":[],"itemsHasMore":true}
        ]}}"""
            )
        val merged = mergeHistoryRefresh(previous, fresh)
        assertEquals("complete", (merged.turns.last().items.single() as CodexItem.AgentMessage).text)
        assertEquals(TurnStatus.Completed, merged.turns.last().status)
        assertEquals("older", merged.olderTurnsCursor)
        assertFalse(merged.turns.last().hasOlderItems)
        val authoritative = fresh.copy(raw = null)
        assertEquals(authoritative, mergeHistoryRefresh(previous, authoritative))
    }

    @Test
    fun an_unhydrated_turn_is_loadable_without_a_cursor_and_retains_its_status() {
        val current =
            codexThreadFromResponse(
                Json.parseToJsonElement(
                    """{"thread":{"id":"thread-1","turns":[
            {"id":"turn","status":"interrupted","items":[],"itemsHasMore":true,"itemsNextCursor":null}
        ]}}"""
                )
            )
        assertTrue(current.turns.single().hasOlderItems)
        assertEquals(null, current.turns.single().olderItemsCursor)
        val page =
            codexThreadFromResponse(
                Json.parseToJsonElement(
                    """
                {
                  "thread": {
                    "id": "thread-1",
                    "turns": [
                      {
                        "id": "turn",
                        "items": [
                          {
                            "id": "reply",
                            "type": "agentMessage",
                            "text": "older reply"
                          }
                        ],
                        "itemsHasMore": false,
                        "itemsNextCursor": null
                      }
                    ]
                  }
                }
            """
                )
            )
        val loaded = mergeOlderHistory(current, page, "turn", null).turns.single()
        assertEquals(TurnStatus.Interrupted, loaded.status)
        assertEquals(listOf("reply"), loaded.items.map { it.id })
        assertFalse(loaded.hasOlderItems)
    }

    @Test
    fun older_items_preserve_live_values_and_tail_refresh_keeps_the_loaded_prefix() {
        fun page(items: String, cursor: String) =
            codexThreadFromResponse(
                Json.parseToJsonElement(
                    """
            {"thread":{"id":"thread-1","historyCursor":"turn-cursor","turns":[
                {"id":"turn","status":"inProgress","itemsNextCursor":$cursor,"items":[$items]}
            ]}}
        """
                )
            )
        val current = page("""{"id":"b","type":"agentMessage","text":"live"}""", "\"items-cursor\"")
        val older =
            page(
                """{"id":"a","type":"userMessage","content":[{"type":"text","text":"question"}]},
                {"id":"b","type":"agentMessage","text":"stale"}""",
                "null",
            )
        val merged = mergeOlderHistory(current, older, "turn", "items-cursor")
        assertEquals(listOf("a", "b"), merged.turns.single().items.map { it.id })
        assertEquals("live", (merged.turns.single().items.last() as CodexItem.AgentMessage).text)
        assertEquals(null, merged.turns.single().olderItemsCursor)
        val initial = reconcileThreadRead(MobileCache(), "host", merged, limits)
        val fresh = page("""{"id":"b","type":"agentMessage","text":"finished"}""", "\"items-cursor\"")
        val refreshed = reconcileThreadRead(initial, "host", fresh, limits).snapshot("host", "thread-1")!!
        assertEquals(listOf("a", "b"), refreshed.turns.single().items.map { it.id })
        assertEquals("finished", (refreshed.turns.single().items.last() as CodexItem.AgentMessage).text)
        assertEquals(null, refreshed.turns.single().olderItemsCursor)
    }
}

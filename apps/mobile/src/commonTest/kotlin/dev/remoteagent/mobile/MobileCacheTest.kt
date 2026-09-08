package dev.remoteagent.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertTrue
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject

class MobileCacheTest {
    private val limits = MobileCacheLimits(maxThreads = 2, maxTurnsPerThread = 2, maxApproximateBytes = 1_024)

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
        val loaded = mergeOlderHistory(current, page, "turn").turns.single()
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
        val merged = mergeOlderHistory(current, older, "turn")
        assertEquals(listOf("a", "b"), merged.turns.single().items.map { it.id })
        assertEquals("live", (merged.turns.single().items.last() as CodexItem.AgentMessage).text)
        assertEquals(null, merged.turns.single().olderItemsCursor)
        val initial = reconcileThreadRead(MobileCache(), "host", ThreadReadResult(merged, emptyList()), limits)
        val fresh = page("""{"id":"b","type":"agentMessage","text":"finished"}""", "\"items-cursor\"")
        val refreshed =
            reconcileThreadRead(initial, "host", ThreadReadResult(fresh, emptyList()), limits)
                .snapshot("host", "thread-1")!!
        assertEquals(listOf("a", "b"), refreshed.turns.single().items.map { it.id })
        assertEquals("finished", (refreshed.turns.single().items.last() as CodexItem.AgentMessage).text)
        assertEquals(null, refreshed.turns.single().olderItemsCursor)
    }

    @Test
    fun older_turn_pages_deduplicate_and_replace_the_older_cursor() {
        fun page(ids: List<String>, cursor: String?) =
            ThreadSnapshot(
                summary("thread-1"),
                ids.map { CodexTurn(it, TurnStatus.Completed) },
                Json.parseToJsonElement("""{"historyCursor":${cursor?.let { "\"$it\"" } ?: "null"}}""").jsonObject,
            )
        val result = mergeOlderHistory(page(listOf("b", "c"), "opaque"), page(listOf("a", "b"), null), null)
        assertEquals(listOf("a", "b", "c"), result.turns.map { it.id })
        assertEquals(null, result.olderTurnsCursor)
    }

    @Test
    fun read_replaces_snapshot_then_applies_buffered_events_in_order() {
        val snapshot =
            ThreadSnapshot(
                summary = summary("thread-1"),
                turns =
                    listOf(CodexTurn("turn-1", TurnStatus.InProgress, listOf(CodexItem.AgentMessage("item-1", "old")))),
            )
        val result =
            ThreadReadResult(
                thread = snapshot,
                bufferedEvents =
                    listOf(
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
        val noTurn =
            applyLiveEvent(
                reconcileThreadRead(MobileCache(), "host-1", ThreadReadResult(snapshot, emptyList()), limits),
                "host-1",
                event,
                limits,
            )

        assertTrue(noSnapshot.profile("host-1").snapshots.isEmpty())
        assertTrue(noTurn.snapshot("host-1", "thread-1")!!.turns.isEmpty())
    }

    @Test
    fun a_title_window_refresh_keeps_the_open_body_receiving_live_updates() {
        val opened =
            ThreadSnapshot(
                summary("open"),
                listOf(CodexTurn("turn", TurnStatus.InProgress, listOf(CodexItem.AgentMessage("reply", "First")))),
            )
        var cache = reconcileThreadRead(MobileCache(), "host-1", ThreadReadResult(opened, emptyList()), limits)
        // A recent-title window can omit an older open thread, and a new
        // in-memory thread has no persisted list entry until its first turn.
        cache = reconcileThreadList(cache, "host-1", listOf(summary("other")), limits)
        cache =
            applyLiveEvent(cache, "host-1", ThreadEvent.AgentMessageDelta("open", "turn", "reply", " second"), limits)
        val body = assertNotNull(cache.snapshot("host-1", "open"))
        assertEquals("First second", (body.turns.single().items.single() as CodexItem.AgentMessage).text)
        assertEquals(listOf("other"), cache.profile("host-1").threadList.map { it.id })
    }

    @Test
    fun snapshot_limit_does_not_truncate_the_newest_first_thread_list() {
        val summaries = (1..21).map { summary("thread-$it", updatedAtMs = it.toLong()) }

        val cache = reconcileThreadList(MobileCache(), "host-1", summaries, MobileCacheLimits())

        assertEquals((21 downTo 1).map { "thread-$it" }, cache.profile("host-1").threadList.map { it.id })
    }

    @Test
    fun thread_list_is_sorted_by_updated_at_and_duplicate_ids_keep_the_newest_summary() {
        val stale = summary("thread-1", updatedAtMs = 1).copy(preview = "stale")
        val newest = summary("thread-1", updatedAtMs = 5).copy(preview = "newest")
        val summaries =
            listOf(summary("thread-3", updatedAtMs = 3), stale, summary("thread-2", updatedAtMs = 2), newest)

        val cache = reconcileThreadList(MobileCache(), "host-1", summaries, MobileCacheLimits(maxThreads = 3))

        assertEquals(listOf("thread-1", "thread-3", "thread-2"), cache.profile("host-1").threadList.map { it.id })
        assertEquals("newest", cache.profile("host-1").threadList.first().preview)
    }

    @Test
    fun snapshots_keep_the_latest_ten_turns_with_all_items_and_text_unchanged() {
        val largeText = "x".repeat(5_000)
        val turns =
            (1..11).map { turnNumber ->
                CodexTurn(
                    id = "turn-$turnNumber",
                    status = TurnStatus.Completed,
                    items =
                        listOf(
                            CodexItem.UserMessage("user-$turnNumber", largeText),
                            CodexItem.AgentMessage("agent-$turnNumber", largeText),
                            CodexItem.Reasoning("reasoning-$turnNumber", largeText),
                            CodexItem.CommandExecution(
                                id = "command-$turnNumber",
                                command = largeText,
                                cwd = largeText,
                                output = largeText,
                                status = CommandExecutionStatus.Completed,
                            ),
                            CodexItem.FileChange(
                                id = "file-$turnNumber",
                                changes = listOf(FileUpdateChange(largeText, FileUpdateKind.Update, largeText)),
                                status = FileChangeStatus.Completed,
                            ),
                        ),
                )
            }
        val summary =
            summary("thread-1")
                .copy(name = largeText, preview = largeText, workingDirectory = WorkingDirectory(largeText))
        val snapshot = ThreadSnapshot(summary = summary, turns = turns)
        val limits = MobileCacheLimits(maxThreads = 1, maxTurnsPerThread = 10, maxApproximateBytes = 1_024)

        val cached =
            reconcileThreadRead(MobileCache(), "host-1", ThreadReadResult(snapshot, emptyList()), limits)
                .snapshot("host-1", "thread-1")!!

        assertEquals(turns.drop(1), cached.turns)
        assertEquals(summary, cached.summary)

        val codexProject = project("project-1", 1).copy(name = largeText, roots = listOf(WorkingDirectory(largeText)))
        val projectCache = reconcileProjectList(MobileCache(), "host-1", listOf(codexProject), limits)
        assertEquals(listOf(codexProject), projectCache.profile("host-1").projects)
    }

    @Test
    fun cache_is_profile_isolated_and_enforces_thread_bounds() {
        val first =
            ThreadSnapshot(
                summary("one", 1),
                listOf(
                    CodexTurn("turn-1", TurnStatus.Completed),
                    CodexTurn("turn-2", TurnStatus.Completed),
                    CodexTurn("turn-3", TurnStatus.Completed),
                ),
            )
        var cache = reconcileThreadRead(MobileCache(), "host-a", ThreadReadResult(first, emptyList()), limits)
        assertEquals(listOf("turn-2", "turn-3"), cache.snapshot("host-a", "one")!!.turns.map { it.id })
        cache =
            reconcileThreadList(
                cache,
                "host-a",
                listOf(summary("one", 1), summary("two", 2), summary("three", 3)),
                limits,
            )
        cache =
            reconcileThreadRead(
                cache,
                "host-b",
                ThreadReadResult(ThreadSnapshot(summary("other"), emptyList()), emptyList()),
                limits,
            )

        val hostA = cache.profile("host-a")
        assertEquals(listOf("three", "two", "one"), hostA.threadList.map { it.id })
        assertTrue("one" in hostA.snapshots)
        assertEquals(listOf("other"), cache.profile("host-b").threadList.map { it.id })
        assertEquals(setOf("other"), cache.profile("host-b").snapshots.keys)
    }

    @Test
    fun read_ignores_buffered_events_for_another_thread() {
        val snapshot = ThreadSnapshot(summary("thread-1"), emptyList())
        val cache =
            reconcileThreadRead(
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
    fun completed_turn_timing_survives_a_late_started_notification() {
        val snapshot = ThreadSnapshot(summary("thread-1"), emptyList())
        var cache = reconcileThreadRead(MobileCache(), "host-1", ThreadReadResult(snapshot, emptyList()), limits)
        cache =
            applyLiveEvent(
                cache,
                "host-1",
                ThreadEvent.TurnCompleted(
                    threadId = "thread-1",
                    turnId = "turn-1",
                    status = TurnStatus.Completed,
                    startedAtMs = 1_000,
                    completedAtMs = 6_000,
                    durationMs = 5_000,
                ),
                limits,
            )
        cache =
            applyLiveEvent(
                cache,
                "host-1",
                ThreadEvent.TurnStarted("thread-1", "turn-1", TurnStatus.InProgress, startedAtMs = 1_000),
                limits,
            )

        val turn = cache.snapshot("host-1", "thread-1")!!.turns.single()
        assertEquals(TurnStatus.Completed, turn.status)
        assertEquals(1_000, turn.startedAtMs)
        assertEquals(6_000, turn.completedAtMs)
        assertEquals(5_000, turn.durationMs)
    }

    @Test
    fun pending_server_requests_are_added_and_resolved_idempotently() {
        val snapshot = ThreadSnapshot(summary("thread-1"), listOf(CodexTurn("turn-1", TurnStatus.InProgress)))
        var cache = reconcileThreadRead(MobileCache(), "host-1", ThreadReadResult(snapshot, emptyList()), limits)
        val request =
            CodexServerRequest(
                id = "request-1",
                method = "item/tool/requestUserInput",
                params =
                    Json.parseToJsonElement(
                            """{"threadId":"thread-1","turnId":"turn-1","questions":[{"question":"Which?"}]}"""
                        )
                        .jsonObject,
            )

        cache = applyLiveEvent(cache, "host-1", ThreadEvent.RequestStarted("thread-1", "turn-1", request), limits)
        cache = applyLiveEvent(cache, "host-1", ThreadEvent.RequestStarted("thread-1", "turn-1", request), limits)
        assertEquals(listOf(request), cache.snapshot("host-1", "thread-1")!!.turns.single().pendingRequests)

        cache =
            applyLiveEvent(cache, "host-1", ThreadEvent.RequestResolved("thread-1", requestId = "request-1"), limits)
        cache =
            applyLiveEvent(cache, "host-1", ThreadEvent.RequestResolved("thread-1", requestId = "request-1"), limits)
        assertTrue(cache.snapshot("host-1", "thread-1")!!.turns.single().pendingRequests.isEmpty())
    }

    @Test
    fun completed_turn_event_preserves_its_terminal_error() {
        val snapshot = ThreadSnapshot(summary("thread-1"), listOf(CodexTurn("turn-1", TurnStatus.InProgress)))
        var cache = reconcileThreadRead(MobileCache(), "host-1", ThreadReadResult(snapshot, emptyList()), limits)
        val error =
            CodexTurnError(
                message = "context full",
                codexErrorInfo = Json.parseToJsonElement("\"contextWindowExceeded\""),
            )

        cache =
            applyLiveEvent(
                cache,
                "host-1",
                ThreadEvent.TurnCompleted("thread-1", "turn-1", TurnStatus.Failed, error = error),
                limits,
            )

        val turn = cache.snapshot("host-1", "thread-1")!!.turns.single()
        assertEquals(TurnStatus.Failed, turn.status)
        assertEquals(error, turn.error)
    }

    @Test
    fun successful_completion_clears_a_transient_retrying_stream_error() {
        val retrying = CodexTurnError(message = "disconnected", willRetry = true)
        val snapshot =
            ThreadSnapshot(summary("thread-1"), listOf(CodexTurn("turn-1", TurnStatus.InProgress, error = retrying)))
        var cache = reconcileThreadRead(MobileCache(), "host-1", ThreadReadResult(snapshot, emptyList()), limits)

        cache =
            applyLiveEvent(
                cache,
                "host-1",
                ThreadEvent.TurnCompleted("thread-1", "turn-1", TurnStatus.Completed),
                limits,
            )

        assertEquals(null, cache.snapshot("host-1", "thread-1")!!.turns.single().error)
    }

    @Test
    fun late_retrying_stream_error_cannot_reopen_a_successful_turn() {
        val snapshot = ThreadSnapshot(summary("thread-1"), listOf(CodexTurn("turn-1", TurnStatus.Completed)))
        var cache = reconcileThreadRead(MobileCache(), "host-1", ThreadReadResult(snapshot, emptyList()), limits)

        cache =
            applyLiveEvent(
                cache,
                "host-1",
                ThreadEvent.Error(
                    threadId = "thread-1",
                    turnId = "turn-1",
                    error = CodexTurnError(message = "disconnected", willRetry = true),
                    willRetry = true,
                ),
                limits,
            )

        assertEquals(null, cache.snapshot("host-1", "thread-1")!!.turns.single().error)
    }

    @Test
    fun thread_status_updates_the_list_and_snapshot_and_auto_review_obeys_visibility_lifecycle() {
        val summary = summary("thread-1")
        var cache =
            reconcileThreadRead(
                MobileCache(),
                "host-1",
                ThreadReadResult(
                    ThreadSnapshot(summary, listOf(CodexTurn("turn-1", TurnStatus.InProgress))),
                    emptyList(),
                ),
                limits,
            )
        val active = ThreadStatus.Active(listOf("waitingOnApproval"))
        cache = applyLiveEvent(cache, "host-1", ThreadEvent.ThreadStatusChanged("thread-1", status = active), limits)
        assertEquals(active, cache.profile("host-1").threadList.single().status)
        assertEquals(active, cache.snapshot("host-1", "thread-1")!!.summary.status)

        val raw = Json.parseToJsonElement("""{"review":{"status":"denied","rationale":"too risky"}}""").jsonObject
        cache =
            applyLiveEvent(
                cache,
                "host-1",
                ThreadEvent.GuardianReviewChanged("thread-1", "turn-1", "review-1", "denied", raw),
                limits,
            )
        assertEquals(
            "automaticApprovalReview",
            (cache.snapshot("host-1", "thread-1")!!.turns.single().items.single() as CodexItem.Unknown).codexType,
        )

        cache =
            applyLiveEvent(
                cache,
                "host-1",
                ThreadEvent.GuardianReviewChanged("thread-1", "turn-1", "review-1", "approved", raw),
                limits,
            )
        assertTrue(cache.snapshot("host-1", "thread-1")!!.turns.single().items.isEmpty())
    }

    @Test
    fun read_keeps_an_oversized_raw_snapshot_and_its_typed_content() {
        val raw = Json.parseToJsonElement("{\"payload\":\"${"x".repeat(2_000)}\"}").jsonObject
        val summary = summary("oversized").copy(raw = raw)
        val turn =
            CodexTurn(
                id = "turn-oversized",
                status = TurnStatus.Completed,
                items = listOf(CodexItem.AgentMessage("item-1", "visible")),
                raw = raw,
            )
        val snapshot = ThreadSnapshot(summary = summary, turns = listOf(turn), raw = raw)

        val cache =
            reconcileThreadRead(
                MobileCache(),
                "host-1",
                ThreadReadResult(snapshot, emptyList()),
                limits.copy(maxApproximateBytes = 64),
            )

        val cached = assertNotNull(cache.snapshot("host-1", "oversized"))
        assertEquals(raw, cached.raw)
        assertEquals(raw, cached.summary.raw)
        assertEquals(raw, cached.turns.single().raw)
        assertEquals("visible", (cached.turns.single().items.single() as CodexItem.AgentMessage).text)
    }

    @Test
    fun projects_keep_codex_position_order_without_deriving_membership_from_cwd() {
        val cache =
            reconcileProjectList(MobileCache(), "host-1", listOf(project("second", 20), project("first", 10)), limits)

        assertEquals(listOf("first", "second"), cache.profile("host-1").projects.map { it.id })
    }

    @Test
    fun projects_follow_latest_conversation_activity_then_catalog_order_for_empty_projects() {
        var cache =
            reconcileProjectList(
                MobileCache(),
                "host-1",
                listOf(project("empty-b", 2), project("older", 0), project("recent", 5), project("empty-a", 1)),
                limits,
            )
        cache =
            reconcileThreadList(
                cache,
                "host-1",
                listOf(
                    summary("recent", 10).copy(projectId = "recent"),
                    summary("stale-in-recent", 1).copy(projectId = "recent"),
                    summary("older", 8).copy(projectId = "older"),
                    summary("chat", 20),
                ),
                limits,
            )
        assertEquals(listOf("recent", "older", "empty-a", "empty-b"), cache.profile("host-1").projects.map { it.id })
        cache =
            reconcileThreadRead(
                cache,
                "host-1",
                ThreadReadResult(ThreadSnapshot(summary("older", 30).copy(projectId = "older")), emptyList()),
                limits,
            )
        assertEquals(listOf("older", "recent", "empty-a", "empty-b"), cache.profile("host-1").projects.map { it.id })
    }

    @Test
    fun opening_an_old_conversation_keeps_its_body_with_a_full_snapshot_cache() {
        var cache = MobileCache()
        for (number in 1..3) {
            cache =
                reconcileThreadRead(
                    cache,
                    "host-1",
                    ThreadReadResult(ThreadSnapshot(summary("thread-$number", number.toLong())), emptyList()),
                    limits,
                )
        }
        cache =
            reconcileThreadRead(
                cache,
                "host-1",
                ThreadReadResult(
                    ThreadSnapshot(summary("old-thread", 0), listOf(CodexTurn("old-turn", TurnStatus.Completed))),
                    emptyList(),
                ),
                limits,
            )
        assertEquals(4, cache.profile("host-1").threadList.size)
        assertEquals(2, cache.profile("host-1").snapshots.size)
        assertEquals("old-turn", cache.snapshot("host-1", "old-thread")?.turns?.single()?.id)
    }

    private fun project(id: String, position: Long) =
        CodexProject(
            id = id,
            name = id,
            roots = listOf(WorkingDirectory("/workspace/$id")),
            position = position,
            createdAtMs = 1,
            updatedAtMs = 1,
        )

    private fun summary(id: String, updatedAtMs: Long = 1) =
        ThreadSummary(
            id = id,
            name = id,
            preview = "preview-$id",
            workingDirectory = WorkingDirectory("/workspace/$id"),
            createdAtMs = updatedAtMs,
            updatedAtMs = updatedAtMs,
            status = ThreadStatus.Idle,
        )
}

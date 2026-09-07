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
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.withTimeoutOrNull
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
    private val profile = HostProfile("runner-1", "Host", "wss://relay.example.test/socket/websocket", "runner-1", "device-key-ref")
    private val thread = snapshot("thread-1")

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
            val blocked = CompletableDeferred<GatewayResult<JsonElement>>()
            var calls = 0
            gateway.rawBlock = { method, _ ->
                assertEquals("host/thread/turns/list", method)
                calls++
                blocked.await()
            }
            val load = scope.launch { controller.loadOlderHistory(profile) }
            controller.loadOlderHistory(profile)
            assertEquals(1, calls)
            controller.dispatch(AppAction.ThreadSelected(profile.id, "thread-2"))
            blocked.complete(GatewayResult.Success(Json.parseToJsonElement("""{"thread":{"id":"thread-1","historyCursor":null,"turns":[{"id":"older","items":[]}]}}""")))
            load.join()
            assertEquals("thread-2", controller.state.selectedView.selectedThreadId)
            assertEquals(listOf("turn-1"), controller.state.cache.snapshot(profile.id, "thread-1")!!.turns.map { it.id })
            assertFalse(controller.state.selectedView.loadingHistory)
        } finally { scope.cancel() }
    }

    @Test
    fun title_list_expands_only_the_requested_section_and_app_entry_resets_limits() {
        val gateway = FakeHostGateway()
        val controller = controller(gateway)
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined)
        try {
            controller.openApp(scope)
            runSuspend { controller.expandTaskList(profile, projects = false, projectId = "project") }
            assertEquals(ThreadListQuery(projectThreadLimits = mapOf("project" to 15)), gateway.listQueries.last())
            runSuspend { controller.expandTaskList(profile, projects = true) }
            assertEquals(ThreadListQuery(15, 5, mapOf("project" to 15)), gateway.listQueries.last())
            runSuspend { controller.expandTaskList(profile, projects = false) }
            assertEquals(ThreadListQuery(15, 15, mapOf("project" to 15)), gateway.listQueries.last())
            controller.openApp(scope)
            assertEquals(ThreadListQuery(), gateway.listQueries.last())
            assertTrue(gateway.readIds.isEmpty(), "Listing titles must never read conversation bodies")
        } finally { scope.cancel() }
    }

    @Test
    fun changing_search_during_list_fetch_discards_the_obsolete_result() = runBlocking {
        val gateway = FakeHostGateway()
        val controller = controller(gateway)
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined)
        try {
            controller.openApp(scope)
            val blocked = CompletableDeferred<GatewayResult<ThreadListPage>>()
            gateway.listBlock = { query ->
                if (query.searchTerm == "first") blocked.await()
                else GatewayResult.Success(ThreadListPage(listOf(summary("latest-match", "/workspace"))))
            }
            val first = scope.launch { controller.searchTaskList(profile, "first") }
            controller.searchTaskList(profile, "latest")
            blocked.complete(GatewayResult.Success(ThreadListPage(listOf(summary("obsolete-match", "/workspace")))))
            first.join()
            assertEquals(listOf("latest-match"), controller.state.cache.profile(profile.id).threadList.map { it.id })
            assertEquals("latest", gateway.listQueries.last().searchTerm)
            assertFalse(controller.state.selectedView.loadingMoreThreads)
            val calls = gateway.listQueries.size
            controller.searchTaskList(profile, "latest")
            assertEquals(calls, gateway.listQueries.size)
        } finally { scope.cancel() }
    }

    @Test
    fun project_change_refreshes_membership_and_titles_together() {
        val gateway = FakeHostGateway()
        val controller = controller(gateway)
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined)
        try {
            controller.openApp(scope)
            val project = CodexProject("new-project", "New project", listOf(WorkingDirectory("/new")), 0, 1, 1)
            gateway.projectResult = GatewayResult.Success(listOf(project))
            gateway.listResult = GatewayResult.Success(listOf(summary("new-title", "/new").copy(projectId = project.id)))
            val calls = gateway.listQueries.size
            gateway.emit(notification("project/changed", "{}"))
            assertEquals(calls + 1, gateway.listQueries.size)
            assertEquals(listOf(project.id), controller.state.cache.profile(profile.id).projects.map { it.id })
            assertEquals(listOf("new-title"), controller.state.cache.profile(profile.id).threadList.map { it.id })
        } finally { scope.cancel() }
    }

    @Test
    fun app_launch_opens_the_list_before_connection_completes() = runBlocking {
        val connected = CompletableDeferred<GatewayResult<Unit>>()
        val gateway = FakeHostGateway().apply { connectBlock = { connected.await() } }
        val controller = controller(gateway, selectedThreadId = "thread-1")
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined)
        try {
            controller.openApp(scope)
            assertNull(controller.state.selectedView.selectedThreadId)
            assertIs<ConnectionPhase.Connecting>(controller.state.selectedView.connection)
            connected.complete(GatewayResult.Success(Unit))
            assertNull(controller.state.selectedView.selectedThreadId)
            assertIs<LoadPhase.Ready>(controller.state.selectedView.threadList)
            assertTrue(gateway.readIds.isEmpty())
            assertEquals(1, gateway.connectedProfiles.size)
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun reopening_app_returns_to_fresh_list_without_replacing_connection() {
        val gateway = FakeHostGateway().apply {
            readResult = GatewayResult.Success(ThreadReadResult(thread, emptyList()))
        }
        val controller = controller(gateway, selectedThreadId = "thread-1")
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined)
        val restoration = controller.maintainConnection(scope)
        try {
            gateway.listResult = GatewayResult.Success(listOf(thread.summary, summary("new-thread", "/workspace")))
            controller.openApp(scope)
            assertNull(controller.state.selectedView.selectedThreadId)
            assertTrue(controller.state.cache.profile(profile.id).threadList.any { it.id == "new-thread" })
            assertEquals(listOf("thread-1"), gateway.readIds)
            assertEquals(1, gateway.connectedProfiles.size)
            assertEquals(0, gateway.subscriptionCancelCount)
        } finally {
            restoration.cancel()
            scope.cancel()
        }
    }

    @Test
    fun returning_to_list_fetches_conversations_created_by_another_client() {
        val gateway = FakeHostGateway().apply {
            listResult = GatewayResult.Success(listOf(thread.summary))
            readResult = GatewayResult.Success(ThreadReadResult(thread, emptyList()))
        }
        val controller = controller(gateway, selectedThreadId = "thread-1")
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        gateway.listResult = GatewayResult.Success(listOf(thread.summary, summary("new-thread", "/workspace")))
        gateway.listQueries.clear()
        val previousReads = gateway.readIds.toList()

        runSuspend { controller.showThreadList(profile) }

        assertNull(controller.state.selectedView.selectedThreadId)
        assertTrue(controller.state.cache.profile(profile.id).threadList.any { it.id == "new-thread" })
        assertEquals(listOf(ThreadListQuery()), gateway.listQueries)
        assertEquals(previousReads, gateway.readIds)
        assertEquals(1, gateway.connectedProfiles.size)
    }

    @Test
    fun connect_restores_the_persisted_task_after_loading_lists() {
        val gateway = FakeHostGateway().apply { readResult = GatewayResult.Success(ThreadReadResult(thread, emptyList())) }
        val controller = controller(gateway, selectedThreadId = "thread-1")

        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }

        assertEquals(listOf(ThreadListQuery()), gateway.listQueries)
        assertEquals(listOf("thread-1"), gateway.readIds)
        assertIs<LoadPhase.Ready>(controller.state.selectedView.threadList)
        assertEquals("thread-1", controller.state.selectedView.selectedThreadId)
        assertIs<LoadPhase.Ready>(controller.state.selectedView.threadDetail)
    }

    @Test
    fun foreground_connect_reuses_a_healthy_transport() {
        val gateway = FakeHostGateway()
        val controller = controller(gateway)
        runSuspend {
            controller.connect(profile, CoroutineScope(Dispatchers.Unconfined))
            controller.connect(profile, CoroutineScope(Dispatchers.Unconfined))
        }
        assertEquals(1, gateway.connectedProfiles.size)
    }

    @Test
    fun foreground_restoration_refreshes_latest_messages_and_tasks_without_replacing_transport() {
        val initial = thread.copy(turns = emptyList())
        val latest = thread.copy(turns = listOf(CodexTurn(
            id = "latest-turn", status = TurnStatus.Completed,
            items = listOf(CodexItem.AgentMessage("latest-answer", "Reply written while backgrounded")),
        )))
        val gateway = FakeHostGateway().apply {
            readResult = GatewayResult.Success(ThreadReadResult(initial, emptyList()))
            listResult = GatewayResult.Success(listOf(initial.summary))
        }
        val controller = controller(gateway, selectedThreadId = "thread-1")
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined)
        val restoration = controller.maintainConnection(scope)
        try {
            val newTask = snapshot("new-task").summary
            gateway.readResult = GatewayResult.Success(ThreadReadResult(latest, emptyList()))
            gateway.listResult = GatewayResult.Success(listOf(latest.summary, newTask))
            controller.restoreConnection(scope)
            assertEquals(latest.turns, controller.state.cache.snapshot(profile.id, "thread-1")?.turns)
            assertTrue(controller.state.cache.profile(profile.id).threadList.any { it.id == "new-task" })
            assertEquals("thread-1", controller.state.selectedView.selectedThreadId)
            assertEquals(1, gateway.connectedProfiles.size)
            assertEquals(0, gateway.subscriptionCancelCount)
        } finally {
            restoration.cancel()
            scope.cancel()
        }
    }

    @Test
    fun restoration_connects_on_start_and_recovers_after_transport_loss_and_a_failed_retry() = runBlocking {
        val gateway = FakeHostGateway().apply { readResult = GatewayResult.Success(ThreadReadResult(thread, emptyList())) }
        val controller = controller(gateway, selectedThreadId = "thread-1")
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined)
        val secondRead = CompletableDeferred<Unit>()
        gateway.readHook = { if (gateway.readIds.size == 2) secondRead.complete(Unit) }
        val restoration = controller.maintainConnection(scope)
        try {
            assertIs<ConnectionPhase.Connected>(controller.state.selectedView.connection)
            gateway.connectResult = GatewayResult.Failure("offline")
            gateway.closeStream("connection lost")
            assertIs<ConnectionPhase.Failed>(controller.state.selectedView.connection)
            assertEquals("thread-1", controller.state.selectedView.selectedThreadId)
            gateway.connectResult = GatewayResult.Success(Unit)
            withTimeout(5_000) { secondRead.await() }
            assertIs<ConnectionPhase.Connected>(controller.state.selectedView.connection)
            assertEquals(3, gateway.connectedProfiles.size)
            assertEquals(listOf("thread-1", "thread-1"), gateway.readIds)
            assertEquals(emptyList(), gateway.turnTexts)
            restoration.cancel()
            gateway.closeStream("closed after stopping restoration")
            assertEquals(3, gateway.connectedProfiles.size)
        } finally {
            restoration.cancel()
            scope.cancel()
        }
    }

    @Test
    fun foreground_restoration_does_not_duplicate_an_in_flight_connection() = runBlocking {
        val connected = CompletableDeferred<GatewayResult<Unit>>()
        val gateway = FakeHostGateway().apply { connectBlock = { connected.await() } }
        val controller = controller(gateway)
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined)
        val restoration = controller.maintainConnection(scope)
        try {
            controller.restoreConnection(scope)
            controller.restoreConnection(scope)
            assertEquals(1, gateway.connectedProfiles.size)
            connected.complete(GatewayResult.Success(Unit))
            assertIs<ConnectionPhase.Connected>(controller.state.selectedView.connection)
            controller.restoreConnection(scope)
            assertEquals(1, gateway.connectedProfiles.size)
        } finally {
            restoration.cancel()
            scope.cancel()
        }
    }

    @Test
    fun changing_hosts_during_restoration_cancels_the_old_attempt_without_duplicate_connections() {
        val gateway = FakeHostGateway().apply {
            connectBlock = {
                if (connectedProfiles.size == 1) awaitCancellation()
                GatewayResult.Success(Unit)
            }
        }
        val controller = controller(gateway)
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined)
        val restoration = controller.maintainConnection(scope)
        try {
            val other = profile.copy(hostIdentity = "other-host")
            controller.dispatch(AppAction.ProfilePaired(other))
            assertEquals(listOf(profile.id, other.id), gateway.connectedProfiles.map { it.id })
            assertEquals(other.id, controller.state.selectedProfileId)
            assertIs<ConnectionPhase.Connected>(controller.state.selectedView.connection)
            assertIs<ConnectionPhase.Disconnected>(controller.state.profileViews.getValue(profile.id).connection)
        } finally {
            restoration.cancel()
            scope.cancel()
        }
    }

    @Test
    fun connect_rediscovers_and_persists_the_current_host_endpoint_before_dialing() {
        val currentRelayUrl = "wss://relay.example.test/socket/rotated"
        val gateway = FakeHostGateway().apply {
            discoveryResult = GatewayResult.Success(listOf(currentRelayUrl))
        }
        val controller = controller(gateway)

        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }

        assertEquals(currentRelayUrl, gateway.connectedProfiles.single().relayUrl)
        assertEquals(currentRelayUrl, controller.state.selectedProfile?.relayUrl)
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

        val result = requireNotNull(controller.state.cache.snapshot(profile.id, "thread-1"))
        val resultTurn = result.turns.single()
        assertEquals(TurnStatus.Completed, resultTurn.status)
        assertEquals("old new", (resultTurn.items.single() as CodexItem.AgentMessage).text)
        assertEquals(3, controller.state.cache.profile(profile.id).rawMessages.size)
        val rawIndex = transitions.indexOfFirst {
            it.cache.profile(profile.id).rawMessages.isNotEmpty()
        }
        val snapshotIndex = transitions.indexOfFirst {
            it.cache.snapshot(profile.id, "thread-1")?.turns?.single()?.items?.single() ==
                CodexItem.AgentMessage("item-1", "old new")
        }
        assertTrue(rawIndex >= 0 && snapshotIndex > rawIndex)
    }

    @Test
    fun external_history_changes_refresh_the_open_body_without_loading_or_sending() = runBlocking {
        val initial = thread.copy(
            summary = thread.summary.copy(status = ThreadStatus.NotLoaded),
            turns = thread.turns.map { it.copy(status = TurnStatus.Completed) },
            raw = Json.parseToJsonElement("""{"path":"/fixture/rollout.jsonl"}""") as JsonObject,
        )
        val gateway = FakeHostGateway().apply { readResult = GatewayResult.Success(ThreadReadResult(initial, emptyList())) }
        val controller = controller(gateway, selectedThreadId = "thread-1", cachedThread = initial)
        val scope = CoroutineScope(coroutineContext + SupervisorJob())
        val registered = CompletableDeferred<JsonElement>()
        val unregistered = CompletableDeferred<Unit>()
        gateway.rawHook = { method, params ->
            if (method == "host/thread/watch") registered.complete(params)
            if (method == "host/thread/unwatch") unregistered.complete(Unit)
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
            val revision = withTimeout(5_000) { registered.await() }.asObjectOrNull()!!.long("watchId")!!
            withTimeout(5_000) { initialRefresh.await() }
            val refreshed = initial.copy(turns = listOf(initial.turns.single().copy(
                items = listOf(CodexItem.AgentMessage("item-1", "Persisted external answer")),
            )))
            gateway.readResult = GatewayResult.Success(ThreadReadResult(refreshed, emptyList()))
            repeat(3) { gateway.emit(notification("host/thread/changed", """
                {"watchId":$revision,"threadId":"thread-1"}
            """)) }
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
            gateway.emit(notification("host/thread/changed", """
                {"watchId":$revision,"threadId":"thread-1"}
            """))
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
        val gateway = FakeHostGateway().apply {
            readResult = GatewayResult.Success(ThreadReadResult(initial, emptyList()))
        }
        val controller = controller(gateway, selectedThreadId = "thread-1", cachedThread = initial)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }

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
    fun opening_a_new_chat_and_leaving_without_sending_creates_nothing() {
        val gateway = FakeHostGateway()
        val controller = controller(gateway, workingDirectory = "")
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        val reads = gateway.readIds.size
        val lists = gateway.listQueries.size
        controller.openNewThread(profile, "")
        assertEquals("", controller.state.selectedView.newThreadCwd)
        assertEquals(null, controller.state.selectedView.selectedThreadId)
        assertEquals(reads, gateway.readIds.size)
        assertEquals(lists, gateway.listQueries.size)
        runSuspend { assertEquals(MessageSendResult(false, null), controller.sendMessage(profile, "  ")) }
        controller.dispatch(AppAction.ThreadListOpened(profile.id))
        assertEquals(null, controller.state.selectedView.newThreadCwd)
        assertEquals(0, gateway.startCalls)
    }

    @Test
    fun first_message_creates_the_thread_and_uses_the_existing_send_path() {
        val started = snapshot("thread-new").copy(turns = emptyList())
        val gateway = FakeHostGateway().apply {
            startResult = GatewayResult.Success(started)
            readResult = GatewayResult.Success(ThreadReadResult(started, emptyList()))
        }
        val controller = controller(gateway)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        controller.openNewThread(profile, "/workspace")
        assertEquals(0, gateway.startCalls)
        runSuspend { assertEquals(MessageSendResult(true, "thread-new"), controller.sendMessage(profile, "Build it")) }
        assertEquals(listOf("/workspace"), gateway.startCwds)
        assertEquals(listOf("Build it"), gateway.turnTexts)
        assertEquals(listOf(false), gateway.turnResumes)
        assertEquals("thread-new", controller.state.selectedView.selectedThreadId)
        assertEquals(listOf("Build it"), controller.state.cache.snapshot(profile.id, "thread-new")!!.conversationSegments().flatMap { it.userMessages }.map { it.text })
        assertEquals(null, controller.state.selectedView.newThreadCwd)
    }

    @Test
    fun failed_first_send_retries_in_the_already_created_thread() {
        val started = snapshot("thread-new").copy(turns = emptyList())
        val gateway = FakeHostGateway().apply {
            startResult = GatewayResult.Success(started)
            turnResult = GatewayResult.Failure("Send failed")
        }
        val controller = controller(gateway)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        controller.openNewThread(profile, "")
        runSuspend { assertEquals(MessageSendResult(false, "thread-new"), controller.sendMessage(profile, "Keep this")) }
        assertEquals("thread-new", controller.state.selectedView.selectedThreadId)
        gateway.turnResult = GatewayResult.Success("turn-new")
        runSuspend { assertEquals(MessageSendResult(true, "thread-new"), controller.sendMessage(profile, "Keep this")) }
        assertEquals(1, gateway.startCalls)
        assertEquals(listOf("Keep this", "Keep this"), gateway.turnTexts)
        assertEquals(listOf(false, false), gateway.turnResumes)
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
    fun successful_first_turn_streams_without_reading_an_unmaterialized_rollout() {
        val started = snapshot("thread-new").copy(turns = emptyList())
        val gateway = FakeHostGateway().apply {
            startResult = GatewayResult.Success(started)
            turnResult = GatewayResult.Success("turn-new")
            readResult = GatewayResult.Failure("rollout is empty")
        }
        val controller = controller(gateway)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        controller.openNewThread(profile, "/workspace")
        runSuspend { assertTrue(controller.sendMessage(profile, "Build it").accepted) }
        assertNull(controller.state.selectedView.notice)
        assertTrue(gateway.readIds.isEmpty())
        gateway.emit(notification("item/started", """
            {"threadId":"thread-new","turnId":"turn-new","item":{"type":"userMessage","id":"user-new","content":[{"type":"text","text":"Build it"}]}}
        """))
        gateway.emit(notification("item/started", """
            {"threadId":"thread-new","turnId":"turn-new","item":{"type":"agentMessage","id":"answer-new","text":""}}
        """))
        gateway.emit(notification("item/agentMessage/delta", """
            {"threadId":"thread-new","turnId":"turn-new","itemId":"answer-new","delta":"Streaming answer"}
        """))
        val items = controller.state.cache.snapshot(profile.id, "thread-new")!!.turns.last().items
        assertEquals(CodexItem.UserMessage("user-new", "Build it"), items.first())
        assertEquals(CodexItem.AgentMessage("answer-new", "Streaming answer"), items.last())
        assertIs<LoadPhase.Ready>(controller.state.selectedView.threadDetail)
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
            controller.dispatch(AppAction.ThreadListOpened(profile.id))
        }

        runSuspend { controller.startTurn(profile, "thread-1", "hello") }

        assertNull(controller.state.selectedView.selectedThreadId)
        assertTrue(gateway.readIds.isEmpty())
    }

    @Test
    fun accepted_steering_input_is_visible_across_reads_until_its_native_echo() {
        val gateway = FakeHostGateway().apply { readResult = GatewayResult.Success(ThreadReadResult(thread, emptyList())) }
        val controller = controller(gateway, selectedThreadId = "thread-1")
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        runSuspend { assertTrue(controller.startTurn(profile, "thread-1", "Change direction")) }
        val clientId = gateway.submittedClientIds.single()
        fun displayed() = controller.state.cache.snapshot(profile.id, "thread-1")!!.conversationSegments().flatMap { it.userMessages }
        assertEquals(listOf("Change direction"), displayed().map { it.text })
        assertEquals(clientId, displayed().single().clientId)
        val restored = assertIs<MobileStateDecodeResult.Success<AppState>>(MobileStateCodec.decode(MobileStateCodec.encode(controller.state))).value
        assertEquals(listOf(clientId), restored.cache.snapshot(profile.id, "thread-1")!!.submittedMessages.map { it.clientId })
        runSuspend { controller.readThread(profile, "thread-1") }
        assertEquals(listOf("Change direction"), displayed().map { it.text }, "a read before persistence cannot erase an accepted input")
        gateway.emit(notification("item/started", """
            {"threadId":"thread-1","turnId":"turn-1","item":{"id":"native-user","clientId":"$clientId","type":"userMessage","content":[{"type":"text","text":"Change direction"}]}}
        """))
        assertTrue(controller.state.cache.snapshot(profile.id, "thread-1")!!.submittedMessages.isEmpty())
        assertEquals(listOf("Change direction"), displayed().map { it.text })
        assertEquals("native-user", displayed().single().id)
    }

    @Test
    fun identical_additional_inputs_have_distinct_ids_and_each_echo_replaces_only_its_submission() {
        val gateway = FakeHostGateway().apply { readResult = GatewayResult.Success(ThreadReadResult(thread, emptyList())) }
        val controller = controller(gateway, selectedThreadId = "thread-1")
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        repeat(2) { runSuspend { assertTrue(controller.startTurn(profile, "thread-1", "Again")) } }
        val ids = gateway.submittedClientIds
        assertEquals(2, ids.toSet().size)
        gateway.emit(notification("item/completed", """
            {"threadId":"thread-1","turnId":"turn-1","item":{"id":"native-first","clientId":"${ids.first()}","type":"userMessage","content":[{"type":"text","text":"Again"}]}}
        """))
        val snapshot = controller.state.cache.snapshot(profile.id, "thread-1")!!
        assertEquals(listOf(ids.last()), snapshot.submittedMessages.map { it.clientId })
        assertEquals(2, snapshot.conversationSegments().sumOf { it.userMessages.size })
    }

    @Test
    fun a_native_echo_before_the_acknowledgement_is_not_added_twice() {
        val gateway = FakeHostGateway().apply { readResult = GatewayResult.Success(ThreadReadResult(thread, emptyList())) }
        val controller = controller(gateway, selectedThreadId = "thread-1")
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        gateway.steerHook = {
            gateway.emit(notification("item/started", """
                {"threadId":"thread-1","turnId":"turn-1","item":{"id":"native-first","clientId":"${gateway.submittedClientIds.last()}","type":"userMessage","content":[{"type":"text","text":"Quick echo"}]}}
            """))
        }
        runSuspend { assertTrue(controller.startTurn(profile, "thread-1", "Quick echo")) }
        val snapshot = controller.state.cache.snapshot(profile.id, "thread-1")!!
        assertTrue(snapshot.submittedMessages.isEmpty())
        assertEquals(listOf("Quick echo"), snapshot.conversationSegments().flatMap { it.userMessages }.map { it.text })
    }

    @Test
    fun active_turn_steers_existing_turn_without_replacing_its_live_snapshot() {
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
        assertTrue(gateway.readIds.isEmpty())
        assertNull(controller.state.selectedView.notice)
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

        runSuspend { assertEquals(false, controller.startTurn(profile, "thread-1", "keep going")) }

        assertEquals("steer failed", controller.state.selectedView.notice)
        assertTrue(controller.state.cache.snapshot(profile.id, "thread-1")!!.submittedMessages.isEmpty())
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

        runSuspend { assertEquals(true, controller.startTurn(profile, "thread-1", "queued hello")) }

        assertEquals(listOf("thread-1"), gateway.queueThreadIds)
        assertEquals(listOf("queued hello"), gateway.queueTexts)
        val queued = controller.state.cache.snapshot(profile.id, "thread-1")!!.submittedMessages.single()
        assertEquals("queued hello", queued.text)
        assertNull(queued.turnId)
        assertEquals(gateway.submittedClientIds.single(), queued.clientId)
        assertTrue(gateway.steerTurnIds.isEmpty())
        assertTrue(gateway.turnTexts.isEmpty())
        assertEquals("現在の処理が完了した後にメッセージを送信します。", controller.state.selectedView.notice)
        assertTrue(controller.state.cache.snapshot(profile.id, "thread-1")?.turns.orEmpty().isEmpty())
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
            AppAction.MessageAccepted(profile.id, "thread-1", SubmittedMessage("client-ack", "Sent input", "turn-ack", null)),
        )

        assertEquals(
            listOf(CodexTurn("turn-ack", TurnStatus.InProgress)),
            controller.state.cache.snapshot(profile.id, "thread-1")?.turns,
        )
    }

    @Test
    fun accepted_image_is_visible_before_echo_and_survives_cache_serialization() {
        val idleThread = thread.copy(turns = emptyList())
        val gateway = FakeHostGateway().apply {
            listResult = GatewayResult.Success(listOf(idleThread.summary))
        }
        val controller = controller(gateway, selectedThreadId = "thread-1", cachedThread = idleThread)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        val sources = listOf("/uploads/photo with spaces.png")
        runSuspend { assertTrue(controller.startTurn(profile, "thread-1", "Look", listOf(CodexAttachment(sources.single(), "photo.png", true)))) }
        val snapshot = controller.state.cache.snapshot(profile.id, "thread-1")!!
        val accepted = snapshot.conversationSegments().single().userMessages.single()
        assertEquals(sources, accepted.imageSources)
        assertEquals("Look", accepted.text)
        val restored = assertIs<MobileStateDecodeResult.Success<MobileCache>>(
            MobileStateCodec.decodeCache(MobileStateCodec.encodeCache(controller.state.cache)),
        ).value
        assertEquals(sources, restored.snapshot(profile.id, "thread-1")!!.conversationSegments().single().userMessages.single().imageSources)
        val echoed = CodexItem.UserMessage("native-image", "Look", accepted.clientId, sources)
        val reconciled = applyLiveEvent(restored, profile.id,
            ThreadEvent.ItemCompleted("thread-1", snapshot.turns.single().id, echoed), MobileCacheLimits())
        assertEquals(listOf(echoed), reconciled.snapshot(profile.id, "thread-1")!!.conversationSegments().single().userMessages)
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
            AppAction.MessageAccepted(profile.id, "thread-1", SubmittedMessage("client-ack", "Sent input", "turn-ack", null)),
        )

        assertEquals(
            listOf(completedTurn),
            controller.state.cache.snapshot(profile.id, "thread-1")?.turns,
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
        val gateway = FakeHostGateway().apply {
            readResult = GatewayResult.Success(ThreadReadResult(thread, emptyList()))
        }
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
        val gateway = FakeHostGateway().apply {
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
        val gateway = FakeHostGateway().apply {
            interruptResult = GatewayResult.Failure("stale interrupt failure")
        }
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
        assertTrue(gateway.listQueries.isEmpty())
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
        assertEquals(emptyList(), controller.state.cache.profile(profile.id).rawMessages)

        controller.disconnect(profile)
        dispatcher.runAll()

        assertEquals(emptyList(), controller.state.cache.profile(profile.id).rawMessages)
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
                profile.id,
                ThreadReadResult(it, emptyList()),
                cacheLimits,
            )
        } ?: MobileCache()
        return MobileController(
            gateway = gateway,
            repository = InMemoryMobileRepository(
                AppState(
                    profiles = listOf(profile),
                    selectedProfileId = profile.id,
                    profileViews = mapOf(
                        profile.id to ProfileViewState(
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
        var turnResult: GatewayResult<String> = GatewayResult.Success("turn-1")
        var steerResult: GatewayResult<Unit> = GatewayResult.Success(Unit)
        var queueResult: GatewayResult<String> = GatewayResult.Success("queue-1")
        var interruptResult: GatewayResult<Unit> = GatewayResult.Success(Unit)
        var disconnectResult: GatewayResult<Unit> = GatewayResult.Success(Unit)
        var disconnectBlock: (suspend () -> GatewayResult<Unit>)? = null
        var rawBlock: (suspend (String, JsonElement) -> GatewayResult<JsonElement>)? = null
        var rawHook: ((String, JsonElement) -> Unit)? = null
        var readHook: (() -> Unit)? = null
        var callback: ((RawCodexMessage) -> Unit)? = null
        var closeCallback: ((String) -> Unit)? = null
        var listQueries = mutableListOf<ThreadListQuery>()
        var readIds = mutableListOf<String>()
        var startCalls = 0
        var startCwds = mutableListOf<String>()
        var turnResumes = mutableListOf<Boolean>()
        var turnCwds = mutableListOf<String>()
        var turnTexts = mutableListOf<String>()
        val submittedClientIds = mutableListOf<String>()
        var steerHook: (() -> Unit)? = null
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

        override suspend fun transfer(profile: HostProfile, params: JsonElement): GatewayResult<JsonElement> = GatewayResult.Failure("Transfer is not exercised by this fixture")

        override suspend fun pair(payload: PairingQrPayload): GatewayResult<HostProfile> = GatewayResult.Failure("unused")
        override suspend fun discover(profile: HostProfile): GatewayResult<List<String>> = when (val result = discoveryResult) {
            is GatewayResult.Success -> GatewayResult.Success(result.value.ifEmpty { listOf(profile.relayUrl) })
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
        var listBlock: (suspend (ThreadListQuery) -> GatewayResult<ThreadListPage>)? = null
        override suspend fun listThreads(profile: HostProfile, query: ThreadListQuery): GatewayResult<ThreadListPage> {
            listQueries += query
            listHook?.invoke()
            listBlock?.let { return it(query) }
            val projects = when (val result = projectResult) {
                is GatewayResult.Success -> result.value
                is GatewayResult.Failure -> return result
            }
            return listResult.mapGateway { ThreadListPage(it, projects) }
        }
        override suspend fun readThread(profile: HostProfile, threadId: String): GatewayResult<ThreadReadResult> {
            readIds += threadId
            readHook?.invoke()
            return readResult
        }
        override suspend fun startThread(profile: HostProfile, cwd: String): GatewayResult<ThreadSnapshot> {
            startCalls += 1
            startCwds += cwd
            return startResult
        }
        override suspend fun startTurn(profile: HostProfile, threadId: String, cwd: String, text: String, attachments: List<CodexAttachment>, resume: Boolean, clientUserMessageId: String): GatewayResult<String> {
            submittedClientIds += clientUserMessageId
            turnResumes += resume
            turnCwds += cwd
            turnTexts += text
            turnHook?.invoke()
            return turnResult
        }
        override suspend fun steerTurn(profile: HostProfile, threadId: String, turnId: String, text: String, attachments: List<CodexAttachment>, clientUserMessageId: String): GatewayResult<Unit> {
            submittedClientIds += clientUserMessageId
            steerHook?.invoke()
            steerTurnIds += turnId
            steerTexts += text
            return steerResult
        }
        override suspend fun queueTurn(profile: HostProfile, threadId: String, text: String, attachments: List<CodexAttachment>, clientUserMessageId: String): GatewayResult<String> {
            submittedClientIds += clientUserMessageId
            queueThreadIds += threadId
            queueTexts += text
            return queueResult
        }
        override suspend fun interrupt(profile: HostProfile, threadId: String, turnId: String): GatewayResult<Unit> {
            interruptHook?.invoke()
            return interruptResult
        }
        override suspend fun rawRequest(profile: HostProfile, method: String, params: JsonElement): GatewayResult<JsonElement> {
            rawBlock?.let { return it(method, params) }
            rawHook?.invoke(method, params)
            return GatewayResult.Success(JsonObject(emptyMap()))
        }
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

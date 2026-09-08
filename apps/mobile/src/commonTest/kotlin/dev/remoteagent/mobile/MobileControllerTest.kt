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
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout

internal class MobileControllerTest : MobileControllerTestFixture() {
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
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun returning_to_list_after_disconnect_clears_detail_and_allows_reopening() = runBlocking {
        val gateway =
            FakeHostGateway().apply { readResult = GatewayResult.Success(ThreadReadResult(thread, emptyList())) }
        val controller = controller(gateway)
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined)
        try {
            repeat(3) {
                controller.connect(profile, scope)
                controller.readThread(profile, "thread-1")
                assertEquals("thread-1", controller.state.selectedView.selectedThreadId)
                controller.disconnect(profile)
                val lists = gateway.listQueries.size

                controller.showThreadList(profile)

                assertNull(controller.state.selectedView.selectedThreadId)
                assertNull(controller.state.selectedView.newThreadCwd)
                assertEquals(lists, gateway.listQueries.size)
                controller.openNewThread(profile, "/workspace")
                assertEquals("/workspace", controller.state.selectedView.newThreadCwd)
                controller.showThreadList(profile)
                assertNull(controller.state.selectedView.newThreadCwd)
            }
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun returning_to_list_preserves_expanded_sections_and_search() {
        val gateway =
            FakeHostGateway().apply { readResult = GatewayResult.Success(ThreadReadResult(thread, emptyList())) }
        val controller = controller(gateway)
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined)
        try {
            controller.openApp(scope)
            runSuspend {
                controller.searchTaskList(profile, "retained")
                controller.expandTaskList(profile, projects = true)
                controller.expandTaskList(profile, projects = false)
                controller.expandTaskList(profile, projects = false, projectId = "project")
                controller.readThread(profile, "thread-1")
                controller.showThreadList(profile)
            }
            assertEquals(ThreadListQuery(15, 15, mapOf("project" to 15), "retained"), gateway.listQueries.last())
            assertNull(controller.state.selectedView.selectedThreadId)
            assertEquals("retained", controller.state.selectedView.threadSearchTerm)
        } finally {
            scope.cancel()
        }
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
        } finally {
            scope.cancel()
        }
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
            gateway.listResult =
                GatewayResult.Success(listOf(summary("new-title", "/new").copy(projectId = project.id)))
            val calls = gateway.listQueries.size
            gateway.emit(notification("project/changed", "{}"))
            assertEquals(calls + 1, gateway.listQueries.size)
            assertEquals(listOf(project.id), controller.state.cache.profile(profile.id).projects.map { it.id })
            assertEquals(listOf("new-title"), controller.state.cache.profile(profile.id).threadList.map { it.id })
        } finally {
            scope.cancel()
        }
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
        val gateway =
            FakeHostGateway().apply { readResult = GatewayResult.Success(ThreadReadResult(thread, emptyList())) }
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
        val gateway =
            FakeHostGateway().apply {
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
        val gateway =
            FakeHostGateway().apply { readResult = GatewayResult.Success(ThreadReadResult(thread, emptyList())) }
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
        val latest =
            thread.copy(
                turns =
                    listOf(
                        CodexTurn(
                            id = "latest-turn",
                            status = TurnStatus.Completed,
                            items = listOf(CodexItem.AgentMessage("latest-answer", "Reply written while backgrounded")),
                        )
                    )
            )
        val gateway =
            FakeHostGateway().apply {
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
        val gateway =
            FakeHostGateway().apply { readResult = GatewayResult.Success(ThreadReadResult(thread, emptyList())) }
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
        val gateway =
            FakeHostGateway().apply {
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
        val gateway = FakeHostGateway().apply { discoveryResult = GatewayResult.Success(listOf(currentRelayUrl)) }
        val controller = controller(gateway)

        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }

        assertEquals(currentRelayUrl, gateway.connectedProfiles.single().relayUrl)
        assertEquals(currentRelayUrl, controller.state.selectedProfile?.relayUrl)
    }

    @Test
    fun connect_uses_saved_addresses_when_rediscovery_is_unavailable() {
        val gateway = FakeHostGateway().apply { discoveryResult = GatewayResult.Failure("mDNS unavailable") }
        val controller = controller(gateway)

        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }

        assertEquals(listOf(profile), gateway.connectedProfiles)
        assertIs<ConnectionPhase.Connected>(controller.state.selectedView.connection)
    }
}

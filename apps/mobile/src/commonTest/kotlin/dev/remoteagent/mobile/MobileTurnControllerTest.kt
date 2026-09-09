package dev.remoteagent.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertIs
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject

internal class MobileTurnControllerTest : MobileControllerTestFixture() {
    @Test
    fun request_validation_failure_stays_in_the_editor_instead_of_leaving_a_turn_notice() {
        val gateway = FakeHostGateway()
        val controller = controller(gateway)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        val request =
            kotlinx.serialization.json.Json.parseToJsonElement(
                """
                {"id":"question-1","method":"item/tool/requestUserInput",
                 "params":{"questions":[{"id":"continue"}]}}
                """
            )
        gateway.agentBlock = { command ->
            if (command is AgentCommand.Respond) GatewayResult.Failure("すべての質問に回答してください") else null
        }
        runSuspend {
            val failed = controller.respond(profile, request, RequestAnswer.Answers(emptyMap()))
            assertEquals("すべての質問に回答してください", assertIs<GatewayResult.Failure>(failed).message)
        }
        assertNull(controller.state.selectedView.notice)
        gateway.agentBlock = { command -> if (command is AgentCommand.Respond) GatewayResult.Success("null") else null }
        runSuspend {
            assertIs<GatewayResult.Success<Unit>>(
                controller.respond(profile, request, RequestAnswer.Answers(mapOf("continue" to "続ける")))
            )
        }
        assertNull(controller.state.selectedView.notice)
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
        val gateway =
            FakeHostGateway().apply {
                startResult = GatewayResult.Success(started)
                readResult = GatewayResult.Success(started)
            }
        val controller = controller(gateway)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        controller.openNewThread(profile, "/workspace")
        assertEquals(0, gateway.startCalls)
        runSuspend { assertEquals(MessageSendResult(true, "thread-new"), controller.sendMessage(profile, "Build it")) }
        assertEquals(listOf("/workspace"), gateway.startCwds)
        assertEquals(listOf("Build it"), gateway.sends.map { it.input.text })
        assertEquals("thread-new", gateway.sends.single().threadId)
        assertEquals("thread-new", controller.state.selectedView.selectedThreadId)
        assertEquals(
            listOf("Build it"),
            controller.state.cache
                .snapshot(profile.id, "thread-new")!!
                .conversationSegments()
                .flatMap { it.userMessages }
                .map { it.text },
        )
        assertEquals(null, controller.state.selectedView.newThreadCwd)
    }

    @Test
    fun failed_first_send_retries_in_the_already_created_thread() {
        val started = snapshot("thread-new").copy(turns = emptyList())
        val gateway =
            FakeHostGateway().apply {
                startResult = GatewayResult.Success(started)
                sendResult = GatewayResult.Failure("Send failed")
            }
        val controller = controller(gateway)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        controller.openNewThread(profile, "")
        runSuspend {
            assertEquals(MessageSendResult(false, "thread-new"), controller.sendMessage(profile, "Keep this"))
        }
        assertEquals("thread-new", controller.state.selectedView.selectedThreadId)
        gateway.sendResult = GatewayResult.Success("turn-new")
        runSuspend { assertEquals(MessageSendResult(true, "thread-new"), controller.sendMessage(profile, "Keep this")) }
        assertEquals(1, gateway.startCalls)
        assertEquals(listOf("Keep this", "Keep this"), gateway.sends.map { it.input.text })
        assertEquals(listOf("thread-new", "thread-new"), gateway.sends.map { it.threadId })
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
        val gateway =
            FakeHostGateway().apply {
                listResult = GatewayResult.Success(listOf(summary("thread-1", "/cached/workspace")))
            }
        val controller = controller(gateway, selectedThreadId = null, cachedThread = thread.copy(turns = emptyList()))
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }

        runSuspend { controller.startTurn(profile, "thread-1", "hello") }

        assertEquals("/workspace", gateway.sends.single().snapshot.asObjectOrNull()?.string("cwd"))
        assertEquals("/cached/workspace", gateway.sends.single().listed.asObjectOrNull()?.string("cwd"))
        assertEquals(listOf("hello"), gateway.sends.map { it.input.text })
    }

    @Test
    fun successful_first_turn_streams_without_reading_an_unmaterialized_rollout() {
        val started = snapshot("thread-new").copy(turns = emptyList())
        val gateway =
            FakeHostGateway().apply {
                startResult = GatewayResult.Success(started)
                sendResult = GatewayResult.Success("turn-new")
                readResult = GatewayResult.Failure("rollout is empty")
            }
        val controller = controller(gateway)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        controller.openNewThread(profile, "/workspace")
        runSuspend { assertTrue(controller.sendMessage(profile, "Build it").accepted) }
        assertNull(controller.state.selectedView.notice)
        assertTrue(gateway.readIds.isEmpty())
        gateway.emit(
            notification(
                "item/started",
                """
            {"threadId":"thread-new","turnId":"turn-new","item":{"type":"userMessage","id":"user-new","content":[{"type":"text","text":"Build it"}]}}
        """,
            )
        )
        gateway.emit(
            notification(
                "item/started",
                """
            {"threadId":"thread-new","turnId":"turn-new","item":{"type":"agentMessage","id":"answer-new","text":""}}
        """,
            )
        )
        gateway.emit(
            notification(
                "item/agentMessage/delta",
                """
            {"threadId":"thread-new","turnId":"turn-new","itemId":"answer-new","delta":"Streaming answer"}
        """,
            )
        )
        val items = controller.state.cache.snapshot(profile.id, "thread-new")!!.turns.last().items
        assertEquals(CodexItem.UserMessage("user-new", "Build it"), items.first())
        assertEquals(CodexItem.AgentMessage("answer-new", "Streaming answer"), items.last())
        assertIs<LoadPhase.Ready>(controller.state.selectedView.threadDetail)
    }

    @Test
    fun successful_turn_start_does_not_reopen_detail_after_the_user_returns_to_the_list() {
        val idleThread = thread.copy(turns = emptyList())
        val gateway =
            FakeHostGateway().apply {
                sendResult = GatewayResult.Success("turn-2")
                readResult = GatewayResult.Success(idleThread)
            }
        val controller = controller(gateway, selectedThreadId = "thread-1", cachedThread = idleThread)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        runSuspend { controller.readThread(profile, "thread-1") }
        gateway.readIds.clear()
        gateway.sendHook = { controller.dispatch(AppAction.ThreadListOpened(profile.id)) }

        runSuspend { controller.startTurn(profile, "thread-1", "hello") }

        assertNull(controller.state.selectedView.selectedThreadId)
        assertTrue(gateway.readIds.isEmpty())
    }

    @Test
    fun accepted_steering_input_is_visible_across_reads_until_its_native_echo() {
        val gateway = FakeHostGateway().apply { readResult = GatewayResult.Success(thread) }
        val controller = controller(gateway, selectedThreadId = "thread-1")
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        runSuspend { assertTrue(controller.startTurn(profile, "thread-1", "Change direction")) }
        val clientId = gateway.sends.single().input.clientUserMessageId
        fun displayed() =
            controller.state.cache.snapshot(profile.id, "thread-1")!!.conversationSegments().flatMap { it.userMessages }
        assertEquals(listOf("Change direction"), displayed().map { it.text })
        assertEquals(clientId, displayed().single().clientId)
        val restored =
            assertIs<MobileStateDecodeResult.Success<AppState>>(
                    MobileStateCodec.decode(MobileStateCodec.encode(controller.state))
                )
                .value
        assertEquals(
            listOf(clientId),
            restored.cache.snapshot(profile.id, "thread-1")!!.submittedMessages.map { it.clientId },
        )
        runSuspend { controller.readThread(profile, "thread-1") }
        assertEquals(
            listOf("Change direction"),
            displayed().map { it.text },
            "a read before persistence cannot erase an accepted input",
        )
        gateway.emit(
            notification(
                "item/started",
                """
            {"threadId":"thread-1","turnId":"turn-1","item":{"id":"native-user","clientId":"$clientId","type":"userMessage","content":[{"type":"text","text":"Change direction"}]}}
        """,
            )
        )
        assertTrue(controller.state.cache.snapshot(profile.id, "thread-1")!!.submittedMessages.isEmpty())
        assertEquals(listOf("Change direction"), displayed().map { it.text })
        assertEquals("native-user", displayed().single().id)
    }

    @Test
    fun identical_additional_inputs_have_distinct_ids_and_each_echo_replaces_only_its_submission() {
        val gateway = FakeHostGateway().apply { readResult = GatewayResult.Success(thread) }
        val controller = controller(gateway, selectedThreadId = "thread-1")
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        repeat(2) { runSuspend { assertTrue(controller.startTurn(profile, "thread-1", "Again")) } }
        val ids = gateway.sends.map { it.input.clientUserMessageId }
        assertEquals(2, ids.toSet().size)
        gateway.emit(
            notification(
                "item/completed",
                """
            {"threadId":"thread-1","turnId":"turn-1","item":{"id":"native-first","clientId":"${ids.first()}","type":"userMessage","content":[{"type":"text","text":"Again"}]}}
        """,
            )
        )
        val snapshot = controller.state.cache.snapshot(profile.id, "thread-1")!!
        assertEquals(listOf(ids.last()), snapshot.submittedMessages.map { it.clientId })
        assertEquals(2, snapshot.conversationSegments().sumOf { it.userMessages.size })
    }

    @Test
    fun a_native_echo_before_the_acknowledgement_is_not_added_twice() {
        val gateway = FakeHostGateway().apply { readResult = GatewayResult.Success(thread) }
        val controller = controller(gateway, selectedThreadId = "thread-1")
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        gateway.sendHook = {
            gateway.emit(
                notification(
                    "item/started",
                    """
                {"threadId":"thread-1","turnId":"turn-1","item":{"id":"native-first","clientId":"${gateway.sends.last().input.clientUserMessageId}","type":"userMessage","content":[{"type":"text","text":"Quick echo"}]}}
            """,
                )
            )
        }
        runSuspend { assertTrue(controller.startTurn(profile, "thread-1", "Quick echo")) }
        val snapshot = controller.state.cache.snapshot(profile.id, "thread-1")!!
        assertTrue(snapshot.submittedMessages.isEmpty())
        assertEquals(listOf("Quick echo"), snapshot.conversationSegments().flatMap { it.userMessages }.map { it.text })
    }

    @Test
    fun accepted_input_retains_the_live_turn_without_rereading() {
        val gateway = FakeHostGateway().apply { readResult = GatewayResult.Success(thread) }
        val controller = controller(gateway, selectedThreadId = "thread-1")
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        runSuspend { controller.readThread(profile, "thread-1") }
        gateway.readIds.clear()
        val before = controller.state.cache.snapshot(profile.id, "thread-1")!!.turns

        runSuspend { controller.startTurn(profile, "thread-1", "keep going") }

        assertEquals(before, controller.state.cache.snapshot(profile.id, "thread-1")!!.turns)
        assertEquals("keep going", gateway.sends.single().input.text)
        assertTrue(gateway.readIds.isEmpty())
        assertNull(controller.state.selectedView.notice)
    }

    @Test
    fun failed_send_keeps_the_snapshot_and_surfaces_the_error_without_accepting_input() {
        val idle = thread.copy(turns = emptyList())
        val queued = idle.copy(summary = idle.summary.copy(status = ThreadStatus.Active(emptyList())))
        for (snapshot in listOf(idle, thread, queued)) {
            val gateway = FakeHostGateway().apply { sendResult = GatewayResult.Failure("send failed") }
            val controller = controller(gateway, selectedThreadId = "thread-1", cachedThread = snapshot)
            runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
            gateway.readIds.clear()
            runSuspend { assertEquals(false, controller.startTurn(profile, "thread-1", "keep going")) }
            assertEquals("send failed", controller.state.selectedView.notice)
            assertEquals(snapshot, controller.state.cache.snapshot(profile.id, "thread-1"))
            assertEquals("keep going", gateway.sends.single().input.text)
            assertTrue(gateway.readIds.isEmpty())
        }
    }

    @Test
    fun active_thread_without_a_visible_turn_queues_message_and_shows_delivery_notice() {
        val activeThread =
            thread.copy(summary = thread.summary.copy(status = ThreadStatus.Active(emptyList())), turns = emptyList())
        val gateway =
            FakeHostGateway().apply {
                listResult = GatewayResult.Success(listOf(activeThread.summary))
                sendResult = GatewayResult.Success(null)
            }
        val controller = controller(gateway, cachedThread = activeThread)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }

        runSuspend { assertEquals(true, controller.startTurn(profile, "thread-1", "queued hello")) }

        assertEquals("thread-1", gateway.sends.single().threadId)
        assertEquals("queued hello", gateway.sends.single().input.text)
        val queued = controller.state.cache.snapshot(profile.id, "thread-1")!!.submittedMessages.single()
        assertEquals("queued hello", queued.text)
        assertNull(queued.turnId)
        assertEquals(gateway.sends.single().input.clientUserMessageId, queued.clientId)
        assertEquals("現在の処理が完了した後にメッセージを送信します。", controller.state.selectedView.notice)
        assertTrue(controller.state.cache.snapshot(profile.id, "thread-1")?.turns.orEmpty().isEmpty())
    }

    @Test
    fun send_passes_the_new_list_status_and_cached_detail_to_the_shared_operation() {
        val idleThread = thread.copy(summary = thread.summary.copy(status = ThreadStatus.Idle), turns = emptyList())
        val activeSummary = idleThread.summary.copy(status = ThreadStatus.Active(emptyList()))
        val gateway =
            FakeHostGateway().apply {
                listResult = GatewayResult.Success(listOf(activeSummary))
                sendResult = GatewayResult.Success(null)
            }
        val controller = controller(gateway, cachedThread = idleThread)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }

        runSuspend { controller.startTurn(profile, "thread-1", "queued hello") }

        val sent = gateway.sends.single()
        assertEquals("idle", sent.snapshot.asObjectOrNull()?.childObject("status")?.string("type"))
        assertEquals("active", sent.listed.asObjectOrNull()?.childObject("status")?.string("type"))
    }

    @Test
    fun turn_start_acknowledgement_upserts_an_in_progress_placeholder_when_started_notification_is_missing() {
        val idleThread = thread.copy(turns = emptyList())
        val gateway = FakeHostGateway().apply { listResult = GatewayResult.Success(listOf(idleThread.summary)) }
        val controller = controller(gateway, selectedThreadId = "thread-1", cachedThread = idleThread)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }

        controller.dispatch(
            AppAction.MessageAccepted(
                profile.id,
                "thread-1",
                SubmittedMessage("client-ack", "Sent input", "turn-ack", null),
            )
        )

        assertEquals(
            listOf(CodexTurn("turn-ack", TurnStatus.InProgress)),
            controller.state.cache.snapshot(profile.id, "thread-1")?.turns,
        )
    }

    @Test
    fun accepted_image_is_visible_before_echo_and_survives_cache_serialization() {
        val idleThread = thread.copy(turns = emptyList())
        val gateway = FakeHostGateway().apply { listResult = GatewayResult.Success(listOf(idleThread.summary)) }
        val controller = controller(gateway, selectedThreadId = "thread-1", cachedThread = idleThread)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        val sources = listOf("/uploads/photo with spaces.png")
        runSuspend {
            assertTrue(
                controller.startTurn(
                    profile,
                    "thread-1",
                    "Look",
                    listOf(CodexAttachment(sources.single(), "photo.png", true)),
                )
            )
        }
        val snapshot = controller.state.cache.snapshot(profile.id, "thread-1")!!
        val accepted = snapshot.conversationSegments().single().userMessages.single()
        assertEquals(sources, accepted.imageSources)
        assertEquals("Look", accepted.text)
        val restored =
            assertIs<MobileStateDecodeResult.Success<AppState>>(
                    MobileStateCodec.decode(MobileStateCodec.encode(controller.state))
                )
                .value
                .cache
        assertEquals(
            sources,
            restored
                .snapshot(profile.id, "thread-1")!!
                .conversationSegments()
                .single()
                .userMessages
                .single()
                .imageSources,
        )
        val echoed = CodexItem.UserMessage("native-image", "Look", accepted.clientId, sources)
        val reconciled =
            applyLiveEvent(
                restored,
                profile.id,
                codexMessage(
                    method = "item/completed",
                    params =
                        buildJsonObject {
                            put("threadId", JsonPrimitive("thread-1"))
                            put("turnId", JsonPrimitive(snapshot.turns.single().id))
                            put("item", (echoed).fixtureJson())
                        },
                ),
                MobileCacheLimits(),
            )
        assertEquals(
            listOf(echoed),
            reconciled.snapshot(profile.id, "thread-1")!!.conversationSegments().single().userMessages,
        )
    }

    @Test
    fun late_turn_start_acknowledgement_preserves_a_terminal_status() {
        val completedTurn = CodexTurn("turn-ack", TurnStatus.Completed)
        val completedThread = thread.copy(turns = listOf(completedTurn))
        val gateway = FakeHostGateway().apply { listResult = GatewayResult.Success(listOf(completedThread.summary)) }
        val controller = controller(gateway, selectedThreadId = "thread-1", cachedThread = completedThread)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }

        controller.dispatch(
            AppAction.MessageAccepted(
                profile.id,
                "thread-1",
                SubmittedMessage("client-ack", "Sent input", "turn-ack", null),
            )
        )

        assertEquals(listOf(completedTurn), controller.state.cache.snapshot(profile.id, "thread-1")?.turns)
    }

    @Test
    fun send_without_a_cached_snapshot_passes_the_listed_thread_to_the_shared_operation() {
        val gateway =
            FakeHostGateway().apply {
                listResult = GatewayResult.Success(listOf(summary("thread-1", "")))
                sendResult = GatewayResult.Failure("タスクの作業ディレクトリが不明です。タスク一覧を更新してください")
            }
        val controller = controller(gateway, cachedThread = null)
        runSuspend { controller.connect(profile, CoroutineScope(Dispatchers.Unconfined)) }
        runSuspend { assertEquals(false, controller.startTurn(profile, "thread-1", "hello")) }
        assertEquals(kotlinx.serialization.json.JsonNull, gateway.sends.single().snapshot)
        assertEquals("", gateway.sends.single().listed.asObjectOrNull()?.string("cwd"))
        assertEquals("タスクの作業ディレクトリが不明です。タスク一覧を更新してください", controller.state.selectedView.notice)
    }
}

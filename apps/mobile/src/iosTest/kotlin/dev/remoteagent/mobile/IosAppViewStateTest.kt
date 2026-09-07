package dev.remoteagent.mobile

import kotlin.test.Test
import kotlin.test.assertNotSame
import kotlin.test.assertSame
import kotlin.test.assertNull
import kotlin.test.assertEquals

class IosAppViewStateTest {
    @Test
    fun streaming_body_does_not_invalidate_navigation_or_title_lists() {
        val projector = IosViewStateProjector()
        val turn = CodexTurn("live", TurnStatus.InProgress, listOf(CodexItem.AgentMessage("answer", "before")))
        val initial = state(listOf(turn))
        val navigation = projector.project(initial)
        val original = projector.thread!!
        val profile = initial.cache.profile("host")
        val snapshot = profile.snapshots.getValue("conversation")
        val updated = initial.copy(cache = MobileCache(mapOf("host" to profile.copy(snapshots = mapOf(
            "conversation" to snapshot.copy(turns = listOf(turn.copy(items = listOf(CodexItem.AgentMessage("answer", "after!"))))),
        )))))
        assertSame(navigation, projector.project(updated))
        assertNotSame(original, projector.thread)
        assertEquals("after!", projector.thread!!.turns.single().responses.single().expandedBody())
        assertNotSame(navigation, projector.project(updated.copy(pairingError = "Pairing failed")))
        assertEquals("conversation", projector.project(updated).selectedThreadId)
    }

    @Test
    fun open_conversation_keeps_its_upload_directory_when_recent_titles_drop_it() {
        val host = HostProfile("runner", "Mac", "wss://relay.example.test", "host", "device-ref")
        val summary = ThreadSummary(
            id = "conversation", preview = "", workingDirectory = WorkingDirectory("/workspace/chat"),
            createdAtMs = 1, updatedAtMs = 1, status = ThreadStatus.Idle,
        )
        val state = AppState(
            profiles = listOf(host), selectedProfileId = host.id,
            profileViews = mapOf(host.id to ProfileViewState(
                connection = ConnectionPhase.Connected, selectedThreadId = summary.id,
            )),
            cache = MobileCache(mapOf(host.id to ProfileMobileCache(
                snapshots = mapOf(summary.id to ThreadSnapshot(summary = summary)),
            ))),
        )
        val projector = IosViewStateProjector()
        assertEquals("/workspace/chat", projector.project(state).workingDirectory)
        val newChat = state.copy(profileViews = mapOf(host.id to state.selectedView.copy(
            selectedThreadId = null, newThreadCwd = "/workspace/other",
        )))
        assertEquals("/workspace/other", projector.project(newChat).workingDirectory)
    }

    @Test
    fun streaming_reuses_unchanged_history_and_updates_equal_length_replacements() {
        val projector = IosViewStateProjector()
        val history = CodexTurn("old", TurnStatus.Completed, listOf(CodexItem.AgentMessage("old-answer", "history")))
        val live = CodexTurn("live", TurnStatus.InProgress, listOf(CodexItem.AgentMessage("answer", "before")))
        val first = projector.conversation(state(listOf(history, live)))!!
        val changed = live.copy(items = listOf(CodexItem.AgentMessage("answer", "after!")))
        val next = projector.conversation(state(listOf(history, changed)))!!
        assertSame(first.turns.first(), next.turns.first())
        assertEquals("after!", next.turns.last().responses.single().expandedBody())
        assertEquals("before", first.turns.last().responses.single().expandedBody())
        val completed = projector.conversation(state(listOf(history, changed.copy(status = TurnStatus.Completed))))!!
        assertEquals(false, completed.turns.last().isInProgress)
    }

    @Test
    fun initial_projection_keeps_every_turn_when_ids_repeat() {
        val projector = IosViewStateProjector()
        val first = CodexTurn("", TurnStatus.Completed, listOf(CodexItem.AgentMessage("answer-1", "first")))
        val second = CodexTurn("", TurnStatus.Completed, listOf(CodexItem.AgentMessage("answer-2", "second")))

        val view = projector.conversation(state(listOf(first, second)))!!

        assertEquals(listOf("first", "second"), view.turns.flatMap { turn ->
            turn.responses.map { it.expandedBody() }
        })
    }

    @Test
    fun accepted_inputs_invalidate_their_turn_until_the_native_echo_arrives() {
        val projector = IosViewStateProjector()
        val turn = CodexTurn("live", TurnStatus.InProgress, listOf(CodexItem.AgentMessage("answer", "reply")))
        projector.project(state(listOf(turn)))
        val pending = SubmittedMessage("client", "additional", "live", "answer")
        val accepted = projector.conversation(state(listOf(turn), listOf(pending)))!!
        assertEquals(listOf("additional"), accepted.turns.flatMap { it.userMessages }.map { it.expandedBody() })
        val echoed = turn.copy(items = turn.items + CodexItem.UserMessage("native", "additional", "client"))
        val reconciled = projector.conversation(state(listOf(echoed)))!!
        assertEquals(listOf("client"), reconciled.turns.flatMap { it.userMessages }.map { it.id })
        val queued = projector.conversation(state(listOf(echoed), listOf(pending.copy(clientId = "queue", turnId = null))))!!
        assertEquals(listOf("queue"), queued.queuedMessages.map { it.id })
    }

    @Test
    fun navigation_and_history_eviction_release_previous_projections() {
        val projector = IosViewStateProjector()
        val turn = CodexTurn("old", TurnStatus.Completed, listOf(CodexItem.AgentMessage("answer", "history")))
        val initial = state(listOf(turn))
        val first = projector.conversation(initial)!!
        assertSame(first, projector.conversation(initial.copy(pairingError = "notice")))
        projector.project(state(emptyList()))
        val restored = projector.conversation(initial)!!
        assertNotSame(first.turns.single(), restored.turns.single())
        val otherHost = projector.conversation(state(listOf(turn), hostId = "other-host"))!!
        assertNotSame(restored.turns.single(), otherHost.turns.single())
        val list = initial.copy(profileViews = mapOf("host" to ProfileViewState()))
        assertNull(projector.conversation(list))
        assertNotSame(first.turns.single(), projector.conversation(initial)!!.turns.single())
    }

    private fun IosViewStateProjector.conversation(state: AppState): IosThreadView? {
        project(state)
        return thread
    }

    private fun state(
        turns: List<CodexTurn>,
        submissions: List<SubmittedMessage> = emptyList(),
        hostId: String = "host",
    ): AppState {
        val host = HostProfile("runner", "Mac", "wss://relay.example.test", hostId, "device-ref")
        val summary = ThreadSummary(id = "conversation", preview = "", workingDirectory = WorkingDirectory("/fixture"),
            createdAtMs = 1, updatedAtMs = 1, status = ThreadStatus.Idle)
        return AppState(profiles = listOf(host), selectedProfileId = host.id,
            profileViews = mapOf(host.id to ProfileViewState(selectedThreadId = summary.id)),
            cache = MobileCache(mapOf(host.id to ProfileMobileCache(snapshots = mapOf(
                summary.id to ThreadSnapshot(summary, turns, submittedMessages = submissions),
            )))))
    }
}

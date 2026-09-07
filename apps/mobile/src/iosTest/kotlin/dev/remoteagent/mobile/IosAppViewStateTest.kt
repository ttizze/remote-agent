package dev.remoteagent.mobile

import kotlin.test.Test
import kotlin.test.assertEquals

class IosAppViewStateTest {
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
        assertEquals("/workspace/chat", state.toIosViewState().workingDirectory)
        val newChat = state.copy(profileViews = mapOf(host.id to state.selectedView.copy(
            selectedThreadId = null, newThreadCwd = "/workspace/other",
        )))
        assertEquals("/workspace/other", newChat.toIosViewState().workingDirectory)
    }
}

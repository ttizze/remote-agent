package dev.remoteagent.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertIs
import kotlin.test.assertNull

class AppStateTest {
    private val mac = HostProfile("runner-mac", "Mac", "wss://relay.example.test/mac", "runner-mac", "device-key-ref")
    private val linux = HostProfile("runner-linux", "Linux", "wss://relay.example.test/linux", "runner-linux", "device-key-ref")

    @Test
    fun pairing_keys_profiles_by_host_identity_and_selects_the_new_profile() {
        val first = reduce(AppState(), AppAction.ProfilePaired(mac))
        val replaced = reduce(first, AppAction.ProfilePaired(mac.copy(name = "MacBook")))

        assertEquals(listOf(mac.copy(name = "MacBook")), replaced.profiles)
        assertEquals(mac.id, replaced.selectedProfileId)
        assertIs<ConnectionPhase.Disconnected>(replaced.connection)
    }

    @Test
    fun profile_selection_preserves_each_hosts_working_directory_and_cache() {
        val paired = reduce(reduce(AppState(), AppAction.ProfilePaired(mac)), AppAction.ProfilePaired(linux))
        val macSelected = reduce(paired, AppAction.ProfileSelected(mac.id))
        val macConnected = reduce(macSelected, AppAction.ConnectSucceeded(mac.id))
        val macCwd = reduce(macConnected, AppAction.WorkingDirectoryChanged(mac.id, "/mac/project"))
        val withMacList = reduce(macCwd, AppAction.ThreadListLoaded(mac.id, listOf(summary("mac-thread"))))

        val linuxSelected = reduce(withMacList, AppAction.ProfileSelected(linux.id))
        val linuxConnected = reduce(linuxSelected, AppAction.ConnectSucceeded(linux.id))
        val linuxCwd = reduce(linuxConnected, AppAction.WorkingDirectoryChanged(linux.id, "/linux/project"))

        assertEquals("/linux/project", linuxCwd.workingDirectoryPath)
        assertEquals("/mac/project", linuxCwd.profileViews.getValue(mac.id).workingDirectoryPath)
        assertEquals(listOf("mac-thread"), linuxCwd.cache.profile(mac.id).threadList.map { it.id })
        assertEquals(emptyList(), linuxCwd.cache.profile(linux.id).threadList)
    }

    @Test
    fun working_directory_cannot_change_until_the_selected_host_is_connected() {
        val paired = reduce(AppState(), AppAction.ProfilePaired(mac))
        val ignored = reduce(paired, AppAction.WorkingDirectoryChanged(mac.id, "/any/directory"))

        assertEquals("", ignored.workingDirectoryPath)
    }

    @Test
    fun disconnection_keeps_the_non_authoritative_cache_but_clears_only_live_control_state() {
        val connected = AppState(
            profiles = listOf(mac),
            selectedProfileId = mac.id,
            profileViews = mapOf(mac.id to ProfileViewState(connection = ConnectionPhase.Connected, interruptingTurnId = "turn-1")),
            cache = reconcileThreadList(MobileCache(), mac.id, listOf(summary("thread-1")), MobileCacheLimits()),
        )

        val disconnected = reduce(connected, AppAction.Disconnected(mac.id))

        assertIs<ConnectionPhase.Disconnected>(disconnected.connection)
        assertNull(disconnected.selectedView.interruptingTurnId)
        assertEquals(listOf("thread-1"), disconnected.cache.profile(mac.id).threadList.map { it.id })
    }

    private fun summary(id: String) = ThreadSummary(
        id = id,
        preview = "preview",
        workingDirectory = WorkingDirectory("/workspace"),
        createdAtMs = 1,
        updatedAtMs = 1,
        status = ThreadStatus.Idle,
    )
}

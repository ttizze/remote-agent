package dev.remoteagent.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertIs
import kotlin.test.assertNull

class AppStateTest {
    private val mac = HostProfile("host-mac", "Mac", listOf("192.0.2.1:49152"), "device-mac")
    private val linux = HostProfile("host-linux", "Linux", listOf("192.0.2.2:49152"), "device-linux")

    @Test
    fun pairing_keys_profiles_by_host_identity_and_selects_the_new_profile() {
        val first = reduce(AppState(), AppAction.ProfilePaired(mac))
        val replaced = reduce(first, AppAction.ProfilePaired(mac.copy(name = "MacBook")))

        assertEquals(listOf(mac.copy(name = "MacBook")), replaced.profiles)
        assertEquals(mac.hostIdentity, replaced.selectedProfileId)
        assertIs<ConnectionPhase.Disconnected>(replaced.connection)
    }

    @Test
    fun profile_selection_preserves_each_hosts_working_directory_and_cache() {
        val paired = reduce(reduce(AppState(), AppAction.ProfilePaired(mac)), AppAction.ProfilePaired(linux))
        val macSelected = reduce(paired, AppAction.ProfileSelected(mac.hostIdentity))
        val macConnected = reduce(macSelected, AppAction.ConnectSucceeded(mac.hostIdentity))
        val macCwd = reduce(macConnected, AppAction.WorkingDirectoryChanged(mac.hostIdentity, "/mac/project"))
        val withMacList = reduce(macCwd, AppAction.ThreadListLoaded(mac.hostIdentity, listOf(summary("mac-thread"))))

        val linuxSelected = reduce(withMacList, AppAction.ProfileSelected(linux.hostIdentity))
        val linuxConnected = reduce(linuxSelected, AppAction.ConnectSucceeded(linux.hostIdentity))
        val linuxCwd = reduce(linuxConnected, AppAction.WorkingDirectoryChanged(linux.hostIdentity, "/linux/project"))

        assertEquals("/linux/project", linuxCwd.workingDirectoryPath)
        assertEquals("/mac/project", linuxCwd.profileViews.getValue(mac.hostIdentity).workingDirectoryPath)
        assertEquals(listOf("mac-thread"), linuxCwd.cache.profile(mac.hostIdentity).threadList.map { it.id })
        assertEquals(emptyList(), linuxCwd.cache.profile(linux.hostIdentity).threadList)
    }

    @Test
    fun working_directory_cannot_change_until_the_selected_host_is_connected() {
        val paired = reduce(AppState(), AppAction.ProfilePaired(mac))
        val ignored = reduce(paired, AppAction.WorkingDirectoryChanged(mac.hostIdentity, "/any/directory"))

        assertEquals("", ignored.workingDirectoryPath)
    }

    @Test
    fun disconnection_keeps_the_non_authoritative_cache_but_clears_only_live_control_state() {
        val connected = AppState(
            profiles = listOf(mac),
            selectedProfileId = mac.hostIdentity,
            profileViews = mapOf(mac.hostIdentity to ProfileViewState(connection = ConnectionPhase.Connected, interruptingTurnId = "turn-1")),
            cache = reconcileThreadList(MobileCache(), mac.hostIdentity, listOf(summary("thread-1")), MobileCacheLimits()),
        )

        val disconnected = reduce(connected, AppAction.Disconnected(mac.hostIdentity))

        assertIs<ConnectionPhase.Disconnected>(disconnected.connection)
        assertNull(disconnected.selectedView.interruptingTurnId)
        assertEquals(listOf("thread-1"), disconnected.cache.profile(mac.hostIdentity).threadList.map { it.id })
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

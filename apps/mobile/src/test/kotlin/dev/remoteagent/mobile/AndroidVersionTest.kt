package dev.remoteagent.mobile

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class AndroidVersionTest {
    @Test
    fun olderAndroidVersionsOpenWithoutRequestingANonexistentPermission() {
        for (sdk in 26..36) {
            assertFalse("API $sdk must not be blocked", requiresLocalNetworkPermission(sdk))
        }
    }

    @Test
    fun android17AndLaterRetainLocalNetworkPermissionProtection() {
        for (sdk in 37..40) {
            assertTrue("API $sdk must request permission", requiresLocalNetworkPermission(sdk))
        }
    }
}

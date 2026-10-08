package dev.remoteagent.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(manifest = Config.NONE, sdk = [35])
class PushNotificationIdentityTest {
    @Test
    fun notificationIdentityUsesTagsAndUniquePendingIntentData() {
        assertEquals(ONGOING_ACTIVITY_TAG, notificationTag("ignored", ongoing = true))
        assertNotEquals(ONGOING_ACTIVITY_TAG, notificationTag("agent-activity", ongoing = false))
        assertNotEquals(
            notificationTag("remoteagent://threads/host/one", ongoing = false),
            notificationTag("remoteagent://threads/host/two", ongoing = false),
        )
        assertNotEquals(
            notificationIntentData("remoteagent://threads/host/one"),
            notificationIntentData("remoteagent://threads/host/two"),
        )
    }
}

package dev.remoteagent.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class AgentPushMessagingServiceTest {
    @Test
    fun activityPresentationPrioritizesAttentionAndKeepsTheOngoingState() {
        val presentation = parseActivityPresentation(
            """
            {
              "title":"Project",
              "subtitle":"Agent work",
              "activeCount":2,
              "activities":[
                {"phase":"running","status":"Working","threadTitle":"Build","projectTitle":"Project","deepLink":"remoteagent://threads/host/build"},
                {"phase":"waiting_for_input","status":"Input","threadTitle":"Answer","projectTitle":"Project","deepLink":"remoteagent://threads/host/answer"}
              ]
            }
            """.trimIndent()
        )

        requireNotNull(presentation)
        assertEquals("2 active agents · 1 needs attention", presentation.title)
        assertEquals("Input · Answer · Project\nWorking · Build · Project", presentation.body)
        assertEquals("remoteagent://threads/host/answer", presentation.deepLink)
        assertTrue(presentation.active)
    }

    @Test
    fun activityPresentationFallsBackToCompletionAndRejectsOversizedJson() {
        val presentation = parseActivityPresentation(
            """
            {"title":"Project","subtitle":"Done","activeCount":0,
             "activities":[{"phase":"failed","status":"Failed","threadTitle":"Run","projectTitle":"Project"}]}
            """.replace("\n", "").replace(" ", "")
        )

        requireNotNull(presentation)
        assertEquals("Agent work failed", presentation.title)
        assertFalse(presentation.active)
        assertNull(parseActivityPresentation("x".repeat(4_097)))
    }

    @Test
    fun activityPresentationRejectsWrongJsonTypesWithoutThrowing() {
        assertNull(parseActivityPresentation("""{"activeCount":{},"activities":[]}"""))
        assertNull(parseActivityPresentation("""{"activeCount":1,"activities":{}}"""))
    }

    @Test
    fun coldStartRemovalReconstructsTheSameHostScopedDeviceId() {
        val base = "device-base"
        assertEquals(
            PushRegistrationStore.scopedDeviceId(base, "host-a"),
            PushRegistrationStore.scopedDeviceId(base, "host-a"),
        )
        assertFalse(
            PushRegistrationStore.scopedDeviceId(base, "host-a") ==
                PushRegistrationStore.scopedDeviceId(base, "host-b"),
        )
    }

    @Test
    fun notificationIdentityUsesTagsAndUniquePendingIntentData() {
        assertEquals(ONGOING_ACTIVITY_TAG, notificationTag("ignored", ongoing = true))
        assertNotEquals(
            ONGOING_ACTIVITY_TAG,
            notificationTag("agent-activity", ongoing = false),
        )
        assertNotEquals(
            notificationTag("remoteagent://threads/host/one", ongoing = false),
            notificationTag("remoteagent://threads/host/two", ongoing = false),
        )
        assertNotEquals(
            notificationIntentData("remoteagent://threads/host/one"),
            notificationIntentData("remoteagent://threads/host/two"),
        )
    }

    @Test
    fun notificationAvailabilityIncludesPermissionPackageAndChannelState() {
        assertTrue(notificationsAllowed(permissionGranted = true, packageEnabled = true, channelBlocked = false))
        assertFalse(notificationsAllowed(permissionGranted = false, packageEnabled = true, channelBlocked = false))
        assertFalse(notificationsAllowed(permissionGranted = true, packageEnabled = false, channelBlocked = false))
        assertFalse(notificationsAllowed(permissionGranted = true, packageEnabled = true, channelBlocked = true))
    }
}

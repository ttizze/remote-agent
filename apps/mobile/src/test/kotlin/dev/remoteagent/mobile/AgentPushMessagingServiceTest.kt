package dev.remoteagent.mobile

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class AgentPushMessagingServiceTest {
    private fun activityState(updatedAt: String): JsonObject =
        Json.parseToJsonElement(
            """
            {
              "title":"Project",
              "subtitle":"Agent work",
              "activeCount":1,
              "updatedAt":"$updatedAt",
              "activities":[]
            }
            """.trimIndent(),
        ).jsonObject

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

    @Test
    fun staleActivityPayloadCannotReplaceNewerHostState() {
        val now = 1_800_000_000_000L
        val newer = activityState("2027-01-15T08:00:02Z")
        val older = activityState("2027-01-15T08:00:01Z")
        val accepted = mergeActivityStates(emptyMap(), "host-a", newer, now, deliveryAllowed = true)
        val ignored = mergeActivityStates(accepted.states, "host-a", older, now, deliveryAllowed = true)

        assertEquals(ActivityStateMergeDisposition.Accepted, accepted.disposition)
        assertEquals(ActivityStateMergeDisposition.Ignored, ignored.disposition)
        assertEquals(newer, ignored.states.getValue("host-a"))
    }

    @Test
    fun expiredActivityStateIsDroppedFromAggregateAndIncomingPayload() {
        val now = 1_800_000_000_000L
        val expired = activityState("2027-01-15T07:49:59Z")

        assertFalse(activityStateIsFresh(expired, now))
        assertTrue(retainedActivityStates(mapOf("host-a" to expired), now).isEmpty())
        val result = mergeActivityStates(emptyMap(), "host-a", expired, now, deliveryAllowed = true)
        assertEquals(ActivityStateMergeDisposition.Ignored, result.disposition)
        assertTrue(result.states.isEmpty())
    }

    @Test
    fun removalGateWinsOverLateActivityPayload() {
        val now = 1_800_000_000_000L
        val state = activityState("2027-01-15T08:00:00Z")
        val result = mergeActivityStates(
            mapOf("host-a" to state),
            "host-a",
            state,
            now,
            deliveryAllowed = false,
        )

        assertEquals(ActivityStateMergeDisposition.Blocked, result.disposition)
        assertTrue(result.states.isEmpty())
    }
}

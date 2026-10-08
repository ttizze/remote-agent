package dev.remoteagent.mobile

import android.Manifest
import android.app.Application
import android.app.NotificationManager
import android.content.Context
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(manifest = Config.NONE, sdk = [35])
class LocalNotificationsTest {
    private fun context(): Context {
        val context = RuntimeEnvironment.getApplication<Application>()
        shadowOf(context).grantPermissions(Manifest.permission.POST_NOTIFICATIONS)
        LocalNotifications.clearDelivered(context)
        return context
    }

    @Test
    fun routes_keep_distinct_full_tags_and_badge_refresh_is_silent() {
        val context = context()
        val manager = context.getSystemService(NotificationManager::class.java)
        val firstRoute = "remoteagent://threads/host/Aa"
        val secondRoute = "remoteagent://threads/host/BB"
        try {
            LocalNotifications.deliver(context, "Input A", "A", sound = true, deepLink = firstRoute)
            LocalNotifications.deliver(context, "Input B", "B", sound = true, deepLink = secondRoute)
            val before = manager.activeNotifications.associateBy { it.tag }
            assertEquals(firstRoute.hashCode(), secondRoute.hashCode())
            assertEquals(setOf("local-attention:$firstRoute", "local-attention:$secondRoute"), before.keys)
            LocalNotifications.updateBadge(context)
            val after = manager.activeNotifications.associateBy { it.tag }
            assertEquals(before.keys, after.keys)
            assertTrue(
                after.values.all { it.notification.flags and android.app.Notification.FLAG_ONLY_ALERT_ONCE != 0 }
            )
        } finally {
            LocalNotifications.clearDelivered(context)
        }
    }

    @Test
    fun replacing_one_route_does_not_remove_another_route() {
        val context = context()
        val manager = context.getSystemService(NotificationManager::class.java)
        val firstRoute = "remoteagent://threads/host-a/thread-a"
        val secondRoute = "remoteagent://threads/host-b/thread-b"
        try {
            LocalNotifications.deliver(context, "Old", "old", false, deepLink = firstRoute)
            LocalNotifications.deliver(context, "Other", "other", false, deepLink = secondRoute)
            LocalNotifications.deliver(context, "New", "new", false, deepLink = firstRoute)

            val notices = manager.activeNotifications.associateBy { it.tag }
            assertEquals(2, notices.size)
            assertNotEquals(
                notices["local-attention:$firstRoute"]
                    ?.notification
                    ?.extras
                    ?.getString(android.app.Notification.EXTRA_TITLE),
                "Old",
            )
            assertEquals(
                "New",
                notices["local-attention:$firstRoute"]
                    ?.notification
                    ?.extras
                    ?.getString(android.app.Notification.EXTRA_TITLE),
            )
            assertEquals(
                "Other",
                notices["local-attention:$secondRoute"]
                    ?.notification
                    ?.extras
                    ?.getString(android.app.Notification.EXTRA_TITLE),
            )
        } finally {
            LocalNotifications.clearDelivered(context)
        }
    }

    @Test
    fun removing_an_environment_clears_only_its_routes_and_updates_badge() {
        val context = context()
        val manager = context.getSystemService(NotificationManager::class.java)
        try {
            LocalNotifications.deliver(context, "A", "A", false, deepLink = "remoteagent://threads/host-a/thread-a")
            LocalNotifications.deliver(context, "B", "B", false, deepLink = "remoteagent://threads/host-b/thread-b")
            LocalNotifications.removeEnvironment(context, "host-a")

            val notices = manager.activeNotifications.associateBy { it.tag }
            assertEquals(setOf("local-attention:remoteagent://threads/host-b/thread-b"), notices.keys)
        } finally {
            LocalNotifications.clearDelivered(context)
        }
    }
}

package dev.remoteagent.mobile

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Intent
import android.content.Context
import android.content.pm.PackageManager
import android.net.Uri

private data class LocalNotificationRequest(
    val title: String,
    val body: String,
    val threadId: String?,
    val deepLink: String?,
    val sound: Boolean,
    val badgeCount: UInt,
    val kind: String?,
    val soundKind: String?,
)

/** Delivers local attention events; network push transport stays outside the client. */
internal object LocalNotifications {
    private val pending = mutableListOf<LocalNotificationRequest>()
    private val activeNotifications = mutableMapOf<Int, LocalNotificationRequest>()
    /** A denied permission must not retain events for a later replay. */
    private var permissionDenied = false

    fun playSound(context: Context, soundKind: String? = null) {
        val tone = when (soundKind?.substringAfterLast('.')?.uppercase()) {
            "COMPLETION" -> android.media.ToneGenerator.TONE_PROP_BEEP
            else -> android.media.ToneGenerator.TONE_PROP_ACK
        }
        val generator = android.media.ToneGenerator(android.media.AudioManager.STREAM_NOTIFICATION, 80)
        generator.startTone(tone, 180)
        android.os.Handler(android.os.Looper.getMainLooper()).postDelayed(
            { generator.release() },
            250,
        )
    }

    fun deliver(
        context: Context,
        title: String,
        body: String,
        sound: Boolean,
        threadId: String? = null,
        deepLink: String? = null,
        badgeCount: UInt = 0u,
        kind: String? = null,
        soundKind: String? = null,
    ) {
        val request = LocalNotificationRequest(
            title,
            body,
            threadId,
            deepLink,
            sound,
            badgeCount,
            kind,
            soundKind,
        )
        if (
            android.os.Build.VERSION.SDK_INT >= 33 &&
                context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) !=
                    PackageManager.PERMISSION_GRANTED
        ) {
            if (permissionDenied) {
                synchronized(pending) { pending.clear() }
                return
            }
            synchronized(pending) {
                pending.removeAll {
                    if (deepLink != null) it.deepLink == deepLink else it.threadId == threadId
                }
                pending += request
            }
            return
        }
        post(context, request)
    }

    /** Replays events queued while Android's notification permission sheet was open. */
    fun permissionResult(context: Context, granted: Boolean) {
        if (!granted) {
            permissionDenied = true
            synchronized(pending) { pending.clear() }
            return
        }
        permissionDenied = false
        val requests = synchronized(pending) {
            val copy = pending.toList()
            pending.clear()
            copy
        }
        requests.forEach { post(context, it) }
    }

    /** Clears this client's native attention notifications when core reports
     * that focus or selection removed the aggregate badge. */
    fun updateBadge(context: Context, count: UInt) {
        val manager = context.getSystemService(NotificationManager::class.java)
        synchronized(activeNotifications) {
            if (count == 0u) {
                activeNotifications.keys.forEach { id -> manager.cancel(id) }
                activeNotifications.clear()
            } else {
                // Existing notifications retain the number they were posted
                // with, so repost each active request when another Host's
                // attention count changes without creating a new event.
                activeNotifications.values
                    .map { it.copy(badgeCount = count) }
                    .forEach { request -> post(context, request) }
            }
        }
    }

    /** Removes a notification after its content intent has been consumed. */
    fun acknowledge(deepLink: String?) {
        val id = deepLink?.hashCode() ?: return
        synchronized(activeNotifications) { activeNotifications.remove(id) }
    }

    fun clearDelivered(context: Context) {
        context.getSystemService(NotificationManager::class.java).cancelAll()
        synchronized(activeNotifications) { activeNotifications.clear() }
    }

    private fun post(context: Context, request: LocalNotificationRequest) {
        val soundClass = request.soundKind?.substringAfterLast('.')?.lowercase() ?: "input"
        val channelId =
            "remote-agent-attention-${if (request.sound) "sound" else "silent"}-$soundClass"
        val manager = context.getSystemService(NotificationManager::class.java)
        val channel = NotificationChannel(
            channelId,
            "Remote Agent attention",
            NotificationManager.IMPORTANCE_HIGH,
        ).apply {
            enableVibration(true)
            setSound(
                if (request.sound) android.media.RingtoneManager.getDefaultUri(android.media.RingtoneManager.TYPE_NOTIFICATION)
                else null,
                null,
            )
        }
        manager.createNotificationChannel(channel)
        val builder = Notification.Builder(context, channelId)
            .setSmallIcon(android.R.drawable.ic_dialog_info)
            .setContentTitle(request.title)
            .setContentText(request.body)
            .setAutoCancel(true)
            .setNumber(request.badgeCount.toInt())
        request.threadId?.let { threadId ->
            val route = Intent(
                Intent.ACTION_VIEW,
                request.deepLink?.let(Uri::parse)
                    ?: Uri.parse("remote-agent://thread/" + Uri.encode(threadId)),
                context,
                MainActivity::class.java,
            )
            request.kind?.let { route.putExtra("notificationKind", it) }
            request.soundKind?.let { route.putExtra("notificationSoundKind", it) }
            builder.setContentIntent(
                PendingIntent.getActivity(
                    context,
                    (request.deepLink ?: threadId).hashCode(),
                    route,
                    PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
                )
            )
        }
        val notificationId = request.deepLink?.hashCode()
            ?: request.threadId?.hashCode()
            ?: request.body.hashCode()
        manager.notify(notificationId, builder.build())
        synchronized(activeNotifications) { activeNotifications[notificationId] = request }
    }
}

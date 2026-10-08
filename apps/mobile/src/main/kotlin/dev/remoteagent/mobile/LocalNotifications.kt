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
    val sound: Boolean,
    val badge: Boolean,
    val kind: String?,
    val soundKind: String?,
)

/** Delivers local attention events; network push transport stays outside the client. */
internal object LocalNotifications {
    private val pending = mutableListOf<LocalNotificationRequest>()

    fun playSound(context: Context) {
        val ringtone =
            android.media.RingtoneManager.getRingtone(
                context,
                android.media.RingtoneManager.getDefaultUri(
                    android.media.RingtoneManager.TYPE_NOTIFICATION,
                ),
            )
        ringtone?.play()
    }

    fun deliver(
        context: Context,
        title: String,
        body: String,
        sound: Boolean,
        threadId: String? = null,
        badge: Boolean = true,
        kind: String? = null,
        soundKind: String? = null,
    ) {
        val request = LocalNotificationRequest(title, body, threadId, sound, badge, kind, soundKind)
        if (
            android.os.Build.VERSION.SDK_INT >= 33 &&
                context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) !=
                    PackageManager.PERMISSION_GRANTED
        ) {
            synchronized(pending) {
                pending.removeAll { it.threadId == threadId }
                pending += request
            }
            return
        }
        post(context, request)
    }

    /** Replays events queued while Android's notification permission sheet was open. */
    fun permissionResult(context: Context, granted: Boolean) {
        if (!granted &&
            android.os.Build.VERSION.SDK_INT >= 33 &&
            context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) !=
                PackageManager.PERMISSION_GRANTED
        ) {
            return
        }
        val requests = synchronized(pending) {
            val copy = pending.toList()
            pending.clear()
            copy
        }
        requests.forEach { post(context, it) }
    }

    private fun post(context: Context, request: LocalNotificationRequest) {
        val channelId = "remote-agent-attention-${if (request.sound) "sound" else "silent"}"
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
            .setNumber(if (request.badge) 1 else 0)
        request.threadId?.let { threadId ->
            val route = Intent(
                Intent.ACTION_VIEW,
                Uri.parse("remote-agent://thread/${Uri.encode(threadId)}"),
                context,
                MainActivity::class.java,
            )
            request.kind?.let { route.putExtra("notificationKind", it) }
            request.soundKind?.let { route.putExtra("notificationSoundKind", it) }
            builder.setContentIntent(
                PendingIntent.getActivity(
                    context,
                    threadId.hashCode(),
                    route,
                    PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
                )
            )
        }
        manager.notify(request.threadId?.hashCode() ?: request.body.hashCode(), builder.build())
    }
}

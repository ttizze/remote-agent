package dev.remoteagent.mobile

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import android.content.pm.PackageManager

/** Delivers local attention events; network push transport stays outside the client. */
internal object LocalNotifications {
    fun deliver(context: Context, title: String, body: String, sound: Boolean) {
        if (
            android.os.Build.VERSION.SDK_INT >= 33 &&
                context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) !=
                    PackageManager.PERMISSION_GRANTED
        ) {
            return
        }
        val channelId = "remote-agent-attention-${if (sound) "sound" else "silent"}"
        val manager = context.getSystemService(NotificationManager::class.java)
        val channel = NotificationChannel(
            channelId,
            "Bex attention",
            NotificationManager.IMPORTANCE_HIGH,
        ).apply {
            enableVibration(true)
            setSound(
                if (sound) android.media.RingtoneManager.getDefaultUri(android.media.RingtoneManager.TYPE_NOTIFICATION)
                else null,
                null,
            )
        }
        manager.createNotificationChannel(channel)
        val notification = Notification.Builder(context, channelId)
            .setSmallIcon(android.R.drawable.ic_dialog_info)
            .setContentTitle(title)
            .setContentText(body)
            .setAutoCancel(true)
            .setNumber(1)
            .build()
        manager.notify(body.hashCode(), notification)
    }
}

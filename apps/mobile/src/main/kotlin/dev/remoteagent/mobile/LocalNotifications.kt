package dev.remoteagent.mobile

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build

private data class LocalNotificationRequest(
    val title: String,
    val body: String,
    val threadId: String?,
    val deepLink: String?,
    val sound: Boolean,
    val kind: String?,
    val soundKind: String?,
)

/** Delivers local attention events; network push transport stays outside the client. */
internal object LocalNotifications {
    private val pending = mutableListOf<LocalNotificationRequest>()
    /** Posted notices are keyed by their complete route, never a lossy hash. */
    private val activeNotifications = mutableMapOf<String, LocalNotificationRequest>()
    /** A denied permission must not retain events for a later replay. */
    private var permissionDenied = false

    fun playSound(context: Context, soundKind: String? = null) {
        val resource =
            when (soundKind?.substringAfterLast('.')?.uppercase()) {
                "COMPLETION" -> R.raw.notification_completion
                else -> R.raw.notification_input
            }
        android.media.MediaPlayer.create(context, resource)?.let { player ->
            player.setOnCompletionListener { it.release() }
            player.setOnErrorListener { mediaPlayer, _, _ ->
                mediaPlayer.release()
                true
            }
            player.start()
        }
    }

    fun deliver(
        context: Context,
        title: String,
        body: String,
        sound: Boolean,
        threadId: String? = null,
        deepLink: String? = null,
        kind: String? = null,
        soundKind: String? = null,
    ) {
        val request = LocalNotificationRequest(title, body, threadId, deepLink, sound, kind, soundKind)
        if (
            Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
                context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            if (permissionDenied) {
                synchronized(pending) { pending.clear() }
                return
            }
            synchronized(pending) {
                pending.removeAll { if (deepLink != null) it.deepLink == deepLink else it.threadId == threadId }
                pending += request
            }
            return
        }
        post(context, request, alert = true)
        updateBadge(context)
    }

    /** Replays events queued while Android's notification permission sheet was open. */
    fun permissionResult(context: Context, granted: Boolean) {
        if (!granted) {
            permissionDenied = true
            synchronized(pending) { pending.clear() }
            clearDelivered(context)
            return
        }
        permissionDenied = false
        val requests =
            synchronized(pending) {
                val copy = pending.toList()
                pending.clear()
                copy
            }
        requests.forEach { post(context, it, alert = true) }
        updateBadge(context)
    }

    /**
     * The badge is the number of OS notices posted by this client. Core's current waiting-row count is intentionally
     * not used here: a completed notice remains pending until focus and a replacement keeps one tag.
     */
    fun updateBadge(context: Context) {
        val manager = context.getSystemService(NotificationManager::class.java)
        val requests = synchronized(activeNotifications) { activeNotifications.values.toList() }
        if (requests.isEmpty()) {
            manager.activeNotifications
                .filter { it.tag?.startsWith(LOCAL_ATTENTION_PREFIX) == true }
                .forEach { notice -> notice.tag?.let { manager.cancel(it, notice.id) } }
            return
        }
        // Rebuild the number on each existing notice without replaying its
        // sound/vibration. A badge refresh is an OS update, not a new event.
        requests.forEach { request -> post(context, request, alert = false) }
    }

    /** Removes a notification after its content intent has been consumed. */
    fun acknowledge(context: Context, deepLink: String?) {
        val route = deepLink ?: return
        val tag = notificationTag(route)
        context.getSystemService(NotificationManager::class.java).cancel(tag, LOCAL_NOTIFICATION_ID)
        synchronized(activeNotifications) { activeNotifications.remove(tag) }
        updateBadge(context)
    }

    fun clearDelivered(context: Context) {
        val manager = context.getSystemService(NotificationManager::class.java)
        manager.activeNotifications
            .filter { it.tag?.startsWith(LOCAL_ATTENTION_PREFIX) == true }
            .forEach { notice -> notice.tag?.let { manager.cancel(it, notice.id) } }
        synchronized(activeNotifications) {
            activeNotifications.keys.forEach { tag -> manager.cancel(tag, LOCAL_NOTIFICATION_ID) }
            activeNotifications.clear()
        }
        synchronized(pending) { pending.clear() }
    }

    /** Removes only notices owned by a Host that left the profile registry. */
    fun removeEnvironment(context: Context, environmentId: String) {
        val manager = context.getSystemService(NotificationManager::class.java)
        val removedTags =
            synchronized(activeNotifications) {
                activeNotifications
                    .filter { (_, request) -> environmentId == request.deepLink?.let(::deepLinkEnvironmentId) }
                    .map { (tag, _) -> tag }
                    .also { tags -> tags.forEach { tag -> activeNotifications.remove(tag) } }
            }
        removedTags.forEach { tag -> manager.cancel(tag, LOCAL_NOTIFICATION_ID) }
        synchronized(pending) { pending.removeAll { environmentId == it.deepLink?.let(::deepLinkEnvironmentId) } }
        updateBadge(context)
    }

    private fun post(context: Context, request: LocalNotificationRequest, alert: Boolean) {
        val tag = notificationTag(request)
        val manager = context.getSystemService(NotificationManager::class.java)
        if (alert) {
            // A replacement is a new event for the same route. Dismiss the
            // old record first so the replacement can alert once.
            manager.cancel(tag, LOCAL_NOTIFICATION_ID)
        }
        val postedCount =
            synchronized(activeNotifications) {
                activeNotifications[tag] = request
                activeNotifications.size
            }
        val soundClass = request.soundKind?.substringAfterLast('.')?.lowercase() ?: "input"
        val channelId = "remote-agent-attention-${if (request.sound) "sound" else "silent"}-$soundClass"
        val channel =
            NotificationChannel(channelId, "Remote Agent attention", NotificationManager.IMPORTANCE_HIGH).apply {
                enableVibration(true)
                val soundUri =
                    if (soundClass == "completion") {
                        android.net.Uri.parse(
                            "android.resource://${context.packageName}/${R.raw.notification_completion}"
                        )
                    } else {
                        android.net.Uri.parse("android.resource://${context.packageName}/${R.raw.notification_input}")
                    }
                setSound(if (request.sound) soundUri else null, null)
            }
        manager.createNotificationChannel(channel)
        val builder =
            Notification.Builder(context, channelId)
                .setSmallIcon(android.R.drawable.ic_dialog_info)
                .setContentTitle(request.title)
                .setContentText(request.body)
                .setAutoCancel(true)
                .setOnlyAlertOnce(true)
                .setNumber(postedCount)
        if (!alert) builder.setSilent(true)
        request.deepLink?.let { deepLink ->
            val route = Intent(Intent.ACTION_VIEW, Uri.parse(deepLink), context, MainActivity::class.java)
            request.kind?.let { route.putExtra("notificationKind", it) }
            request.soundKind?.let { route.putExtra("notificationSoundKind", it) }
            builder.setContentIntent(
                PendingIntent.getActivity(
                    context,
                    LOCAL_NOTIFICATION_ID,
                    route,
                    PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
                )
            )
        }
        manager.notify(tag, LOCAL_NOTIFICATION_ID, builder.build())
    }

    private const val LOCAL_NOTIFICATION_ID = 1
    private const val LOCAL_ATTENTION_PREFIX = "$LOCAL_ATTENTION_TAG:"

    private fun notificationTag(request: LocalNotificationRequest): String =
        notificationTag(request.deepLink ?: request.threadId ?: "local")

    private fun notificationTag(route: String): String = "$LOCAL_ATTENTION_TAG:$route"

    private fun deepLinkEnvironmentId(deepLink: String): String? =
        runCatching { Uri.parse(deepLink) }
            .getOrNull()
            ?.takeIf { it.scheme == "remoteagent" && it.host == "threads" }
            ?.pathSegments
            ?.takeIf { it.size == 2 }
            ?.firstOrNull()
}

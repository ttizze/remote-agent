package dev.remoteagent.mobile

import android.Manifest
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import com.google.firebase.FirebaseApp
import com.google.firebase.FirebaseOptions
import java.security.MessageDigest
import java.util.UUID

internal const val ACTION_PUSH_TOKEN_UPDATED = "dev.remoteagent.mobile.PUSH_TOKEN_UPDATED"
internal const val ACTION_OPEN_PUSH = "dev.remoteagent.mobile.OPEN_PUSH"
internal const val EXTRA_PUSH_DEEP_LINK = "push_deep_link"
internal const val EXTRA_OPEN_USAGE = "open_usage"

internal data class PushRegistrationPreferences(
    val notificationsEnabled: Boolean,
    val notifyOnApproval: Boolean,
    val notifyOnInput: Boolean,
    val notifyOnCompletion: Boolean,
    val notifyOnFailure: Boolean,
    val liveActivitiesEnabled: Boolean,
)

internal enum class PushCapability {
    DisabledByPreference,
    UnsupportedUnconfigured,
    ProviderUnavailable,
    Ready,
}

/**
 * Firebase app identifiers are public build configuration. The service
 * account used by the Host never enters this app. An absent configuration is
 * reported as an unsupported capability instead of a swallowed SDK error.
 */
internal object FirebasePushBootstrap {
    fun ensure(context: Context): Boolean {
        if (FirebaseApp.getApps(context).any { it.name == FirebaseApp.DEFAULT_APP_NAME }) {
            return true
        }
        if (BuildConfig.FIREBASE_API_KEY.isBlank() ||
            BuildConfig.FIREBASE_APPLICATION_ID.isBlank() ||
            BuildConfig.FIREBASE_PROJECT_ID.isBlank() ||
            BuildConfig.FIREBASE_SENDER_ID.isBlank()
        ) {
            return false
        }
        val builder = FirebaseOptions.Builder()
            .setApiKey(BuildConfig.FIREBASE_API_KEY)
            .setApplicationId(BuildConfig.FIREBASE_APPLICATION_ID)
            .setProjectId(BuildConfig.FIREBASE_PROJECT_ID)
            .setGcmSenderId(BuildConfig.FIREBASE_SENDER_ID)
        if (BuildConfig.FIREBASE_STORAGE_BUCKET.isNotBlank()) {
            builder.setStorageBucket(BuildConfig.FIREBASE_STORAGE_BUCKET)
        }
        return runCatching { FirebaseApp.initializeApp(context, builder.build()) != null }.getOrDefault(false)
    }
}

private const val CHANNEL_ID = "agent_awareness"
private const val CHANNEL_NAME = "Agent activity"

internal object PushRegistrationStore {
    private const val PREFERENCES = "push-registration"
    private const val DEVICE_ID = "device-id"
    private const val TOKEN = "fcm-token"

    fun baseDeviceId(context: Context): String {
        val preferences = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
        return preferences.getString(DEVICE_ID, null)?.takeIf(String::isNotBlank) ?: UUID.randomUUID().toString().also {
            preferences.edit().putString(DEVICE_ID, it).apply()
        }
    }

    /** Each Host gets a stable principal so its registration cannot overwrite another Host. */
    fun deviceId(context: Context, hostId: String): String {
        return scopedDeviceId(baseDeviceId(context), hostId)
    }

    /** Pure counterpart used by cold-start removal and unit tests. */
    internal fun scopedDeviceId(base: String, hostId: String): String {
        val candidate = "$base:$hostId"
        if (candidate.toByteArray(Charsets.UTF_8).size <= 128) return candidate
        val digest = MessageDigest.getInstance("SHA-256")
            .digest(hostId.toByteArray(Charsets.UTF_8))
            .joinToString("") { byte -> "%02x".format(byte) }
        return "$base:$digest"
    }

    fun token(context: Context): String? =
        context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).getString(TOKEN, null)

    fun saveToken(context: Context, token: String) {
        context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE).edit().putString(TOKEN, token).apply()
    }

}

internal object PushNotificationCenter {
    @Volatile private var visibleThread: String? = null

    fun setVisibleThread(deepLink: String?) {
        visibleThread = deepLink
    }

    fun notificationsEnabled(context: Context): Boolean =
        Build.VERSION.SDK_INT < 33 ||
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) ==
                PackageManager.PERMISSION_GRANTED

    fun canRequestPermission(context: Context): Boolean =
        Build.VERSION.SDK_INT >= 33 &&
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) !=
                PackageManager.PERMISSION_GRANTED

    fun show(context: Context, title: String, body: String, deepLink: String?) {
        show(context, title, body, deepLink, deepLink ?: "title:$title")
    }

    fun show(context: Context, title: String, body: String, deepLink: String?, key: String) {
        post(
            context,
            title,
            body,
            deepLink,
            ongoing = false,
            key = key,
            suppressVisible = true,
        )
    }

    fun showActivity(
        context: Context,
        title: String,
        body: String,
        deepLink: String?,
        active: Boolean,
    ) {
        if (!active) {
            cancelActivity(context)
            return
        }
        post(
            context,
            title,
            body,
            deepLink,
            ongoing = active,
            key = "agent-activity",
            suppressVisible = false,
        )
    }

    fun cancelActivity(context: Context) {
        context.getSystemService(NotificationManager::class.java)
            ?.cancel(NotificationIds.id(context, "agent-activity"))
    }

    private fun post(
        context: Context,
        title: String,
        body: String,
        deepLink: String?,
        ongoing: Boolean,
        key: String,
        suppressVisible: Boolean,
    ) {
        if (suppressVisible && deepLink != null && deepLink == visibleThread) return
        if (!notificationsEnabled(context)) return
        val manager = context.getSystemService(NotificationManager::class.java) ?: return
        ensureChannel(manager)
        val intent = Intent(context, MainActivity::class.java).apply {
            action = ACTION_OPEN_PUSH
            putExtra(EXTRA_PUSH_DEEP_LINK, deepLink)
            data = deepLink?.let(Uri::parse)
            flags = Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP
        }
        val pending = PendingIntent.getActivity(
            context,
            NotificationIds.id(context, key),
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val notificationId = NotificationIds.id(context, key)
        val notification = NotificationCompat.Builder(context, CHANNEL_ID)
            .setSmallIcon(R.drawable.bex_icon)
            .setContentTitle(title)
            .setContentText(body)
            .setStyle(NotificationCompat.BigTextStyle().bigText(body))
            .setAutoCancel(!ongoing)
            .setOngoing(ongoing)
            .setOnlyAlertOnce(ongoing)
            .setContentIntent(pending)
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .build()
        manager.notify(notificationId, notification)
    }

    private fun ensureChannel(manager: NotificationManager) {
        if (Build.VERSION.SDK_INT >= 26 && manager.getNotificationChannel(CHANNEL_ID) == null) {
            manager.createNotificationChannel(
                NotificationChannel(CHANNEL_ID, CHANNEL_NAME, NotificationManager.IMPORTANCE_HIGH)
            )
        }
    }
}

/** Allocates stable, collision-free notification ids without deriving them
 * from untrusted deep-link hash codes. */
internal object NotificationIds {
    private const val PREFERENCES = "push-notification-ids"
    private const val NEXT_ID = "next-id"
    private const val FIRST_ID = 1_000

    @Synchronized
    fun id(context: Context, key: String): Int {
        val preferences = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
        val stored = preferences.getInt("id:$key", 0)
        if (stored > 0) return stored
        val used = preferences.all.values.filterIsInstance<Int>().toMutableSet()
        var next = preferences.getInt(NEXT_ID, FIRST_ID).coerceAtLeast(FIRST_ID)
        while (next <= 0 || used.contains(next)) {
            next = if (next == Int.MAX_VALUE) FIRST_ID else next + 1
        }
        preferences.edit().putInt("id:$key", next).putInt(NEXT_ID, if (next == Int.MAX_VALUE) FIRST_ID else next + 1).apply()
        return next
    }
}

package dev.remoteagent.mobile

import android.Manifest
import android.app.AlarmManager
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.ProcessLifecycleOwner
import com.google.firebase.FirebaseApp
import com.google.firebase.FirebaseOptions
import dev.remoteagent.core.agentActivityDisplayExpiryAt
import java.security.MessageDigest
import java.util.UUID

internal const val ACTION_PUSH_TOKEN_UPDATED = "dev.remoteagent.mobile.PUSH_TOKEN_UPDATED"
internal const val ACTION_OPEN_PUSH = "dev.remoteagent.mobile.OPEN_PUSH"
internal const val EXTRA_PUSH_DEEP_LINK = "push_deep_link"
internal const val EXTRA_OPEN_USAGE = "open_usage"

internal enum class PushCapability {
    DisabledByPreference,
    UnsupportedUnconfigured,
    ProviderUnavailable,
    Ready,
}

/**
 * Firebase app identifiers are public build configuration. The service account used by the Host never enters this app.
 * An absent configuration is reported as an unsupported capability instead of a swallowed SDK error.
 */
internal object FirebasePushBootstrap {
    fun ensure(context: Context): Boolean {
        return when {
            FirebaseApp.getApps(context).any { it.name == FirebaseApp.DEFAULT_APP_NAME } -> true
            !firebaseBuildConfigurationAvailable() -> false
            else -> {
                val builder =
                    FirebaseOptions.Builder()
                        .setApiKey(BuildConfig.FIREBASE_API_KEY)
                        .setApplicationId(BuildConfig.FIREBASE_APPLICATION_ID)
                        .setProjectId(BuildConfig.FIREBASE_PROJECT_ID)
                        .setGcmSenderId(BuildConfig.FIREBASE_SENDER_ID)
                if (BuildConfig.FIREBASE_STORAGE_BUCKET.isNotBlank()) {
                    builder.setStorageBucket(BuildConfig.FIREBASE_STORAGE_BUCKET)
                }
                runCatching { FirebaseApp.initializeApp(context, builder.build()) != null }.getOrDefault(false)
            }
        }
    }
}

private fun firebaseBuildConfigurationAvailable(): Boolean =
    sequenceOf(
            BuildConfig.FIREBASE_API_KEY,
            BuildConfig.FIREBASE_APPLICATION_ID,
            BuildConfig.FIREBASE_PROJECT_ID,
            BuildConfig.FIREBASE_SENDER_ID,
        )
        .all(String::isNotBlank)

private const val CHANNEL_ID = "agent_awareness"
private const val CHANNEL_NAME = "Agent activity"
private const val ALERT_HISTORY_PREFERENCES = "push-alert-history"
private const val ALERT_HISTORY_KEY = "seen"
private val ALERT_HISTORY_LOCK = Any()
private const val ACTIVITY_DISPLAY_PREFERENCES = "push-activity-display"
private const val ACTIVITY_DISPLAY_EXPIRY_KEY = "expires_at"
private const val PUSH_DEVICE_ID_MAX_BYTES = 128
private const val ALERT_HISTORY_LIMIT = 64
internal const val AWARENESS_NOTIFICATION_ID = 1_000
internal const val ONGOING_ACTIVITY_TAG = "agent-activity"
internal const val LOCAL_ATTENTION_TAG = "local-attention"
internal const val EXTRA_ACTIVITY_EXPIRY_AT = "activity_expiry_at"
private const val ACTIVITY_EXPIRY_REQUEST_CODE = AWARENESS_NOTIFICATION_ID + 1

/**
 * NotificationManager tags provide the event identity; the integer slot is deliberately fixed so we do not persist an
 * unbounded key-to-id table.
 */
internal fun notificationTag(key: String, ongoing: Boolean): String =
    if (ongoing) ONGOING_ACTIVITY_TAG else "agent-event:$key"

/**
 * PendingIntent identity includes Intent.data, so each event gets a stable unique identity even though the request code
 * is fixed. The actual route is carried separately in EXTRA_PUSH_DEEP_LINK and is validated by the model.
 */
internal fun notificationIntentData(key: String): Uri = Uri.parse("remoteagent://notifications/${Uri.encode(key)}")

internal fun notificationsAllowed(
    permissionGranted: Boolean,
    packageEnabled: Boolean,
    channelBlocked: Boolean,
): Boolean = permissionGranted && packageEnabled && !channelBlocked

internal object PushRegistrationStore {
    private const val PREFERENCES = "push-registration"
    private const val DEVICE_ID = "device-id"
    private const val TOKEN = "fcm-token"

    fun baseDeviceId(context: Context): String {
        val preferences = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
        return preferences.getString(DEVICE_ID, null)?.takeIf(String::isNotBlank)
            ?: UUID.randomUUID().toString().also { preferences.edit().putString(DEVICE_ID, it).apply() }
    }

    /** Each Host gets a stable principal so its registration cannot overwrite another Host. */
    fun deviceId(context: Context, hostId: String): String {
        return scopedDeviceId(baseDeviceId(context), hostId)
    }

    /** Pure counterpart used by cold-start removal and unit tests. */
    internal fun scopedDeviceId(base: String, hostId: String): String {
        val candidate = "$base:$hostId"
        if (candidate.toByteArray(Charsets.UTF_8).size <= PUSH_DEVICE_ID_MAX_BYTES) return candidate
        val digest =
            MessageDigest.getInstance("SHA-256").digest(hostId.toByteArray(Charsets.UTF_8)).joinToString("") { byte ->
                "%02x".format(byte)
            }
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

    fun notificationsEnabled(context: Context): Boolean {
        val permissionGranted =
            Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
                ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) ==
                    PackageManager.PERMISSION_GRANTED
        val packageEnabled = NotificationManagerCompat.from(context).areNotificationsEnabled()
        val channelBlocked =
            Build.VERSION.SDK_INT >= Build.VERSION_CODES.O &&
                context
                    .getSystemService(NotificationManager::class.java)
                    ?.getNotificationChannel(CHANNEL_ID)
                    ?.importance == NotificationManager.IMPORTANCE_NONE
        return notificationsAllowed(permissionGranted, packageEnabled, channelBlocked)
    }

    fun canRequestPermission(context: Context): Boolean =
        Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) !=
                PackageManager.PERMISSION_GRANTED

    fun show(context: Context, title: String, body: String, deepLink: String?) {
        show(context, title, body, deepLink, deepLink ?: "title:$title")
    }

    fun show(
        context: Context,
        title: String,
        body: String,
        deepLink: String?,
        key: String,
        deduplicate: Boolean = false,
    ) {
        if (deduplicate && (!notificationsEnabled(context) || !rememberAlert(context, key))) return
        post(context, title, body, deepLink, ongoing = false, key = key, suppressVisible = true)
    }

    private fun rememberAlert(context: Context, key: String): Boolean {
        synchronized(ALERT_HISTORY_LOCK) {
            val preferences = context.getSharedPreferences(ALERT_HISTORY_PREFERENCES, Context.MODE_PRIVATE)
            val seen = preferences.getString(ALERT_HISTORY_KEY, null)?.split('\n').orEmpty()
            if (key in seen) return false
            preferences
                .edit()
                .putString(ALERT_HISTORY_KEY, (seen.takeLast(ALERT_HISTORY_LIMIT - 1) + key).joinToString("\n"))
                .apply()
            return true
        }
    }

    /**
     * Renders the one ongoing activity card. These values are separate core projection facts and must stay flat so
     * timeout and active state cannot be hidden in a mutable notification wrapper.
     */
    @Suppress("LongParameterList")
    fun showActivity(
        context: Context,
        title: String,
        body: String,
        deepLink: String?,
        active: Boolean,
        expiresAtMillis: Long,
    ) {
        val now = System.currentTimeMillis()
        val displayExpiryAtMillis = agentActivityDisplayExpiryAt(expiresAtMillis, now)
        if (displayExpiryAtMillis <= now) {
            cancelActivity(context)
            return
        }
        val timeoutAfterMillis =
            if (displayExpiryAtMillis == Long.MAX_VALUE) {
                0L
            } else {
                (displayExpiryAtMillis - now).coerceAtLeast(1L)
            }
        if (timeoutAfterMillis > 0L && Build.VERSION.SDK_INT < Build.VERSION_CODES.O) {
            context
                .getSharedPreferences(ACTIVITY_DISPLAY_PREFERENCES, Context.MODE_PRIVATE)
                .edit()
                .putLong(ACTIVITY_DISPLAY_EXPIRY_KEY, displayExpiryAtMillis)
                .apply()
        }
        post(
            context,
            title,
            body,
            deepLink,
            ongoing = active,
            key = "agent-activity",
            suppressVisible = false,
            timeoutAfterMillis = timeoutAfterMillis,
            deleteIntent = activityDismissIntent(context),
            tag = ONGOING_ACTIVITY_TAG,
        )
        if (timeoutAfterMillis > 0L && Build.VERSION.SDK_INT < Build.VERSION_CODES.O) {
            context
                .getSystemService(AlarmManager::class.java)
                ?.setAndAllowWhileIdle(
                    AlarmManager.RTC_WAKEUP,
                    now + timeoutAfterMillis,
                    activityExpiryIntent(context, displayExpiryAtMillis),
                )
        }
    }

    fun cancelActivity(context: Context) {
        context
            .getSystemService(NotificationManager::class.java)
            ?.cancel(ONGOING_ACTIVITY_TAG, AWARENESS_NOTIFICATION_ID)
        context
            .getSharedPreferences(ACTIVITY_DISPLAY_PREFERENCES, Context.MODE_PRIVATE)
            .edit()
            .remove(ACTIVITY_DISPLAY_EXPIRY_KEY)
            .apply()
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) {
            context.getSystemService(AlarmManager::class.java)?.cancel(activityExpiryIntent(context, null))
        }
    }

    internal fun isCurrentActivityExpiry(context: Context, expectedExpiryAtMillis: Long): Boolean {
        if (expectedExpiryAtMillis <= 0L) return false
        val preferences = context.getSharedPreferences(ACTIVITY_DISPLAY_PREFERENCES, Context.MODE_PRIVATE)
        return preferences.getLong(ACTIVITY_DISPLAY_EXPIRY_KEY, 0L) == expectedExpiryAtMillis &&
            expectedExpiryAtMillis <= System.currentTimeMillis()
    }

    /**
     * Posts either an alert or the ongoing card. The independent arguments preserve route, lifecycle, timeout, and
     * dismissal ownership.
     */
    @Suppress("LongParameterList")
    private fun post(
        context: Context,
        title: String,
        body: String,
        deepLink: String?,
        ongoing: Boolean,
        key: String,
        suppressVisible: Boolean,
        timeoutAfterMillis: Long = 0L,
        deleteIntent: PendingIntent? = null,
        tag: String? = null,
    ) {
        if (
            shouldSuppressVisibleNotification(
                deepLink = deepLink,
                suppressVisible = suppressVisible,
                visibleThread = visibleThread,
                appResumed = ProcessLifecycleOwner.get().lifecycle.currentState.isAtLeast(Lifecycle.State.RESUMED),
            ) || !notificationsEnabled(context)
        )
            return
        val manager = context.getSystemService(NotificationManager::class.java) ?: return
        ensureChannel(manager)
        val intent =
            Intent(context, MainActivity::class.java).apply {
                action = ACTION_OPEN_PUSH
                putExtra(EXTRA_PUSH_DEEP_LINK, deepLink)
                data = notificationIntentData(key)
                flags = Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP
            }
        val pending =
            PendingIntent.getActivity(
                context,
                AWARENESS_NOTIFICATION_ID,
                intent,
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
            )
        val notification =
            NotificationCompat.Builder(context, CHANNEL_ID)
                .setSmallIcon(R.drawable.bex_icon)
                .setContentTitle(title)
                .setContentText(body)
                .setStyle(NotificationCompat.BigTextStyle().bigText(body))
                .setAutoCancel(!ongoing)
                .setOngoing(ongoing)
                .setOnlyAlertOnce(ongoing)
                .setContentIntent(pending)
                .setPriority(NotificationCompat.PRIORITY_HIGH)
                .apply {
                    if (timeoutAfterMillis > 0L) setTimeoutAfter(timeoutAfterMillis)
                    if (deleteIntent != null) setDeleteIntent(deleteIntent)
                    if (ongoing && deleteIntent != null) addAction(0, "Dismiss", deleteIntent)
                }
                .build()
        manager.notify(tag ?: notificationTag(key, ongoing), AWARENESS_NOTIFICATION_ID, notification)
    }
}

private fun shouldSuppressVisibleNotification(
    deepLink: String?,
    suppressVisible: Boolean,
    visibleThread: String?,
    appResumed: Boolean,
): Boolean {
    if (!suppressVisible || deepLink == null) return false
    return deepLink == visibleThread && appResumed
}

private fun activityDismissIntent(context: Context): PendingIntent =
    PendingIntent.getBroadcast(
        context,
        AWARENESS_NOTIFICATION_ID,
        Intent(context, AgentActivityDismissReceiver::class.java),
        PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
    )

private fun activityExpiryIntent(context: Context, expiresAtMillis: Long?): PendingIntent =
    PendingIntent.getBroadcast(
        context,
        ACTIVITY_EXPIRY_REQUEST_CODE,
        Intent(context, AgentActivityExpiryReceiver::class.java).apply {
            expiresAtMillis?.let { putExtra(EXTRA_ACTIVITY_EXPIRY_AT, it) }
        },
        PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
    )

private fun ensureChannel(manager: NotificationManager) {
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O && manager.getNotificationChannel(CHANNEL_ID) == null) {
        manager.createNotificationChannel(
            NotificationChannel(CHANNEL_ID, CHANNEL_NAME, NotificationManager.IMPORTANCE_HIGH)
        )
    }
}

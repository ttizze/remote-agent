package dev.remoteagent.mobile

import android.app.AlarmManager
import android.app.PendingIntent
import android.appwidget.AppWidgetManager
import android.appwidget.AppWidgetProvider
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.view.View
import android.widget.RemoteViews
import dev.remoteagent.core.subscriptionWidgetEntryJson
import org.json.JSONObject

private const val WIDGET_STORAGE = "subscription-widget"
private const val WIDGET_KEY = "timeline"
private const val WIDGET_EXPIRY = "dev.remoteagent.mobile.SUBSCRIPTION_EXPIRY"
private const val WIDGET_MAX_BYTES = 256 * 1024

internal class UsageWidgetPublisher(private val context: Context) {
    private var published: String? = null

    fun publish(json: String) {
        if (json == published) return
        context.getSharedPreferences(WIDGET_STORAGE, Context.MODE_PRIVATE).edit().putString(WIDGET_KEY, json).apply()
        published = json
        SubscriptionUsageWidget.update(context)
    }
}

class SubscriptionUsageWidget : AppWidgetProvider() {
    override fun onUpdate(context: Context, manager: AppWidgetManager, ids: IntArray) = update(context)

    override fun onReceive(context: Context, intent: Intent) {
        super.onReceive(context, intent)
        when (intent.action) {
            WIDGET_EXPIRY, Intent.ACTION_BOOT_COMPLETED, Intent.ACTION_TIME_CHANGED,
            Intent.ACTION_TIMEZONE_CHANGED -> update(context)
        }
    }

    override fun onDisabled(context: Context) {
        context.getSystemService(AlarmManager::class.java).cancel(expiryIntent(context))
    }

    companion object {
        internal fun update(context: Context) {
            val manager = AppWidgetManager.getInstance(context)
            val ids = manager.getAppWidgetIds(ComponentName(context, SubscriptionUsageWidget::class.java))
            if (ids.isEmpty()) return
            val now = System.currentTimeMillis()
            val stored = context.getSharedPreferences(WIDGET_STORAGE, Context.MODE_PRIVATE).getString(WIDGET_KEY, null)
                ?.takeIf { it.length <= WIDGET_MAX_BYTES && it.toByteArray(Charsets.UTF_8).size <= WIDGET_MAX_BYTES }
                ?: "{\"entries\":[]}"
            val entry = JSONObject(subscriptionWidgetEntryJson(stored, now, "android", "auto", "auto"))
            val views = RemoteViews(context.packageName, R.layout.subscription_usage_widget)
            views.removeAllViews(R.id.usage_providers)
            val providers = entry.getJSONArray("providers")
            var deadline: Long? = null
            for (index in 0 until providers.length()) {
                val provider = providers.getJSONObject(index)
                val expiry = provider.getLong("expiresAt")
                if (expiry > now && (deadline == null || expiry < deadline)) deadline = expiry
                val row = RemoteViews(context.packageName, R.layout.subscription_usage_provider)
                row.setTextViewText(R.id.usage_provider_name, provider.getString("name"))
                val windows = provider.getJSONArray("windows")
                row.setViewVisibility(R.id.usage_provider_detail, if (windows.length() == 0) View.VISIBLE else View.GONE)
                row.setTextViewText(R.id.usage_provider_detail, provider.getString("detail"))
                row.removeAllViews(R.id.usage_windows)
                for (windowIndex in 0 until windows.length()) {
                    val window = windows.getJSONObject(windowIndex)
                    val quota = RemoteViews(context.packageName, R.layout.subscription_usage_quota)
                    val remaining = window.getInt("remaining")
                    quota.setTextViewText(R.id.usage_window_label, "${window.getString("label")} · $remaining% left")
                    quota.setProgressBar(R.id.usage_window_progress, 100, remaining, false)
                    if (remaining <= 10) quota.setTextColor(R.id.usage_window_label, android.graphics.Color.rgb(185, 28, 28))
                    val reset = window.optLong("resetAt", 0)
                    quota.setTextViewText(R.id.usage_window_reset, if (reset > 0) "Next reset ${formatDate(context, reset)}" else "Reset time unavailable")
                    row.addView(R.id.usage_windows, quota)
                }
                val hidden = provider.getInt("totalWindows") - windows.length()
                row.setViewVisibility(R.id.usage_hidden_windows, if (hidden > 0) View.VISIBLE else View.GONE)
                row.setTextViewText(R.id.usage_hidden_windows, "$hidden more in app")
                views.addView(R.id.usage_providers, row)
            }
            val checked = entry.getLong("checkedAt")
            views.setTextViewText(R.id.usage_checked, if (checked > 0) "As of ${formatDate(context, checked)}" else "Tap to connect")
            val open = Intent(context, MainActivity::class.java).putExtra("open_usage", true)
                .setAction(Intent.ACTION_VIEW)
                .setData(Uri.parse("remoteagent://settings/usage?tab=limits"))
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP)
            views.setOnClickPendingIntent(R.id.usage_widget_root, PendingIntent.getActivity(context, 0, open, PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE))
            manager.updateAppWidget(ids, views)
            val alarms = context.getSystemService(AlarmManager::class.java)
            alarms.cancel(expiryIntent(context))
            if (deadline != null) alarms.set(AlarmManager.RTC_WAKEUP, deadline, expiryIntent(context))
        }

        private fun expiryIntent(context: Context): PendingIntent = PendingIntent.getBroadcast(
            context, 0, Intent(context, SubscriptionUsageWidget::class.java).setAction(WIDGET_EXPIRY),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )

        private fun formatDate(context: Context, millis: Long): String {
            val date = java.util.Date(millis)
            return "${android.text.format.DateFormat.getMediumDateFormat(context).format(date)} ${android.text.format.DateFormat.getTimeFormat(context).format(date)}"
        }
    }
}

package dev.remoteagent.mobile

import android.net.Uri
import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage
import dev.remoteagent.core.agentActivityWidgetJson
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.put

internal data class ActivityPresentation(
    val title: String,
    val body: String,
    val active: Boolean,
    val deepLink: String?,
)

private const val ACTIVITY_STATE_PREFERENCES = "push-activity-state"
private const val ACTIVITY_MAX_BYTES = 64 * 1024
private const val ACTIVITY_MAX_HOSTS = 64

/**
 * Parses the core-owned display projection. Android does not choose priority,
 * colors, urgency, or rows; those decisions come from agent-core's shared
 * activity widget helper.
 */
internal fun parseActivityPresentation(value: String): ActivityPresentation? {
    if (value.toByteArray(Charsets.UTF_8).size > ACTIVITY_MAX_BYTES) return null
    val input = activityState(value) ?: return null
    val activeCount = (input["activeCount"] as? JsonPrimitive)?.content?.toIntOrNull()?.coerceAtLeast(0) ?: return null
    val projected = runCatching {
        agentActivityWidgetJson(value, stale = false, light = false, monochrome = false, reduced = false)
    }.getOrNull() ?: return null
    val display = runCatching { Json.parseToJsonElement(projected).jsonObject }.getOrNull() ?: return null
    val title = (display["headline"] as? JsonPrimitive)?.content?.trim()?.takeIf(String::isNotEmpty) ?: return null
    val rows = display["rows"]?.let { element ->
        (element as? JsonArray)?.mapNotNull { row ->
            val object = row as? JsonObject ?: return@mapNotNull null
            val project = (object["project"] as? JsonPrimitive)?.content?.trim().orEmpty()
            val thread = (object["title"] as? JsonPrimitive)?.content?.trim().orEmpty()
            val status = (object["status"] as? JsonPrimitive)?.content?.trim().orEmpty()
            listOf(project, thread, status).filter(String::isNotEmpty).joinToString(" · ").takeIf(String::isNotEmpty)
        }
    }.orEmpty()
    val body = rows.takeIf { it.isNotEmpty() }?.joinToString("\n")
        ?: (display["summary"] as? JsonPrimitive)?.content?.trim().orEmpty()
    val deepLink = (display["deepLink"] as? JsonPrimitive)?.content?.takeIf(String::isNotBlank)
    return ActivityPresentation(title, body, activeCount > 0, deepLink)
}

private fun activityState(value: String): JsonObject? {
    if (value.toByteArray(Charsets.UTF_8).size > ACTIVITY_MAX_BYTES) return null
    val root = runCatching { Json.parseToJsonElement(value).jsonObject }.getOrNull() ?: return null
    return runCatching {
        val activities = root["activities"] as? JsonArray ?: return@runCatching null
        if (activities.size > 64) return@runCatching null
        val activeCount = (root["activeCount"] as? JsonPrimitive)?.content?.toIntOrNull()?.coerceAtLeast(0)
            ?: return@runCatching null
        val title = (root["title"] as? JsonPrimitive)?.content?.trim().orEmpty()
        val subtitle = (root["subtitle"] as? JsonPrimitive)?.content?.trim().orEmpty()
        val updatedAt = (root["updatedAt"] as? JsonPrimitive)?.content?.trim().orEmpty()
        buildJsonObject {
            put("title", title)
            put("subtitle", subtitle)
            put("activeCount", activeCount)
            put("updatedAt", updatedAt)
            put("activities", activities)
        }
    }.getOrNull()
}

/** Stores one Host state and returns the serialized cross-Host aggregate. */
private fun mergeActivityState(context: android.content.Context, hostId: String, value: String): String? {
    if (hostId.isBlank() || hostId.toByteArray(Charsets.UTF_8).size > 256) return null
    val state = activityState(value) ?: return null
    val preferences = context.getSharedPreferences(ACTIVITY_STATE_PREFERENCES, android.content.Context.MODE_PRIVATE)
    val next = preferences.all
        .filterKeys { it.startsWith("host:") }
        .mapNotNull { (key, raw) ->
            val json = raw as? String ?: return@mapNotNull null
            val parsed = activityState(json) ?: return@mapNotNull null
            key.removePrefix("host:") to parsed
        }
        .toMutableMap()
    next[hostId] = state
    val retained = next.toList().sortedByDescending {
        (it.second["updatedAt"] as? JsonPrimitive)?.content.orEmpty()
    }
        .take(ACTIVITY_MAX_HOSTS)
    val editor = preferences.edit().clear()
    retained.forEach { (id, parsed) -> editor.putString("host:$id", Json.encodeToString(JsonObject.serializer(), parsed)) }
    editor.apply()

    val allRows = retained.flatMap { (_, parsed) -> (parsed["activities"] as? JsonArray).orEmpty() }.take(64)
    val activeCount = retained.sumOf { (_, parsed) ->
        (parsed["activeCount"] as? JsonPrimitive)?.content?.toIntOrNull()?.coerceAtLeast(0) ?: 0
    }
    val title = (retained.firstOrNull()?.second?.get("title") as? JsonPrimitive)?.content?.takeIf(String::isNotBlank)
        ?: "Agent activity"
    val subtitle = (retained.firstOrNull()?.second?.get("subtitle") as? JsonPrimitive)?.content.orEmpty()
    val updatedAt = retained.maxOfOrNull { (_, parsed) -> (parsed["updatedAt"] as? JsonPrimitive)?.content.orEmpty() }.orEmpty()
    val aggregate = buildJsonObject {
        put("title", title)
        put("subtitle", subtitle)
        put("activeCount", activeCount)
        put("updatedAt", updatedAt)
        put("activities", JsonArray(allRows))
    }
    return Json.encodeToString(JsonObject.serializer(), aggregate)
}

internal class AgentPushMessagingService : FirebaseMessagingService() {
    override fun onNewToken(token: String) {
        if (token.isBlank() || !FirebasePushBootstrap.ensure(this)) return
        PushRegistrationStore.saveToken(this, token)
        sendBroadcast(android.content.Intent(ACTION_PUSH_TOKEN_UPDATED))
    }

    override fun onMessageReceived(message: RemoteMessage) {
        if (!FirebasePushBootstrap.ensure(this)) return
        val data = message.data
        val deepLink = data["deepLink"]?.takeIf(String::isNotBlank)
            ?: threadDeepLink(data["environmentId"], data["threadId"])
        val title = message.notification?.title ?: data["alertTitle"] ?: data["headline"] ?: "Agent activity"
        val body = message.notification?.body ?: data["alertBody"] ?: data["detail"] ?: data["threadTitle"] ?: "Agent update"
        val hostId = data["environmentId"]?.takeIf(String::isNotBlank)
        val activityJson = data["activity"]
        val aggregate = if (hostId != null && activityJson != null) {
            mergeActivityState(this, hostId, activityJson)
        } else null
        val activity = aggregate?.let(::parseActivityPresentation)
        if (activity == null) {
            PushNotificationCenter.show(this, title, body, deepLink)
        } else {
            val activityDeepLink = activity.deepLink ?: deepLink
            PushNotificationCenter.showActivity(
                this,
                activity.title,
                activity.body,
                activityDeepLink,
                activity.active,
            )
            if (data["alert"] == "1") {
                val alertKey = listOfNotNull(deepLink, data["phase"], data["updatedAt"])
                    .joinToString(":")
                    .ifEmpty { "alert:$title:$body" }
                PushNotificationCenter.show(this, title, body, deepLink, alertKey)
            }
        }
    }

    private fun threadDeepLink(environment: String?, thread: String?): String? =
        if (environment.isNullOrBlank() || thread.isNullOrBlank()) null
        else "remoteagent://threads/${UriComponent.encode(environment)}/${UriComponent.encode(thread)}"
}

private object UriComponent {
    fun encode(value: String): String =
        java.net.URLEncoder.encode(value, Charsets.UTF_8.name()).replace("+", "%20")
}

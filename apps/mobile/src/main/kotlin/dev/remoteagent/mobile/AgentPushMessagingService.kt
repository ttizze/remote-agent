package dev.remoteagent.mobile

import android.net.Uri
import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage
import dev.remoteagent.core.aggregateAgentActivityContentStatesJson
import dev.remoteagent.core.agentActivityWidgetJson
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.put
import java.time.Instant

internal data class ActivityPresentation(
    val title: String,
    val body: String,
    val active: Boolean,
    val deepLink: String?,
)

private const val ACTIVITY_STATE_PREFERENCES = "push-activity-state"
private const val ACTIVITY_MAX_BYTES = 64 * 1024
private const val ACTIVITY_MAX_HOSTS = 64
internal const val ACTIVITY_STALE_AFTER_MILLIS = 10 * 60 * 1000L

private const val ACTIVITY_HOST_PREFIX = "host:"
private const val ACTIVITY_BLOCKED_PREFIX = "blocked:"
private val ACTIVITY_STATE_LOCK = Any()

internal enum class ActivityStateMergeDisposition {
    Accepted,
    Ignored,
    Blocked,
}

internal data class ActivityStateMerge(
    val disposition: ActivityStateMergeDisposition,
    val states: Map<String, JsonObject>,
)

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

internal fun activityUpdatedAtMillis(state: JsonObject): Long? =
    (state["updatedAt"] as? JsonPrimitive)?.content?.trim()?.takeIf(String::isNotEmpty)?.let { value ->
        runCatching { Instant.parse(value).toEpochMilli() }.getOrNull()
    }

internal fun activityStateIsFresh(state: JsonObject, nowMillis: Long): Boolean =
    activityUpdatedAtMillis(state)?.let { it >= nowMillis - ACTIVITY_STALE_AFTER_MILLIS } == true

private fun storedActivityStates(
    preferences: android.content.SharedPreferences,
): MutableMap<String, JsonObject> = preferences.all
    .filterKeys { it.startsWith(ACTIVITY_HOST_PREFIX) }
    .mapNotNull { (key, raw) ->
        val json = raw as? String ?: return@mapNotNull null
        val parsed = activityState(json) ?: return@mapNotNull null
        key.removePrefix(ACTIVITY_HOST_PREFIX) to parsed
    }
    .toMutableMap()

internal fun retainedActivityStates(
    states: Map<String, JsonObject>,
    nowMillis: Long = System.currentTimeMillis(),
): List<Pair<String, JsonObject>> =
    states.asSequence()
        .filter { (_, state) -> activityStateIsFresh(state, nowMillis) }
        .sortedWith(
            compareByDescending<Pair<String, JsonObject>> { (_, state) ->
                activityUpdatedAtMillis(state) ?: Long.MIN_VALUE
            }
                .thenBy { (hostId, _) -> hostId },
        )
        .take(ACTIVITY_MAX_HOSTS)
        .toList()

internal fun mergeActivityStates(
    states: Map<String, JsonObject>,
    hostId: String,
    incoming: JsonObject,
    nowMillis: Long,
    deliveryAllowed: Boolean,
): ActivityStateMerge {
    val retained = retainedActivityStates(states, nowMillis).toMap().toMutableMap()
    if (!deliveryAllowed) {
        retained.remove(hostId)
        return ActivityStateMerge(ActivityStateMergeDisposition.Blocked, retained)
    }
    if (!activityStateIsFresh(incoming, nowMillis)) {
        return ActivityStateMerge(ActivityStateMergeDisposition.Ignored, retained)
    }
    val previous = retained[hostId]
    val previousUpdatedAt = previous?.let(::activityUpdatedAtMillis)
    val incomingUpdatedAt = activityUpdatedAtMillis(incoming)
    if (previousUpdatedAt != null && incomingUpdatedAt != null && incomingUpdatedAt < previousUpdatedAt) {
        return ActivityStateMerge(ActivityStateMergeDisposition.Ignored, retained)
    }
    retained[hostId] = incoming
    return ActivityStateMerge(ActivityStateMergeDisposition.Accepted, retained)
}

private fun aggregateActivityState(states: List<Pair<String, JsonObject>>): String? {
    if (states.isEmpty()) return null
    val input = Json.encodeToString(
        JsonArray.serializer(),
        JsonArray(states.map { (_, parsed) -> parsed }),
    )
    return aggregateAgentActivityContentStatesJson(input)
}

private fun blockedKey(hostId: String): String = "$ACTIVITY_BLOCKED_PREFIX$hostId"

private fun writeActivityStates(
    preferences: android.content.SharedPreferences,
    states: Map<String, JsonObject>,
    nowMillis: Long = System.currentTimeMillis(),
) {
    val editor = preferences.edit()
    preferences.all.keys.filter { it.startsWith(ACTIVITY_HOST_PREFIX) }.forEach { key -> editor.remove(key) }
    retainedActivityStates(states, nowMillis).forEach { (id, parsed) ->
        editor.putString("$ACTIVITY_HOST_PREFIX$id", Json.encodeToString(JsonObject.serializer(), parsed))
    }
    editor.apply()
}

private fun activityDeliveryAllowed(
    context: android.content.Context,
    preferences: android.content.SharedPreferences,
    hostId: String,
): Boolean =
    !preferences.getBoolean(blockedKey(hostId), false) &&
        AndroidMobileRepository(context).profiles().any { it.id == hostId }

private fun activityAggregateLocked(
    context: android.content.Context,
    preferences: android.content.SharedPreferences,
    nowMillis: Long,
): String? {
    val profiles = AndroidMobileRepository(context).profiles().mapTo(HashSet()) { it.id }
    val states = storedActivityStates(preferences)
        .filterKeys { it in profiles && !preferences.getBoolean(blockedKey(it), false) }
    val retained = retainedActivityStates(states, nowMillis)
    if (retained.size != states.size) {
        writeActivityStates(preferences, retained.toMap(), nowMillis)
    }
    return aggregateActivityState(retained)
}

/** Stores one Host state and returns the serialized cross-Host aggregate. */
private fun mergeActivityState(
    context: android.content.Context,
    hostId: String,
    value: String,
): ActivityStateMerge? {
    if (hostId.isBlank() || hostId.toByteArray(Charsets.UTF_8).size > 256) return null
    val state = activityState(value) ?: return null
    synchronized(ACTIVITY_STATE_LOCK) {
        val preferences = context.getSharedPreferences(ACTIVITY_STATE_PREFERENCES, android.content.Context.MODE_PRIVATE)
        val nowMillis = System.currentTimeMillis()
        val result = mergeActivityStates(
            states = storedActivityStates(preferences),
            hostId = hostId,
            incoming = state,
            nowMillis = nowMillis,
            deliveryAllowed = activityDeliveryAllowed(context, preferences, hostId),
        )
        if (result.disposition == ActivityStateMergeDisposition.Blocked) {
            preferences.edit().putBoolean(blockedKey(hostId), true).apply()
        }
        writeActivityStates(preferences, result.states, nowMillis)
        result.copy(states = result.states)
    }
}

/** Enables or disables activity delivery for one Host and returns the aggregate. */
internal fun setActivityDeliveryEnabled(
    context: android.content.Context,
    hostId: String,
    enabled: Boolean,
): String? {
    if (hostId.isBlank() || hostId.toByteArray(Charsets.UTF_8).size > 256) return null
    synchronized(ACTIVITY_STATE_LOCK) {
        val preferences = context.getSharedPreferences(ACTIVITY_STATE_PREFERENCES, android.content.Context.MODE_PRIVATE)
        val editor = preferences.edit()
        if (enabled) {
            editor.remove(blockedKey(hostId))
        } else {
            editor.remove("$ACTIVITY_HOST_PREFIX$hostId").putBoolean(blockedKey(hostId), true)
        }
        editor.apply()
        return activityAggregateLocked(context, preferences, System.currentTimeMillis())
    }
}

/** Drops a disabled Host and returns the remaining aggregate, if any. */
internal fun removeActivityState(context: android.content.Context, hostId: String): String? {
    return setActivityDeliveryEnabled(context, hostId, enabled = false)
}

internal fun currentActivityAggregate(context: android.content.Context): String? = synchronized(ACTIVITY_STATE_LOCK) {
    val preferences = context.getSharedPreferences(ACTIVITY_STATE_PREFERENCES, android.content.Context.MODE_PRIVATE)
    activityAggregateLocked(context, preferences, System.currentTimeMillis())
}

internal fun renderActivityAggregate(context: android.content.Context, aggregate: String?) {
    val presentation = aggregate?.let(::parseActivityPresentation)
    if (presentation == null) {
        PushNotificationCenter.cancelActivity(context)
    } else {
        PushNotificationCenter.showActivity(
            context,
            presentation.title,
            presentation.body,
            presentation.deepLink,
            presentation.active,
        )
    }
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
        val merge = if (hostId != null && activityJson != null) {
            mergeActivityState(this, hostId, activityJson)
        } else null
        if (merge != null && merge.disposition != ActivityStateMergeDisposition.Accepted) {
            renderActivityAggregate(this, aggregateActivityState(retainedActivityStates(merge.states)))
            return
        }
        val aggregate = merge?.let { aggregateActivityState(retainedActivityStates(it.states)) }
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

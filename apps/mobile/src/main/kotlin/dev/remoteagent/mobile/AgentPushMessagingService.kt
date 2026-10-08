// This file owns the complete FCM/activity state machine. Its small pure
// policy functions stay beside the Android callback so their shared storage
// and lifecycle callers cannot drift into separate, duplicate implementations.
@file:Suppress("TooManyFunctions")

package dev.remoteagent.mobile

import android.content.Context
import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage
import dev.remoteagent.core.agentActivityDeliveryDecision
import dev.remoteagent.core.agentActivityExpiryIsDue
import dev.remoteagent.core.agentActivityMessageIsFresh
import dev.remoteagent.core.agentActivityNotificationDeepLink
import dev.remoteagent.core.agentActivityTimestampMillis
import dev.remoteagent.core.agentActivityWidgetJson
import dev.remoteagent.core.aggregateAgentActivityContentStatesJson
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
private const val ACTIVITY_MAX_ROWS = 64
private const val ACTIVITY_MAX_HOSTS = 64
private const val MAX_HOST_ID_BYTES = 256

private const val ACTIVITY_HOST_PREFIX = "host:"
private const val ACTIVITY_EXPIRY_PREFIX = "expiry:"
private const val ACTIVITY_BLOCKED_PREFIX = "blocked:"
private const val ACTIVITY_DISMISSED_KEY = "dismissed"
private const val ACTIVITY_LAST_ACTIVE_KEY = "last-active"
private val ACTIVITY_STATE_LOCK = Any()

internal enum class ActivityStateMergeDisposition {
    Accepted,
    Ignored,
    Expired,
    Blocked,
}

internal data class ActivityStateMerge(
    val disposition: ActivityStateMergeDisposition,
    val states: Map<String, JsonObject>,
)

internal data class ActivityDeliveryResult(
    val decision: String,
    val aggregate: String?,
    val expiresAtMillis: Long,
    val active: Boolean,
)

internal data class ActivityAggregateSnapshot(val aggregate: String?, val expiresAtMillis: Long, val active: Boolean)

/**
 * Parses the core-owned display projection. Android does not choose priority, colors, urgency, or rows; those decisions
 * come from agent-core's shared activity widget helper.
 */
internal fun parseActivityPresentation(value: String): ActivityPresentation? =
    activityState(value)?.let { input ->
        val activeCount = (input["activeCount"] as? JsonPrimitive)?.content?.toIntOrNull()?.coerceAtLeast(0)
        val projected =
            runCatching {
                    agentActivityWidgetJson(value, stale = false, light = false, monochrome = false, reduced = false)
                }
                .getOrNull()
        val display = projected?.let { runCatching { Json.parseToJsonElement(it).jsonObject }.getOrNull() }
        val title = (display?.get("headline") as? JsonPrimitive)?.content?.trim()?.takeIf(String::isNotEmpty)
        if (activeCount == null || display == null || title == null) {
            null
        } else {
            val rows =
                (display["rows"] as? JsonArray)
                    ?.mapNotNull { row ->
                        (row as? JsonObject)?.let { rowObject ->
                            val project = (rowObject["project"] as? JsonPrimitive)?.content?.trim().orEmpty()
                            val thread = (rowObject["title"] as? JsonPrimitive)?.content?.trim().orEmpty()
                            val status = (rowObject["status"] as? JsonPrimitive)?.content?.trim().orEmpty()
                            listOf(project, thread, status)
                                .filter(String::isNotEmpty)
                                .joinToString(" · ")
                                .takeIf(String::isNotEmpty)
                        }
                    }
                    .orEmpty()
            val body =
                rows.takeIf { it.isNotEmpty() }?.joinToString("\n")
                    ?: (display["summary"] as? JsonPrimitive)?.content?.trim().orEmpty()
            val deepLink = (display["deepLink"] as? JsonPrimitive)?.content?.takeIf(String::isNotBlank)
            ActivityPresentation(title, body, activeCount > 0, deepLink)
        }
    }

private fun activityState(value: String): JsonObject? =
    value
        .takeIf { it.toByteArray(Charsets.UTF_8).size <= ACTIVITY_MAX_BYTES }
        ?.let { raw -> runCatching { Json.parseToJsonElement(raw).jsonObject }.getOrNull() }
        ?.let { root ->
            val activities = root["activities"] as? JsonArray
            val activeCount = (root["activeCount"] as? JsonPrimitive)?.content?.toIntOrNull()?.coerceAtLeast(0)
            if (activities == null || activeCount == null) {
                null
            } else if (activities.isEmpty() || activities.size > ACTIVITY_MAX_ROWS) {
                null
            } else {
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
            }
        }

internal fun activityUpdatedAtMillis(state: JsonObject): Long? =
    (state["updatedAt"] as? JsonPrimitive)?.content?.trim()?.takeIf(String::isNotEmpty)?.let { value ->
        agentActivityTimestampMillis(value).takeIf { it >= 0 }
    }

internal fun activityExpiryDue(expiresAtMillis: Long, nowMillis: Long, expectedExpiryAtMillis: Long?): Boolean =
    agentActivityExpiryIsDue(expiresAtMillis, nowMillis) &&
        (expectedExpiryAtMillis == null || expiresAtMillis <= expectedExpiryAtMillis)

internal fun activityLifecycleNeedsReset(expiredHosts: Boolean, aggregateActive: Boolean): Boolean =
    expiredHosts && !aggregateActive

private fun storedActivityStates(preferences: android.content.SharedPreferences): MutableMap<String, JsonObject> =
    preferences.all
        .filterKeys { it.startsWith(ACTIVITY_HOST_PREFIX) }
        .asSequence()
        .mapNotNull { (key, raw) ->
            val json = raw as? String ?: return@mapNotNull null
            val parsed = activityState(json) ?: return@mapNotNull null
            key.removePrefix(ACTIVITY_HOST_PREFIX) to parsed
        }
        .toMap()
        .toMutableMap()

private fun storedActivityExpiries(preferences: android.content.SharedPreferences): MutableMap<String, Long> =
    preferences.all
        .filterKeys { it.startsWith(ACTIVITY_EXPIRY_PREFIX) }
        .asSequence()
        .mapNotNull { (key, raw) ->
            val expiry = raw as? Long ?: return@mapNotNull null
            key.removePrefix(ACTIVITY_EXPIRY_PREFIX) to expiry
        }
        .toMap()
        .toMutableMap()

internal fun retainedActivityStates(
    states: Map<String, JsonObject>,
    expiryAtMillis: Map<String, Long> = emptyMap(),
    nowMillis: Long = System.currentTimeMillis(),
): List<Pair<String, JsonObject>> =
    states
        .asSequence()
        .map { (hostId, state) -> hostId to state }
        .filter { (hostId, _) -> expiryAtMillis[hostId]?.let { !activityExpiryDue(it, nowMillis, null) } ?: true }
        .sortedWith(
            compareByDescending<Pair<String, JsonObject>> { (_, state) ->
                    activityUpdatedAtMillis(state) ?: Long.MIN_VALUE
                }
                .thenBy { (hostId, _) -> hostId }
        )
        .take(ACTIVITY_MAX_HOSTS)
        .toList()

internal fun mergeActivityStates(
    states: Map<String, JsonObject>,
    hostId: String,
    incoming: JsonObject,
    nowMillis: Long,
    deliveryAllowed: Boolean,
    incomingExpiryAtMillis: Long = Long.MAX_VALUE,
    incomingDeliveryAtMillis: Long = nowMillis,
    expiryAtMillis: Map<String, Long> = emptyMap(),
): ActivityStateMerge {
    val retained = retainedActivityStates(states, expiryAtMillis, nowMillis).toMap().toMutableMap()
    val previous = retained[hostId]
    val previousUpdatedAt = previous?.let(::activityUpdatedAtMillis)
    val incomingUpdatedAt = activityUpdatedAtMillis(incoming)
    val disposition =
        when {
            !deliveryAllowed -> ActivityStateMergeDisposition.Blocked
            incomingUpdatedAt == null -> ActivityStateMergeDisposition.Ignored
            else ->
                when (
                    agentActivityDeliveryDecision(
                        deliveryUpdatedAtMs = incomingDeliveryAtMillis,
                        sourceUpdatedAtMs = incomingUpdatedAt,
                        expiryAtMs = incomingExpiryAtMillis,
                        nowMs = nowMillis,
                        previousSourceUpdatedAtMs = previousUpdatedAt ?: -1,
                        dismissed = false,
                        active = false,
                        previousActive = false,
                    )
                ) {
                    "expired" -> ActivityStateMergeDisposition.Expired
                    "ignore_stale",
                    "dismissed" -> ActivityStateMergeDisposition.Ignored
                    else -> ActivityStateMergeDisposition.Accepted
                }
        }
    when (disposition) {
        ActivityStateMergeDisposition.Blocked,
        ActivityStateMergeDisposition.Expired -> retained.remove(hostId)
        ActivityStateMergeDisposition.Accepted -> retained[hostId] = incoming
        ActivityStateMergeDisposition.Ignored -> Unit
    }
    return ActivityStateMerge(disposition, retained)
}

private fun aggregateActivityState(states: List<Pair<String, JsonObject>>): String? {
    if (states.isEmpty()) return null
    val input = Json.encodeToString(JsonArray.serializer(), JsonArray(states.map { (_, parsed) -> parsed }))
    return aggregateAgentActivityContentStatesJson(input)
}

private fun blockedKey(hostId: String): String = "$ACTIVITY_BLOCKED_PREFIX$hostId"

private fun writeActivityStates(
    preferences: android.content.SharedPreferences,
    states: Map<String, JsonObject>,
    expiryAtMillis: Map<String, Long> = emptyMap(),
    nowMillis: Long = System.currentTimeMillis(),
) {
    val editor = preferences.edit()
    preferences.all.keys.filter { it.startsWith(ACTIVITY_HOST_PREFIX) }.forEach { key -> editor.remove(key) }
    preferences.all.keys.filter { it.startsWith(ACTIVITY_EXPIRY_PREFIX) }.forEach { key -> editor.remove(key) }
    retainedActivityStates(states, expiryAtMillis, nowMillis).forEach { (id, parsed) ->
        editor.putString("$ACTIVITY_HOST_PREFIX$id", Json.encodeToString(JsonObject.serializer(), parsed))
        expiryAtMillis[id]?.let { editor.putLong("$ACTIVITY_EXPIRY_PREFIX$id", it) }
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
    val expiryAtMillis = storedActivityExpiries(preferences)
    val states =
        storedActivityStates(preferences).filterKeys {
            it in profiles && !preferences.getBoolean(blockedKey(it), false)
        }
    val retained = retainedActivityStates(states, expiryAtMillis, nowMillis)
    if (retained.size != states.size || expiryAtMillis.keys != states.keys) {
        val retainedIds = retained.mapTo(HashSet()) { (hostId, _) -> hostId }
        writeActivityStates(preferences, retained.toMap(), expiryAtMillis.filterKeys { it in retainedIds }, nowMillis)
    }
    return aggregateActivityState(retained)
}

private fun aggregateActive(aggregate: String?): Boolean =
    aggregate?.let { value ->
        val state = activityState(value) ?: return@let false
        (state["activeCount"] as? JsonPrimitive)?.content?.toIntOrNull()?.let { it > 0 } == true
    } == true

private fun activityAggregateSnapshotLocked(
    context: android.content.Context,
    preferences: android.content.SharedPreferences,
    nowMillis: Long,
): ActivityAggregateSnapshot {
    val aggregate = activityAggregateLocked(context, preferences, nowMillis)
    val expiresAtMillis = storedActivityExpiries(preferences).values.maxOrNull() ?: 0L
    return ActivityAggregateSnapshot(aggregate, expiresAtMillis, aggregateActive(aggregate))
}

/** Stores one Host state and resolves the core-owned card lifecycle policy. */
private fun mergeActivityState(
    context: android.content.Context,
    hostId: String,
    value: String,
    incomingExpiryAtMillis: Long,
    incomingDeliveryAtMillis: Long,
): ActivityDeliveryResult? {
    if (
        hostId.isBlank() || hostId.toByteArray(Charsets.UTF_8).size > MAX_HOST_ID_BYTES || incomingExpiryAtMillis <= 0L
    ) {
        return null
    }
    return activityState(value)?.let { state ->
        synchronized(ACTIVITY_STATE_LOCK) {
            mergeActivityStateLocked(context, hostId, state, incomingExpiryAtMillis, incomingDeliveryAtMillis)
        }
    }
}

private fun mergeActivityStateLocked(
    context: Context,
    hostId: String,
    state: JsonObject,
    incomingExpiryAtMillis: Long,
    incomingDeliveryAtMillis: Long,
): ActivityDeliveryResult {
    val preferences = context.getSharedPreferences(ACTIVITY_STATE_PREFERENCES, Context.MODE_PRIVATE)
    val nowMillis = System.currentTimeMillis()
    val expiryAtMillis = storedActivityExpiries(preferences)
    val storedStates = storedActivityStates(preferences)
    val currentStates = storedStates.filterKeys { !activityExpiryDue(expiryAtMillis[it] ?: 0L, nowMillis, null) }
    val previous = currentStates[hostId]
    val result =
        mergeActivityStates(
            states = currentStates,
            hostId = hostId,
            incoming = state,
            nowMillis = nowMillis,
            deliveryAllowed = activityDeliveryAllowed(context, preferences, hostId),
            incomingExpiryAtMillis = incomingExpiryAtMillis,
            incomingDeliveryAtMillis = incomingDeliveryAtMillis,
        )
    val nextExpiryAtMillis =
        persistActivityMerge(preferences, hostId, result, expiryAtMillis, incomingExpiryAtMillis, nowMillis)
    var snapshot = activityAggregateSnapshotLocked(context, preferences, nowMillis)
    val expiredBeforeMerge = storedStates.keys.any { it !in currentStates }
    val previousActive = previousActivityActive(preferences, currentStates)
    val previousActiveForDecision =
        resetActivityLifecycleIfNeeded(preferences, expiredBeforeMerge, snapshot.active, previousActive)
    val decision =
        activityDecisionForMerge(
            result = result,
            incomingUpdatedAt = activityUpdatedAtMillis(state),
            incomingDeliveryAtMillis = incomingDeliveryAtMillis,
            incomingExpiryAtMillis = incomingExpiryAtMillis,
            nowMillis = nowMillis,
            previousSourceUpdatedAtMillis = previous?.let(::activityUpdatedAtMillis) ?: -1,
            dismissed = preferences.getBoolean(ACTIVITY_DISMISSED_KEY, false),
            active = snapshot.active,
            previousActive = previousActiveForDecision,
        )
    if (decision == "expired") {
        val states = result.states.toMutableMap().also { it.remove(hostId) }
        val expiries = nextExpiryAtMillis.toMutableMap().also { it.remove(hostId) }
        writeActivityStates(preferences, states, expiries, nowMillis)
        snapshot = activityAggregateSnapshotLocked(context, preferences, nowMillis)
    }
    persistActivityLifecycleDecision(preferences, decision, result.disposition, snapshot.active)
    return ActivityDeliveryResult(decision, snapshot.aggregate, snapshot.expiresAtMillis, snapshot.active)
}

private fun previousActivityActive(
    preferences: android.content.SharedPreferences,
    currentStates: Map<String, JsonObject>,
): Boolean =
    if (preferences.contains(ACTIVITY_LAST_ACTIVE_KEY)) {
        preferences.getBoolean(ACTIVITY_LAST_ACTIVE_KEY, false)
    } else {
        currentStates.values.any { state ->
            (state["activeCount"] as? JsonPrimitive)?.content?.toIntOrNull()?.let { it > 0 } == true
        }
    }

/** Persists each independently owned Host state and its absolute expiry. */
@Suppress("LongParameterList")
private fun persistActivityMerge(
    preferences: android.content.SharedPreferences,
    hostId: String,
    result: ActivityStateMerge,
    expiryAtMillis: Map<String, Long>,
    incomingExpiryAtMillis: Long,
    nowMillis: Long,
): MutableMap<String, Long> {
    if (result.disposition == ActivityStateMergeDisposition.Blocked) {
        preferences.edit().putBoolean(blockedKey(hostId), true).apply()
    }
    val nextExpiryAtMillis = expiryAtMillis.filterKeys { it in result.states }.toMutableMap()
    if (result.disposition == ActivityStateMergeDisposition.Accepted) {
        nextExpiryAtMillis[hostId] = incomingExpiryAtMillis
    }
    writeActivityStates(preferences, result.states, nextExpiryAtMillis, nowMillis)
    return nextExpiryAtMillis
}

private fun resetActivityLifecycleIfNeeded(
    preferences: android.content.SharedPreferences,
    expiredBeforeMerge: Boolean,
    active: Boolean,
    previousActive: Boolean,
): Boolean =
    if (activityLifecycleNeedsReset(expiredBeforeMerge, active)) {
        preferences.edit().putBoolean(ACTIVITY_DISMISSED_KEY, false).putBoolean(ACTIVITY_LAST_ACTIVE_KEY, false).apply()
        false
    } else {
        previousActive
    }

/** Keeps delivery facts flat because the core decision has no mutable owner. */
@Suppress("LongParameterList")
private fun activityDecisionForMerge(
    result: ActivityStateMerge,
    incomingUpdatedAt: Long?,
    incomingDeliveryAtMillis: Long,
    incomingExpiryAtMillis: Long,
    nowMillis: Long,
    previousSourceUpdatedAtMillis: Long,
    dismissed: Boolean,
    active: Boolean,
    previousActive: Boolean,
): String =
    when (result.disposition) {
        ActivityStateMergeDisposition.Blocked -> "blocked"
        ActivityStateMergeDisposition.Ignored -> "ignore_stale"
        ActivityStateMergeDisposition.Expired -> "expired"
        ActivityStateMergeDisposition.Accepted ->
            agentActivityDeliveryDecision(
                deliveryUpdatedAtMs = incomingDeliveryAtMillis,
                sourceUpdatedAtMs = incomingUpdatedAt ?: -1,
                expiryAtMs = incomingExpiryAtMillis,
                nowMs = nowMillis,
                previousSourceUpdatedAtMs = previousSourceUpdatedAtMillis,
                dismissed = dismissed,
                active = active,
                previousActive = previousActive,
            )
    }

private fun persistActivityLifecycleDecision(
    preferences: android.content.SharedPreferences,
    decision: String,
    disposition: ActivityStateMergeDisposition,
    active: Boolean,
) {
    when {
        decision == "expired" ->
            preferences
                .edit()
                .putBoolean(ACTIVITY_DISMISSED_KEY, false)
                .putBoolean(ACTIVITY_LAST_ACTIVE_KEY, false)
                .apply()
        decision == "accept" || decision == "rearmed" ->
            preferences
                .edit()
                .putBoolean(ACTIVITY_DISMISSED_KEY, false)
                .putBoolean(ACTIVITY_LAST_ACTIVE_KEY, active)
                .apply()
        disposition == ActivityStateMergeDisposition.Accepted ->
            preferences.edit().putBoolean(ACTIVITY_LAST_ACTIVE_KEY, active).apply()
    }
}

/** Removes one Host's typed empty aggregate without fabricating a source row. */
private fun clearActivityState(
    context: android.content.Context,
    hostId: String,
    deliveryAtMillis: Long,
    sourceUpdatedAtMillis: Long,
): ActivityDeliveryResult? {
    if (hostId.isBlank() || hostId.toByteArray(Charsets.UTF_8).size > MAX_HOST_ID_BYTES) return null
    return synchronized(ACTIVITY_STATE_LOCK) {
        val preferences = context.getSharedPreferences(ACTIVITY_STATE_PREFERENCES, android.content.Context.MODE_PRIVATE)
        val nowMillis = System.currentTimeMillis()
        if (!agentActivityMessageIsFresh(deliveryAtMillis, nowMillis)) {
            null
        } else {
            val states = storedActivityStates(preferences)
            val previous = states[hostId]
            if (previous != null && (activityUpdatedAtMillis(previous) ?: Long.MIN_VALUE) > sourceUpdatedAtMillis) {
                null
            } else {
                val expiries = storedActivityExpiries(preferences)
                val nextStates = states.toMutableMap().also { it.remove(hostId) }
                val nextExpiries = expiries.toMutableMap().also { it.remove(hostId) }
                writeActivityStates(preferences, nextStates, nextExpiries, nowMillis)
                val snapshot = activityAggregateSnapshotLocked(context, preferences, nowMillis)
                if (activityLifecycleNeedsReset(true, snapshot.active)) {
                    preferences
                        .edit()
                        .putBoolean(ACTIVITY_DISMISSED_KEY, false)
                        .putBoolean(ACTIVITY_LAST_ACTIVE_KEY, false)
                        .apply()
                }
                ActivityDeliveryResult(
                    decision = "cleared",
                    aggregate = snapshot.aggregate,
                    expiresAtMillis = snapshot.expiresAtMillis,
                    active = snapshot.active,
                )
            }
        }
    }
}

/** Enables or disables activity delivery for one Host and returns the aggregate. */
internal fun setActivityDeliveryEnabled(context: android.content.Context, hostId: String, enabled: Boolean): String? {
    if (hostId.isBlank() || hostId.toByteArray(Charsets.UTF_8).size > MAX_HOST_ID_BYTES) return null
    synchronized(ACTIVITY_STATE_LOCK) {
        val preferences = context.getSharedPreferences(ACTIVITY_STATE_PREFERENCES, android.content.Context.MODE_PRIVATE)
        val editor = preferences.edit()
        if (enabled) {
            editor.remove(blockedKey(hostId)).putBoolean(ACTIVITY_DISMISSED_KEY, false)
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

internal fun currentActivitySnapshot(context: android.content.Context): ActivityAggregateSnapshot =
    synchronized(ACTIVITY_STATE_LOCK) {
        val preferences = context.getSharedPreferences(ACTIVITY_STATE_PREFERENCES, android.content.Context.MODE_PRIVATE)
        activityAggregateSnapshotLocked(context, preferences, System.currentTimeMillis())
    }

internal fun dismissActivity(context: android.content.Context) {
    synchronized(ACTIVITY_STATE_LOCK) {
        context
            .getSharedPreferences(ACTIVITY_STATE_PREFERENCES, android.content.Context.MODE_PRIVATE)
            .edit()
            .putBoolean(ACTIVITY_DISMISSED_KEY, true)
            .apply()
    }
    PushNotificationCenter.cancelActivity(context)
}

internal fun expireActivity(context: android.content.Context, expectedExpiryAtMillis: Long?) {
    synchronized(ACTIVITY_STATE_LOCK) {
        if (
            expectedExpiryAtMillis != null &&
                !PushNotificationCenter.isCurrentActivityExpiry(context, expectedExpiryAtMillis)
        ) {
            return
        }
        val preferences = context.getSharedPreferences(ACTIVITY_STATE_PREFERENCES, android.content.Context.MODE_PRIVATE)
        val nowMillis = System.currentTimeMillis()
        val expiries = storedActivityExpiries(preferences)
        val expiredHosts =
            expiries.filter { (_, expiry) -> activityExpiryDue(expiry, nowMillis, expectedExpiryAtMillis) }.keys
        if (expiredHosts.isNotEmpty()) {
            val states = storedActivityStates(preferences).toMutableMap()
            val nextExpiries = expiries.toMutableMap()
            expiredHosts.forEach {
                states.remove(it)
                nextExpiries.remove(it)
            }
            writeActivityStates(preferences, states, nextExpiries, nowMillis)
        }
        if (expiredHosts.isNotEmpty()) {
            val snapshot = activityAggregateSnapshotLocked(context, preferences, nowMillis)
            if (activityLifecycleNeedsReset(true, snapshot.active)) {
                preferences
                    .edit()
                    .putBoolean(ACTIVITY_DISMISSED_KEY, false)
                    .putBoolean(ACTIVITY_LAST_ACTIVE_KEY, false)
                    .apply()
            }
            renderActivitySnapshot(context, snapshot)
        } else {
            // On pre-O devices the display timeout is bounded to 24 hours
            // while the Host's source expiry can be later. The alarm expires
            // the OS card only; a newer run is protected by the expected
            // expiry check above.
            PushNotificationCenter.cancelActivity(context)
        }
    }
}

internal fun renderActivityAggregate(context: android.content.Context, aggregate: String?) {
    val snapshot = currentActivitySnapshot(context)
    renderActivitySnapshot(context, snapshot.copy(aggregate = aggregate, active = aggregateActive(aggregate)))
}

internal fun renderActivitySnapshot(context: android.content.Context, snapshot: ActivityAggregateSnapshot) {
    synchronized(ACTIVITY_STATE_LOCK) {
        val presentation = snapshot.aggregate?.let(::parseActivityPresentation)
        if (presentation == null) {
            PushNotificationCenter.cancelActivity(context)
        } else {
            PushNotificationCenter.showActivity(
                context,
                presentation.title,
                presentation.body,
                presentation.deepLink,
                snapshot.active,
                snapshot.expiresAtMillis,
            )
        }
    }
}

internal class AgentActivityDismissReceiver : android.content.BroadcastReceiver() {
    override fun onReceive(context: android.content.Context, intent: android.content.Intent) {
        dismissActivity(context)
    }
}

internal class AgentActivityExpiryReceiver : android.content.BroadcastReceiver() {
    override fun onReceive(context: android.content.Context, intent: android.content.Intent) {
        val expectedExpiryAtMillis = intent.getLongExtra(EXTRA_ACTIVITY_EXPIRY_AT, Long.MAX_VALUE)
        expireActivity(context, expectedExpiryAtMillis)
    }
}

internal class AgentPushMessagingService : FirebaseMessagingService() {
    override fun onNewToken(token: String) {
        if (token.isBlank() || !FirebasePushBootstrap.ensure(this)) return
        PushRegistrationStore.saveToken(this, token)
        sendBroadcast(android.content.Intent(ACTION_PUSH_TOKEN_UPDATED))
    }

    // This platform callback owns payload parsing, Host-state delivery, and
    // alert routing; splitting those gates would duplicate the lifecycle.
    @Suppress("CyclomaticComplexMethod")
    override fun onMessageReceived(message: RemoteMessage) {
        if (!FirebasePushBootstrap.ensure(this)) return
        val data = message.data
        val deepLink = agentActivityNotificationDeepLink(data["deepLink"], data["environmentId"], data["threadId"])
        val title = message.notification?.title ?: data["alertTitle"] ?: data["headline"] ?: "Agent activity"
        val body =
            message.notification?.body ?: data["alertBody"] ?: data["detail"] ?: data["threadTitle"] ?: "Agent update"
        val alertRequested = data["alert"] == "1" || message.notification != null
        val hostId = data["environmentId"]?.takeIf(String::isNotBlank)
        val activityJson = data["activity"]
        val activityExpiryAtMillis = data["activity_expires_at"]?.toLongOrNull()
        val activityDeliveryAtMillis = data["updated_at"]?.toLongOrNull()
        val activityClearSourceAtMillis = data["activity_clear_source_at"]?.toLongOrNull()
        val activityClear = data["activity_clear"] == "1"
        val merge =
            when {
                hostId == null -> null
                activityClear ->
                    if (activityDeliveryAtMillis == null || activityClearSourceAtMillis == null) {
                        null
                    } else {
                        clearActivityState(this, hostId, activityDeliveryAtMillis, activityClearSourceAtMillis)
                    }
                activityJson == null || activityDeliveryAtMillis == null -> null
                else ->
                    activityExpiryAtMillis?.let { expiry ->
                        mergeActivityState(this, hostId, activityJson, expiry, activityDeliveryAtMillis)
                    }
            }
        merge
            ?.takeIf { it.decision != "dismissed" }
            ?.let { result ->
                renderActivitySnapshot(
                    this,
                    ActivityAggregateSnapshot(result.aggregate, result.expiresAtMillis, result.active),
                )
            }
        if (alertRequested) {
            val alertKey =
                data["alertId"]?.takeIf(String::isNotBlank)
                    ?: listOfNotNull(deepLink, data["phase"], data["updatedAt"]).joinToString(":").ifEmpty {
                        "alert:$title:$body"
                    }
            val alertDeepLink =
                if (data.containsKey("alertDeepLink")) {
                    agentActivityNotificationDeepLink(data["alertDeepLink"], null, null)
                } else {
                    deepLink
                }
            PushNotificationCenter.show(this, title, body, alertDeepLink, alertKey, deduplicate = true)
        }
    }
}

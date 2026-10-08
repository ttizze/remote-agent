package dev.remoteagent.mobile

import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.jsonObject

internal data class ActivityPresentation(
    val title: String,
    val body: String,
    val active: Boolean,
    val deepLink: String?,
)

private data class ActivityRow(
    val phase: String,
    val text: String,
    val deepLink: String?,
)

/** Pure presentation of the bounded aggregate used by the ongoing Android notification. */
internal fun parseActivityPresentation(value: String): ActivityPresentation? {
    if (value.length > 4_096) return null
    val root = runCatching { Json.parseToJsonElement(value).jsonObject }.getOrNull() ?: return null
    val title = (root["title"] as? JsonPrimitive)?.content?.trim()?.takeIf(String::isNotEmpty)
        ?: return null
    val subtitle = (root["subtitle"] as? JsonPrimitive)?.content?.trim().orEmpty()
    val activeCount = (root["activeCount"] as? JsonPrimitive)?.intOrNull?.coerceAtLeast(0) ?: 0
    val rows = (root["activities"] as? JsonArray)?.take(5).orEmpty().mapNotNull { row ->
        val fields = row as? JsonObject ?: return@mapNotNull null
        val phase = (fields["phase"] as? JsonPrimitive)?.content?.trim().orEmpty()
        val status = (fields["status"] as? JsonPrimitive)?.content?.trim().orEmpty()
        val thread = (fields["threadTitle"] as? JsonPrimitive)?.content?.trim().orEmpty()
        val project = (fields["projectTitle"] as? JsonPrimitive)?.content?.trim().orEmpty()
        val deepLink = (fields["deepLink"] as? JsonPrimitive)?.content?.trim()
            ?.takeIf(String::isNotEmpty)
        if (thread.isEmpty() && project.isEmpty()) null else {
            ActivityRow(
                phase,
                listOf(status.take(40), thread.take(120), project.take(120))
                    .filter(String::isNotEmpty)
                    .joinToString(" · "),
                deepLink,
            )
        }
    }
    val orderedRows = rows.sortedBy(::activityRowPriority)
    val attentionCount = orderedRows.count {
        it.phase == "waiting_for_approval" || it.phase == "waiting_for_input"
    }
    val failed = orderedRows.any { it.phase == "failed" }
    val activityTitle = if (activeCount > 0) {
        "$activeCount active agent${if (activeCount == 1) "" else "s"}" +
            if (attentionCount > 0) " · $attentionCount need${if (attentionCount == 1) "s" else ""} attention" else ""
    } else if (failed) {
        "Agent work failed"
    } else {
        "Agent work completed"
    }
    val body = orderedRows.takeIf { it.isNotEmpty() }?.joinToString("\n") { it.text } ?: subtitle
    return ActivityPresentation(
        activityTitle.take(120),
        body.take(512),
        activeCount > 0,
        orderedRows.firstOrNull()?.deepLink,
    )
}

private fun activityRowPriority(row: ActivityRow): Int =
    when (row.phase) {
        "waiting_for_approval", "waiting_for_input" -> 0
        "failed" -> 1
        "starting", "running" -> 2
        else -> 3
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
        val title =
            message.notification?.title ?: data["alertTitle"] ?: data["headline"] ?: "Agent activity"
        val body =
            message.notification?.body
                ?: data["alertBody"]
                ?: data["detail"]
                ?: data["threadTitle"]
                ?: "Agent update"
        val activity = data["activity"]?.let(::parseActivityPresentation)
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

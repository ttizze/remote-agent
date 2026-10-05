package dev.remoteagent.mobile

import dev.remoteagent.core.SessionRef
import dev.remoteagent.core.Snapshot
import org.json.JSONArray
import org.json.JSONObject

internal fun messageItem(id: String, text: String, user: Boolean = false): JSONObject {
    val body =
        if (user) {
            JSONObject()
                .put(
                    "userMessage",
                    JSONObject()
                        .put("content", JSONArray().put(JSONObject().put("text", JSONObject().put("text", text)))),
                )
        } else {
            JSONObject().put("assistantText", JSONObject().put("text", text).put("phase", "unknown"))
        }
    return JSONObject()
        .put("id", id)
        .put("status", "completed")
        .put("body", JSONObject().put("inline", JSONObject().put("body", body)))
}

internal fun Snapshot.threadJson(session: SessionRef): JSONObject {
    val entries = JSONObject(serialize().decodeToString()).getJSONArray("conversations")
    return (0 until entries.length()).firstNotNullOf { index ->
        val entry = entries.getJSONArray(index)
        val identity = entry.getJSONObject(0)
        if (identity.getString("id") == session.id) {
            entry.getJSONObject(1)
        } else null
    }
}

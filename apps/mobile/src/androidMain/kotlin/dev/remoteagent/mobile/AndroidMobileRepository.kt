package dev.remoteagent.mobile

import android.content.Context
import android.util.AtomicFile
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject

/** App-private, non-secret JSON persistence. Pairing tickets and device PKCS#8 material stay out. */
class AndroidMobileRepository(context: Context) : MobileRepository {
    private val file = AtomicFile(File(context.filesDir, "mobile-state.v1.json"))

    override fun load(): AppState = runCatching {
        val bytes = file.openRead().use { it.readBytes() }
        require(bytes.size <= MaximumStateBytes)
        JSONObject(bytes.decodeToString()).toAppState()
    }.getOrElse { AppState() }

    override fun save(state: AppState) {
        val bytes = state.toJson().toString().encodeToByteArray()
        require(bytes.size <= MaximumStateBytes) { "Mobile cache exceeds storage limit" }
        val output = file.startWrite()
        try {
            output.write(bytes)
            output.fd.sync()
            file.finishWrite(output)
        } catch (error: Throwable) {
            file.failWrite(output)
            throw error
        }
    }

    private fun AppState.toJson() = JSONObject().apply {
        put("version", 1)
        put("selectedProfileId", selectedProfileId)
        put("profiles", JSONArray().also { array -> profiles.forEach { array.put(it.toJson()) } })
        put("views", JSONObject().also { views ->
            profileViews.forEach { (id, view) ->
                views.put(
                    id,
                    JSONObject()
                        .put("workingDirectoryPath", view.workingDirectoryPath)
                        .put("selectedThreadId", view.selectedThreadId),
                )
            }
        })
        put("cache", JSONObject().also { caches ->
            cache.profiles.forEach { (id, profileCache) ->
                caches.put(id, profileCache.toJson())
            }
        })
    }

    private fun HostProfile.toJson() = JSONObject().apply {
        put("hostIdentity", hostIdentity)
        put("name", name)
        put("addresses", JSONArray(addresses))
        put("deviceIdentityReference", deviceIdentityReference)
    }

    private fun ThreadSummary.toJson() = JSONObject().apply {
        put("id", id); put("name", name); put("preview", preview)
        put("workingDirectory", workingDirectory.path)
        put("createdAtMs", createdAtMs); put("updatedAtMs", updatedAtMs)
        put("status", status.toJson())
        raw?.let { put("raw", JSONObject(it.toString())) }
    }

    private fun ProfileMobileCache.toJson() = JSONObject().apply {
        put("threadList", JSONArray().also { values -> threadList.forEach { values.put(it.toJson()) } })
        put("snapshots", JSONArray().also { values -> snapshots.values.forEach { values.put(it.toJson()) } })
        put("unknownEvents", JSONArray().also { values -> unknownEvents.forEach { values.put(it.toJson()) } })
    }

    private fun ThreadSnapshot.toJson() = JSONObject().apply {
        put("summary", summary.toJson())
        put("turns", JSONArray().also { values -> turns.forEach { values.put(it.toJson()) } })
        raw?.let { put("raw", JSONObject(it.toString())) }
    }

    private fun CodexTurn.toJson() = JSONObject().apply {
        put("id", id); put("status", status.name)
        put("items", JSONArray().also { values -> items.forEach { values.put(it.toJson()) } })
        raw?.let { put("raw", JSONObject(it.toString())) }
    }

    private fun CodexItem.toJson(): JSONObject = when (this) {
        is CodexItem.UserMessage -> JSONObject().put("type", "userMessage").put("id", id).put("text", text)
        is CodexItem.AgentMessage -> JSONObject().put("type", "agentMessage").put("id", id).put("text", text)
        is CodexItem.Reasoning -> JSONObject().put("type", "reasoning").put("id", id).put("summary", summary)
        is CodexItem.CommandExecution -> JSONObject().apply {
            put("type", "commandExecution"); put("id", id); put("command", command); put("cwd", cwd)
            put("output", output); put("status", status.name); put("exitCode", exitCode)
        }
        is CodexItem.FileChange -> JSONObject().apply {
            put("type", "fileChange"); put("id", id); put("status", status.name)
            put("changes", JSONArray().also { values -> changes.forEach { change ->
                values.put(JSONObject().put("path", change.path).put("kind", change.kind.name).put("diff", change.diff))
            } })
        }
        is CodexItem.Unknown -> JSONObject(raw.toString()).apply {
            put("type", codexType)
            put("id", id)
        }
    }

    private fun ThreadStatus.toJson(): JSONObject = when (this) {
        ThreadStatus.NotLoaded -> JSONObject().put("type", "notLoaded")
        ThreadStatus.Idle -> JSONObject().put("type", "idle")
        ThreadStatus.SystemError -> JSONObject().put("type", "systemError")
        is ThreadStatus.Active -> JSONObject().put("type", "active").put("activeFlags", JSONArray(activeFlags))
    }

    private fun JSONObject.toAppState(): AppState {
        require(optInt("version") == 1)
        val profiles = optJSONArray("profiles").orEmpty().mapNotNull { it as? JSONObject }.mapNotNull { json ->
            val hostIdentity = json.optString("hostIdentity")
            val reference = json.optString("deviceIdentityReference")
            if (hostIdentity.isBlank() || reference.isBlank()) null else HostProfile(
                hostIdentity = hostIdentity,
                name = json.optString("name", hostIdentity),
                addresses = json.optJSONArray("addresses").orEmpty().mapNotNull { it as? String }.filter { it.isNotBlank() },
                deviceIdentityReference = reference,
            )
        }.distinctBy { it.hostIdentity }
        val profileIds = profiles.mapTo(mutableSetOf()) { it.hostIdentity }
        val viewsJson = optJSONObject("views") ?: JSONObject()
        val views = profileIds.associateWith { id ->
            val view = viewsJson.optJSONObject(id)
            ProfileViewState(
                workingDirectoryPath = view?.optString("workingDirectoryPath").orEmpty(),
                selectedThreadId = view?.optString("selectedThreadId")?.takeIf { it.isNotBlank() },
            )
        }
        val cacheJson = optJSONObject("cache") ?: JSONObject()
        val cache = MobileCache(profileIds.associateWith { id -> cacheJson.optJSONObject(id).toProfileMobileCache() })
        return AppState(
            profiles = profiles,
            selectedProfileId = optString("selectedProfileId").takeIf { it in profileIds },
            profileViews = views,
            cache = cache,
        )
    }

    private fun JSONObject.toThreadSummary(): ThreadSummary? = runCatching {
        val id = getString("id")
        require(id.isNotBlank())
        ThreadSummary(
            id = id,
            name = optString("name").takeIf { it.isNotBlank() },
            preview = optString("preview"),
            workingDirectory = WorkingDirectory(optString("workingDirectory")),
            createdAtMs = optLong("createdAtMs"),
            updatedAtMs = optLong("updatedAtMs"),
            status = optJSONObject("status").toThreadStatus(),
            raw = optJSONObject("raw")?.toJsonObject(),
        )
    }.getOrNull()

    private fun JSONObject?.toProfileMobileCache(): ProfileMobileCache {
        val json = this ?: return ProfileMobileCache()
        return ProfileMobileCache(
            threadList = json.optJSONArray("threadList").orEmpty().mapNotNull { it as? JSONObject }.mapNotNull(JSONObject::toThreadSummary),
            snapshots = json.optJSONArray("snapshots").orEmpty().mapNotNull { it as? JSONObject }.mapNotNull(JSONObject::toThreadSnapshot)
                .associateBy { it.summary.id },
            unknownEvents = json.optJSONArray("unknownEvents").orEmpty().mapNotNull { it as? JSONObject }.mapNotNull(JSONObject::toUnknownEvent),
        )
    }

    private fun JSONObject.toThreadSnapshot(): ThreadSnapshot? = runCatching {
        val summary = getJSONObject("summary").toThreadSummary() ?: error("Invalid snapshot")
        ThreadSnapshot(
            summary = summary,
            turns = optJSONArray("turns").orEmpty().mapNotNull { it as? JSONObject }.mapNotNull(JSONObject::toCodexTurn),
            raw = optJSONObject("raw")?.toJsonObject(),
        )
    }.getOrNull()

    private fun JSONObject.toCodexTurn(): CodexTurn? = runCatching {
        val id = getString("id"); require(id.isNotBlank())
        CodexTurn(
            id = id,
            status = enumValueOf(optString("status", TurnStatus.Failed.name)),
            items = optJSONArray("items").orEmpty().mapNotNull { it as? JSONObject }.mapNotNull(JSONObject::toCodexItem),
            raw = optJSONObject("raw")?.toJsonObject(),
        )
    }.getOrNull()

    private fun JSONObject.toCodexItem(): CodexItem? = runCatching {
        val id = optString("id")
        val type = optString("type", "unknown")
        when (type) {
            "userMessage" -> CodexItem.UserMessage(id, getString("text"))
            "agentMessage" -> CodexItem.AgentMessage(id, getString("text"))
            "reasoning" -> CodexItem.Reasoning(id, getString("summary"))
            "commandExecution" -> CodexItem.CommandExecution(
                id = id, command = getString("command"), cwd = optString("cwd").takeIf { it.isNotBlank() },
                output = optString("output"), status = enumValueOf(optString("status", CommandExecutionStatus.Failed.name)),
                exitCode = if (has("exitCode") && !isNull("exitCode")) getInt("exitCode") else null,
            )
            "fileChange" -> CodexItem.FileChange(
                id = id,
                changes = optJSONArray("changes").orEmpty().mapNotNull { it as? JSONObject }.map { change ->
                    FileUpdateChange(change.getString("path"), enumValueOf(change.getString("kind")), change.optString("diff"))
                },
                status = enumValueOf(optString("status", FileChangeStatus.Failed.name)),
            )
            else -> CodexItem.Unknown(id, type, toJsonObject())
        }
    }.getOrNull()

    private fun ThreadEvent.Unknown.toJson() = JSONObject().apply {
        put("threadId", threadId)
        put("turnId", turnId)
        put("method", method)
        put("raw", JSONObject(raw.toString()))
    }

    private fun JSONObject.toUnknownEvent(): ThreadEvent.Unknown? = runCatching {
        ThreadEvent.Unknown(
            threadId = optString("threadId"),
            turnId = optString("turnId"),
            method = optString("method", "unknown"),
            raw = optJSONObject("raw")?.toJsonObject() ?: toJsonObject(),
        )
    }.getOrNull()

    private fun JSONObject.toJsonObject(): JsonObject =
        Json.parseToJsonElement(toString()).jsonObject

    private fun JSONObject?.toThreadStatus(): ThreadStatus = when (this?.optString("type")) {
        "notLoaded" -> ThreadStatus.NotLoaded
        "systemError" -> ThreadStatus.SystemError
        "active" -> ThreadStatus.Active(optJSONArray("activeFlags").orEmpty().mapNotNull { it as? String })
        else -> ThreadStatus.Idle
    }

    private fun JSONArray?.orEmpty(): List<Any> = buildList {
        if (this@orEmpty != null) for (index in 0 until length()) add(opt(index))
    }

    private companion object { const val MaximumStateBytes = 1024 * 1024 }
}

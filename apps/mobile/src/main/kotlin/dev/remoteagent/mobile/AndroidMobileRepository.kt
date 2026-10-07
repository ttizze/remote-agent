package dev.remoteagent.mobile

import android.content.Context
import android.util.AtomicFile
import java.io.File
import java.util.Base64
import kotlinx.serialization.Serializable
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json

@Serializable internal data class HostProfile(val id: String, val name: String, val ticket: String)

/** Platform persistence owns profile metadata and opaque core snapshots. */
internal class AndroidMobileRepository(context: Context) {
    private val directory = context.filesDir
    private val preferences = context.getSharedPreferences("agent-hosts", Context.MODE_PRIVATE)
    var selected: String?
        get() = preferences.getString("selected", null)
        set(value) {
            preferences.edit().putString("selected", value).apply()
        }

    fun profiles(): List<HostProfile> =
        preferences.getString("profiles", null)?.let { Json.decodeFromString(it) } ?: emptyList()

    fun saveProfiles(profiles: List<HostProfile>) {
        preferences.edit().putString("profiles", Json.encodeToString(profiles)).apply()
    }

    fun diagnosticsDirectory(id: String): String =
        File(directory, "connection-diagnostics/${encodedId(id)}").absolutePath

    /** The shell and thread snapshots core keeps for a warm start. */
    fun cacheDirectory(id: String): String = File(directory, "conversation-cache/${encodedId(id)}").absolutePath

    fun modelPreferences(): ByteArray =
        preferences.getString("orchestration-model-defaults", null)?.let { Base64.getDecoder().decode(it) }
            ?: byteArrayOf()

    fun saveModelPreferences(bytes: ByteArray) {
        preferences.edit().putString("orchestration-model-defaults", Base64.getEncoder().encodeToString(bytes)).apply()
    }

    fun load(id: String): ByteArray {
        val file = snapshotFile(id)
        return if (file.baseFile.exists()) file.openRead().use { it.readBytes() } else byteArrayOf()
    }

    fun save(id: String, bytes: ByteArray) = snapshotFile(id).writeSynced(bytes)

    private fun snapshotFile(id: String) = AtomicFile(File(directory, "orchestration-${encodedId(id)}.json"))

    private fun encodedId(id: String): String =
        Base64.getUrlEncoder().withoutPadding().encodeToString(id.encodeToByteArray())
}

/** Replaces the file only after the new bytes reach storage. */
internal fun AtomicFile.writeSynced(bytes: ByteArray) {
    val output = startWrite()
    var committed = false
    try {
        output.write(bytes)
        output.fd.sync()
        finishWrite(output)
        committed = true
    } finally {
        if (!committed) failWrite(output)
    }
}

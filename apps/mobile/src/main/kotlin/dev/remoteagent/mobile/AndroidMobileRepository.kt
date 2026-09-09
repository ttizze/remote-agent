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

    fun load(id: String): ByteArray {
        val file = snapshotFile(id)
        return if (file.baseFile.exists()) file.openRead().use { it.readBytes() } else byteArrayOf()
    }

    fun save(id: String, bytes: ByteArray) {
        val file = snapshotFile(id)
        val output = file.startWrite()
        var committed = false
        try {
            output.write(bytes)
            output.fd.sync()
            file.finishWrite(output)
            committed = true
        } finally {
            if (!committed) file.failWrite(output)
        }
    }

    private fun snapshotFile(id: String): AtomicFile {
        val name = Base64.getUrlEncoder().withoutPadding().encodeToString(id.encodeToByteArray())
        return AtomicFile(File(directory, "agent-snapshot-$name.json"))
    }
}

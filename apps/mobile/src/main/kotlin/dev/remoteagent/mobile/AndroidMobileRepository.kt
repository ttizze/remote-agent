package dev.remoteagent.mobile

import android.content.Context
import android.util.AtomicFile
import java.io.File
import java.util.Base64
import kotlinx.serialization.Serializable
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json

@Serializable internal data class HostProfile(val id: String, val name: String, val ticket: String)

private class ModelPreferencesReadException(cause: IllegalArgumentException) :
    Exception("Saved model preferences could not be read; defaults were restored.", cause)

/** Platform persistence owns profile metadata and the shared model preferences; core writes each Host's state. */
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

    fun modelPreferences(): Result<ByteArray> {
        val encoded = preferences.getString("orchestration-model-defaults", null).orEmpty()
        return if (encoded.isEmpty()) {
            Result.success(byteArrayOf())
        } else {
            try {
                Result.success(Base64.getDecoder().decode(encoded))
            } catch (error: IllegalArgumentException) {
                Result.failure(ModelPreferencesReadException(error))
            }
        }
    }

    fun saveModelPreferences(bytes: ByteArray) {
        preferences.edit().putString("orchestration-model-defaults", Base64.getEncoder().encodeToString(bytes)).apply()
    }

    /** The device state core keeps for the Host. */
    fun stateFile(id: String): String = File(directory, "orchestration-${encodedId(id)}.json").absolutePath

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

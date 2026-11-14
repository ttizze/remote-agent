package dev.remoteagent.mobile

import android.content.Context
import android.net.Uri
import android.os.Build
import android.provider.OpenableColumns
import android.provider.Settings
import android.util.AtomicFile
import dev.remoteagent.core.AgentException
import dev.remoteagent.core.Attachment
import dev.remoteagent.core.ClientPlatform
import dev.remoteagent.core.ClientStorage
import dev.remoteagent.core.PreferenceKey
import dev.remoteagent.core.PreferenceStorage
import dev.remoteagent.core.generateIdentity
import java.io.File
import java.io.IOException
import java.security.GeneralSecurityException
import java.util.Base64
import java.util.UUID
import kotlinx.serialization.Serializable
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json

@Serializable internal data class HostProfile(val id: String, val name: String, val ticket: String)

/** Platform persistence owns profile metadata and opaque core snapshots. */
internal class AndroidMobileRepository(private val context: Context) {
    private val directory = context.filesDir
    private val preferences = context.getSharedPreferences("agent-hosts", Context.MODE_PRIVATE)
    val deviceName: String
        get() =
            Settings.Global.getString(context.contentResolver, Settings.Global.DEVICE_NAME)?.takeIf { it.isNotBlank() }
                ?: Build.MODEL

    var selected: String?
        get() = preferences.getString("selected", null)
        set(value) {
            preferences.edit().putString("selected", value).apply()
        }

    var allHosts: Boolean
        get() = preferences.getBoolean("all-hosts", false)
        set(value) {
            preferences.edit().putBoolean("all-hosts", value).apply()
        }

    fun profiles(): List<HostProfile> =
        preferences.getString("profiles", null)?.let { Json.decodeFromString(it) } ?: emptyList()

    fun saveProfiles(profiles: List<HostProfile>) {
        preferences.edit().putString("profiles", Json.encodeToString(profiles)).apply()
    }

    fun sessionStorage(id: String) = SessionStorage(id)

    fun removeHost(id: String) {
        AndroidCredentialStore(context, id).remove()
        val copies = File(directory, "conversations/${encodedId(id)}")
        if (copies.exists() && !copies.deleteRecursively()) throw IOException("会話のキャッシュを削除できませんでした。")
    }

    inner class SessionStorage(private val id: String) : ClientStorage, ClientPlatform {
        private fun <T> access(body: () -> T): T =
            try {
                body()
            } catch (error: IOException) {
                throw AgentException.Failed(error.message ?: error.toString()).apply { initCause(error) }
            } catch (error: GeneralSecurityException) {
                throw AgentException.Failed(error.message ?: error.toString()).apply { initCause(error) }
            } catch (error: IllegalArgumentException) {
                throw AgentException.Failed(error.message ?: error.toString()).apply { initCause(error) }
            }

        private val file = AtomicFile(File(directory, "agent-snapshot-v3-${encodedId(id)}.json"))

        override fun load(): ByteArray = access {
            if (file.baseFile.exists()) file.openRead().use { it.readBytes() } else byteArrayOf()
        }

        override fun save(bytes: ByteArray) = access { file.writeSynced(bytes) }

        override fun identity(): ByteArray = access {
            AndroidCredentialStore(context, id).loadOrCreate(::generateIdentity)
        }

        override fun diagnosticsDirectory(): String? = access {
            File(directory, "connection-diagnostics/${encodedId(id)}").absolutePath
        }

        override fun conversationDirectory(): String? = access {
            File(directory, "conversations/${encodedId(id)}").absolutePath
        }
    }

    private fun encodedId(id: String): String =
        Base64.getUrlEncoder().withoutPadding().encodeToString(id.encodeToByteArray())
}

/** Device settings use the existing files and native language preference location. */
internal class AndroidPreferenceStorage(context: Context) : PreferenceStorage {
    private val directory = context.filesDir
    private val preferences = context.getSharedPreferences("bex-app", Context.MODE_PRIVATE)

    private fun file(key: PreferenceKey): AtomicFile? =
        when (key) {
            PreferenceKey.MODELS -> AtomicFile(File(directory, "model-preferences.json"))
            PreferenceKey.EDITOR -> AtomicFile(File(directory, "editor-preferences.json"))
            PreferenceKey.COMPOSER -> AtomicFile(File(directory, "composer-preferences.json"))
            PreferenceKey.CONTRAST -> AtomicFile(File(directory, "contrast.json"))
            PreferenceKey.WEB_LINKS -> AtomicFile(File(directory, "web-links.json"))
            PreferenceKey.LANGUAGE -> null
        }

    override fun load(key: PreferenceKey): ByteArray = access {
        val file = file(key)
        if (file == null) {
            (preferences.all["language"] as? String)?.let { Json.encodeToString(it).encodeToByteArray() }
                ?: byteArrayOf()
        } else {
            if (file.baseFile.exists()) file.openRead().use { it.readBytes() } else byteArrayOf()
        }
    }

    override fun save(key: PreferenceKey, bytes: ByteArray) = access {
        val file = file(key)
        if (file == null) {
            preferences.edit().putString("language", Json.decodeFromString<String>(bytes.decodeToString())).apply()
        } else {
            file.writeSynced(bytes)
        }
    }

    private fun <T> access(body: () -> T): T =
        try {
            body()
        } catch (error: IOException) {
            throw AgentException.Failed(error.message ?: error.toString()).apply { initCause(error) }
        }
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

internal fun importAttachment(context: Context, uri: Uri): Attachment {
    val resolver = context.contentResolver
    val name =
        resolver
            .query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)
            ?.use { if (it.moveToFirst()) it.getString(0) else null }
            ?.substringAfterLast('/') ?: "attachment"
    val folder = File(context.cacheDir, UUID.randomUUID().toString())
    if (!folder.mkdir()) throw IOException("添付用フォルダを作成できません")
    var imported = false
    try {
        val file = File(folder, name)
        resolver.openInputStream(uri)?.use { input -> file.outputStream().use(input::copyTo) }
            ?: throw IOException("添付ファイルを開けません")
        val attachment = Attachment(file.path, name, resolver.getType(uri)?.startsWith("image/") == true)
        imported = true
        return attachment
    } finally {
        if (!imported) folder.deleteRecursively()
    }
}

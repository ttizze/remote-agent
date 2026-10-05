package dev.remoteagent.mobile

import android.content.Context
import android.net.Uri
import android.provider.OpenableColumns
import android.util.AtomicFile
import dev.remoteagent.core.AgentException
import dev.remoteagent.core.Attachment
import dev.remoteagent.core.Snapshot
import dev.remoteagent.core.applyClientPreferences
import java.io.File
import java.io.IOException
import java.util.Base64
import java.util.UUID
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
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

    fun load(id: String?): ByteArray {
        val file = id?.let(::snapshotFile)
        val saved = if (file?.baseFile?.exists() == true) file.openRead().use { it.readBytes() } else byteArrayOf()
        val preferences = clientPreferencesFile()
        val defaults =
            if (preferences.baseFile.exists()) preferences.openRead().use { it.readBytes() } else byteArrayOf()
        return applyClientPreferences(saved, defaults)
    }

    fun save(id: String?, bytes: ByteArray, preferences: ByteArray?) {
        preferences?.let { clientPreferencesFile().writeSynced(it) }
        id?.let { snapshotFile(it).writeSynced(bytes) }
    }

    private fun clientPreferencesFile() = AtomicFile(File(directory, "agent-client-preferences.json"))

    private fun snapshotFile(id: String) = AtomicFile(File(directory, "agent-snapshot-${encodedId(id)}.json"))

    private fun encodedId(id: String): String =
        Base64.getUrlEncoder().withoutPadding().encodeToString(id.encodeToByteArray())
}

/** Serializes device preferences and Host saves before any subsequent load. */
internal class AndroidSnapshotStorage(
    private val repository: AndroidMobileRepository,
    scope: CoroutineScope,
    report: (String?) -> Unit,
) {
    private sealed interface Request {
        data class Save(val id: String?, val snapshot: Snapshot) : Request

        data class Load(val id: String?, val result: CompletableDeferred<ByteArray>) : Request
    }

    private val requests = Channel<Request>(Channel.UNLIMITED)
    private val worker =
        scope.launch(Dispatchers.IO) {
            var preferences: ByteArray? = null
            for (request in requests) {
                val error =
                    try {
                        when (request) {
                            is Request.Save -> {
                                val next = request.snapshot.serializeClientPreferences()
                                repository.save(
                                    request.id,
                                    request.snapshot.serializeLocalState(),
                                    next.takeUnless { preferences?.contentEquals(it) == true },
                                )
                                preferences = next
                            }
                            is Request.Load -> request.result.complete(repository.load(request.id))
                        }
                        null
                    } catch (error: IOException) {
                        error
                    } catch (error: AgentException) {
                        error
                    }
                if (error != null) {
                    if (request is Request.Load) request.result.completeExceptionally(error)
                    else withContext(Dispatchers.Main) { report(error.message) }
                }
            }
        }

    suspend fun save(id: String?, snapshot: Snapshot) {
        requests.send(Request.Save(id, snapshot))
    }

    suspend fun load(id: String?): ByteArray {
        val result = CompletableDeferred<ByteArray>()
        requests.send(Request.Load(id, result))
        return result.await()
    }

    suspend fun close() {
        requests.close()
        worker.join()
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

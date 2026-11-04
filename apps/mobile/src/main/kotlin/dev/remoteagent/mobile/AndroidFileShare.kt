package dev.remoteagent.mobile

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent as AndroidIntent
import androidx.compose.runtime.Composable
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.platform.LocalContext
import androidx.core.content.FileProvider
import dev.remoteagent.core.AgentException
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Outcome
import java.io.File
import java.io.IOException
import java.util.UUID
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch

/** The provider exposes this bounded cache, never Host paths. */
@Composable
internal fun rememberFileSharer(
    perform: ((Intent, (Result<Outcome>) -> Unit) -> Unit)?,
    failure: (String) -> Unit,
): (String, String) -> Unit {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    return { path, mime ->
        scope.launch {
            try {
                exportHostFile(context, path, mime, perform, FileExportAction.Share)
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (error: AgentException) {
                failure(error.displayMessage() ?: "ファイルを共有できません")
            } catch (error: IOException) {
                failure(error.displayMessage() ?: "ファイルを共有できません")
            }
        }
    }
}

internal enum class FileExportAction {
    Open,
    Share,
}

internal suspend fun exportHostFile(
    context: Context,
    path: String,
    mime: String,
    perform: ((Intent, (Result<Outcome>) -> Unit) -> Unit)?,
    action: FileExportAction,
) {
    val directory = File(context.cacheDir, "shared-files")
    directory.mkdirs()
    directory
        .listFiles()
        ?.filter { it.lastModified() < System.currentTimeMillis() - SHARE_CACHE_MILLIS }
        ?.forEach { it.delete() }
    val target = File(directory, "${UUID.randomUUID()}-${path.replace('\\', '/').substringAfterLast('/')}")
    var exported = false
    val failure =
        try {
            downloadPreview(target, path, perform)
            val uri = FileProvider.getUriForFile(context, "${context.packageName}.files", target)
            val intent =
                AndroidIntent().apply {
                    addFlags(AndroidIntent.FLAG_GRANT_READ_URI_PERMISSION)
                    clipData = android.content.ClipData.newRawUri(null, uri)
                }
            when (action) {
                FileExportAction.Open -> {
                    intent.action = AndroidIntent.ACTION_VIEW
                    intent.setDataAndType(uri, mime)
                    context.startActivity(intent)
                }
                FileExportAction.Share -> {
                    intent.action = AndroidIntent.ACTION_SEND
                    intent.type = mime
                    intent.putExtra(AndroidIntent.EXTRA_STREAM, uri)
                    context.startActivity(AndroidIntent.createChooser(intent, null))
                }
            }
            exported = true
            null
        } catch (error: ActivityNotFoundException) {
            error
        } catch (error: IllegalArgumentException) {
            error
        } catch (error: SecurityException) {
            error
        } finally {
            if (!exported) target.delete()
        }
    if (failure != null) throw IOException(failure.message, failure)
}

private const val SHARE_CACHE_MILLIS = 24 * 60 * 60 * 1000L

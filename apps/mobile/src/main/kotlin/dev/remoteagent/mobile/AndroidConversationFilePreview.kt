package dev.remoteagent.mobile

import android.graphics.BitmapFactory
import androidx.compose.foundation.Image
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.DownloadFile
import dev.remoteagent.core.Intent
import dev.remoteagent.core.MarkdownFileKind
import dev.remoteagent.core.MarkdownFileReference
import dev.remoteagent.core.Outcome
import java.io.File
import java.io.IOException
import java.util.UUID
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext

private const val PREVIEW_MAX_PIXELS = 2048
private const val TEXT_PREVIEW_MAX_BYTES = 2 * 1024 * 1024

private data class FilePreview(
    val text: String? = null,
    val image: ImageBitmap? = null,
    val markdown: Boolean = false,
    val cwd: String = "",
)

/** Existing attachment images read through authenticated Host transfers. File references use ConversationFileViewer. */
@Composable
internal fun ConversationFilePreview(
    reference: MarkdownFileReference,
    perform: ((Intent, (Result<Outcome>) -> Unit) -> Unit)?,
    dismiss: () -> Unit,
) {
    val language = LocalUiLanguage.current
    val context = LocalContext.current
    val clipboard = LocalClipboardManager.current
    val path = reference.path
    val label = reference.label
    val target = remember(path) { File(context.cacheDir, "preview-${UUID.randomUUID()}") }
    var content by remember(path) { mutableStateOf(FilePreview()) }
    var error by remember(path) { mutableStateOf<String?>(null) }
    var loading by remember(path) { mutableStateOf(true) }
    DisposableEffect(target) { onDispose { target.delete() } }
    LaunchedEffect(path) {
        try {
            loading = true
            error = null
            content = loadFilePreview(target, path, reference.kind, perform)
        } catch (cancelled: kotlinx.coroutines.CancellationException) {
            throw cancelled
        } catch (expectedFailure: Exception) {
            error = expectedFailure.displayMessage() ?: "ファイルを開けません"
        } finally {
            loading = false
        }
    }
    AlertDialog(
        onDismissRequest = dismiss,
        title = { Text(label) },
        text = {
            Column(Modifier.heightIn(max = 480.dp).verticalScroll(rememberScrollState())) {
                SelectionContainer {
                    Text(
                        path.takeUnless { it.startsWith("data:image/") } ?: language.text("添付画像"),
                        style = MaterialTheme.typography.labelSmall,
                    )
                }
                if (loading) CircularProgressIndicator()
                error?.let { Text(language.text(it), color = MaterialTheme.colorScheme.error) }
                content.image?.let { Image(it, label, Modifier.fillMaxWidth()) }
                content.text?.let { PreviewText(it, content.markdown, content.cwd, perform) }
            }
        },
        confirmButton = { TextButton(onClick = dismiss) { Text(language.text("閉じる")) } },
        dismissButton = {
            TextButton(onClick = { clipboard.setText(AnnotatedString(path)) }) { Text(language.text("パスをコピー")) }
        },
    )
}

private suspend fun loadFilePreview(
    target: File,
    path: String,
    kind: MarkdownFileKind,
    perform: ((Intent, (Result<Outcome>) -> Unit) -> Unit)?,
): FilePreview {
    downloadPreview(target, path, perform)
    return withContext(Dispatchers.IO) { readFilePreview(target, kind, path) }
}

private fun readFilePreview(target: File, kind: MarkdownFileKind, path: String): FilePreview {
    return when {
        kind == MarkdownFileKind.IMAGE -> {
            val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
            BitmapFactory.decodeFile(target.path, bounds)
            val options =
                BitmapFactory.Options().apply {
                    inSampleSize = 1
                    while (maxOf(bounds.outWidth, bounds.outHeight) / inSampleSize > PREVIEW_MAX_PIXELS) inSampleSize *=
                        2
                }
            FilePreview(image = checkNotNull(BitmapFactory.decodeFile(target.path, options)).asImageBitmap())
        }
        else -> {
            check(target.length() <= TEXT_PREVIEW_MAX_BYTES) { "プレビューできるサイズを超えています" }
            val bytes = target.readBytes()
            check(0.toByte() !in bytes) { "このファイル形式はプレビューできません" }
            FilePreview(
                text = bytes.toString(Charsets.UTF_8),
                markdown = kind == MarkdownFileKind.MARKDOWN,
                cwd = path.replace('\\', '/').substringBeforeLast('/'),
            )
        }
    }
}

@Composable
internal fun PreviewText(
    text: String,
    markdown: Boolean,
    cwd: String,
    perform: ((Intent, (Result<Outcome>) -> Unit) -> Unit)?,
) {
    val language = LocalUiLanguage.current
    var rendered by remember(text) { mutableStateOf(markdown) }
    if (markdown)
        TextButton(onClick = { rendered = !rendered }) {
            Text(if (rendered) language.text("ソースを表示") else language.text("Markdownを表示"))
        }
    if (rendered) Column { ConversationBody(text, cwd, perform) }
    else
        SelectionContainer {
            Text(
                text,
                Modifier.horizontalScroll(rememberScrollState()).padding(top = 12.dp),
                fontFamily = FontFamily.Monospace,
            )
        }
}

internal suspend fun downloadPreview(
    target: File,
    path: String,
    perform: ((Intent, (Result<Outcome>) -> Unit) -> Unit)?,
) {
    if (!target.exists()) {
        if (path.startsWith("data:image/")) {
            withContext(Dispatchers.IO) {
                target.writeBytes(java.util.Base64.getDecoder().decode(path.substringAfter(',')))
            }
            return
        }
        if (perform == null) throw IOException("ホストに接続してください")
        suspendCancellableCoroutine<Unit> { continuation ->
            perform(Intent.DownloadFile(DownloadFile(path, target.path))) { result ->
                if (continuation.isActive) continuation.resumeWith(result.map { Unit }) else target.delete()
            }
        }
    }
}

internal suspend fun fileOperation(
    intent: Intent,
    perform: ((Intent, (Result<Outcome>) -> Unit) -> Unit)?,
): Outcome {
    if (perform == null) throw IOException("ホストに接続してください")
    return suspendCancellableCoroutine { continuation ->
        perform(intent) { result -> if (continuation.isActive) continuation.resumeWith(result) }
    }
}

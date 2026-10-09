package dev.remoteagent.mobile

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.pdf.PdfRenderer
import android.os.ParcelFileDescriptor
import androidx.compose.foundation.Image
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
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
import java.util.UUID
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext

private const val PDF_PREVIEW_WIDTH = 1024f
private const val PREVIEW_MAX_PIXELS = 2048
private const val TEXT_PREVIEW_MAX_BYTES = 2 * 1024 * 1024

private data class FilePreview(
    val text: String? = null,
    val image: ImageBitmap? = null,
    val pages: Int = 0,
)

/** File previews always read through the authenticated Host transfer. */
@Composable
internal fun ConversationFilePreview(
    reference: MarkdownFileReference,
    perform: ((Intent, (Result<Outcome>) -> Unit) -> Unit)?,
    dismiss: () -> Unit,
) {
    val context = LocalContext.current
    val clipboard = LocalClipboardManager.current
    val target = remember(reference.path) { File(context.cacheDir, "preview-${UUID.randomUUID()}") }
    var content by remember(reference.path) { mutableStateOf(FilePreview()) }
    var error by remember(reference.path) { mutableStateOf<String?>(null) }
    var loading by remember(reference.path) { mutableStateOf(true) }
    var page by remember(reference.path) { mutableStateOf(0) }
    DisposableEffect(target) { onDispose { target.delete() } }
    LaunchedEffect(reference.path, page) {
        try {
            downloadPreview(target, reference.path, perform)
            loading = true
            error = null
            content =
                withContext(Dispatchers.IO) {
                    readFilePreview(target, reference.kind, reference.path, page)
                }
        } catch (cancelled: kotlinx.coroutines.CancellationException) {
            throw cancelled
        } catch (expectedFailure: Exception) {
            error = expectedFailure.message ?: "ファイルを開けません"
        } finally {
            loading = false
        }
    }
    AlertDialog(
        onDismissRequest = dismiss,
        title = { Text(reference.label) },
        text = {
            Column {
                SelectionContainer {
                    Text(reference.path, style = MaterialTheme.typography.labelSmall)
                }
                if (loading) CircularProgressIndicator()
                error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
                content.image?.let { Image(it, reference.label, Modifier.fillMaxWidth()) }
                PreviewPages(page, content.pages, { page-- }, { page++ })
                content.text?.let { content ->
                    PreviewText(
                        content,
                        reference.kind == MarkdownFileKind.MARKDOWN,
                        reference.path.substringBeforeLast('/'),
                        perform,
                    )
                }
            }
        },
        confirmButton = { TextButton(onClick = dismiss) { Text("閉じる") } },
        dismissButton = {
            TextButton(onClick = { clipboard.setText(AnnotatedString(reference.path)) }) {
                Text("パスをコピー")
            }
        },
    )
}

private fun readFilePreview(
    target: File,
    kind: MarkdownFileKind,
    path: String,
    page: Int,
): FilePreview {
    return when {
        path.endsWith(".pdf", ignoreCase = true) -> {
            PdfRenderer(ParcelFileDescriptor.open(target, ParcelFileDescriptor.MODE_READ_ONLY))
                .use { pdf ->
                    pdf.openPage(page).use { pdfPage ->
                        val scale =
                            minOf(
                                PDF_PREVIEW_WIDTH / pdfPage.width,
                                PREVIEW_MAX_PIXELS.toFloat() / pdfPage.height,
                            )
                        val bitmap =
                            Bitmap.createBitmap(
                                (pdfPage.width * scale).toInt().coerceAtLeast(1),
                                (pdfPage.height * scale).toInt().coerceAtLeast(1),
                                Bitmap.Config.ARGB_8888,
                            )
                        pdfPage.render(bitmap, null, null, PdfRenderer.Page.RENDER_MODE_FOR_DISPLAY)
                        FilePreview(image = bitmap.asImageBitmap(), pages = pdf.pageCount)
                    }
                }
        }
        kind == MarkdownFileKind.IMAGE -> {
            val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
            BitmapFactory.decodeFile(target.path, bounds)
            val options =
                BitmapFactory.Options().apply {
                    inSampleSize = 1
                    while (
                        maxOf(bounds.outWidth, bounds.outHeight) / inSampleSize > PREVIEW_MAX_PIXELS
                    ) inSampleSize *= 2
                }
            FilePreview(
                image = checkNotNull(BitmapFactory.decodeFile(target.path, options)).asImageBitmap()
            )
        }
        else -> {
            check(target.length() <= TEXT_PREVIEW_MAX_BYTES) { "プレビューできるサイズを超えています" }
            val bytes = target.readBytes()
            check(0.toByte() !in bytes) { "このファイル形式はプレビューできません" }
            FilePreview(text = bytes.toString(Charsets.UTF_8))
        }
    }
}

@Composable
private fun PreviewText(
    text: String,
    markdown: Boolean,
    cwd: String,
    perform: ((Intent, (Result<Outcome>) -> Unit) -> Unit)?,
) {
    var rendered by remember(text) { mutableStateOf(markdown) }
    if (markdown)
        TextButton(onClick = { rendered = !rendered }) {
            Text(if (rendered) "ソースを表示" else "Markdownを表示")
        }
    if (rendered)
        Column(Modifier.verticalScroll(rememberScrollState())) {
            ConversationBody(text, cwd, perform)
        }
    else
        SelectionContainer {
            Text(
                text,
                Modifier.horizontalScroll(rememberScrollState())
                    .verticalScroll(rememberScrollState())
                    .padding(top = 12.dp),
                fontFamily = FontFamily.Monospace,
            )
        }
}

private suspend fun downloadPreview(
    target: File,
    path: String,
    perform: ((Intent, (Result<Outcome>) -> Unit) -> Unit)?,
) {
    if (!target.exists()) {
        checkNotNull(perform) { "Hostに接続してください" }
        suspendCancellableCoroutine<Unit> { continuation ->
            perform(Intent.DownloadFile(DownloadFile(path, target.path))) { result ->
                if (continuation.isActive) continuation.resumeWith(result.map { Unit })
                else target.delete()
            }
        }
    }
}

@Composable
private fun PreviewPages(page: Int, pages: Int, previous: () -> Unit, next: () -> Unit) {
    if (pages > 1)
        Row {
            TextButton(onClick = previous, enabled = page > 0) { Text("前") }
            Text("${page + 1} / $pages")
            TextButton(onClick = next, enabled = page + 1 < pages) { Text("次") }
        }
}

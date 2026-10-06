@file:Suppress(
    "TooGenericExceptionCaught"
) // Native document providers and UniFFI surface heterogeneous I/O errors; cancellation is rethrown.

package dev.remoteagent.mobile

import android.content.Context
import android.net.Uri
import android.provider.OpenableColumns
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Image
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import dev.remoteagent.core.DraftAttachment
import dev.remoteagent.core.Intent
import java.io.File
import java.util.UUID
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

private const val MAX_ATTACHMENTS = 100
private const val COPY_BUFFER_BYTES = 32768
private const val MAX_FILE_BYTES = 50L * 1024 * 1024
private const val PHOTO_DECODE_EDGE = 4096
private const val PHOTO_OUTPUT_EDGE = 2048f
private const val JPEG_QUALITY = 85
private const val SAMPLE_STEP = 2

@Composable
@Suppress("LongMethod") // Native picker launchers share their captured destination draft.
internal fun ComposerAttachmentButton(model: AndroidAppModel) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var menu by remember { mutableStateOf(false) }
    var target by remember { mutableStateOf(model.profileId to model.snapshot.currentDraftKey()) }
    fun picked(uris: List<Uri>, photo: Boolean = false) {
        val destination = target
        scope.launch {
            for (uri in uris) {
                try {
                    val imported =
                        withContext(Dispatchers.IO) {
                            if (photo) importPhoto(context, uri) else importAttachment(context, uri)
                        }
                    if (model.profileId != destination.first) {
                        imported.file.delete()
                        return@launch
                    }
                    model.perform(
                        Intent.AttachFile(imported.file.path, imported.name, imported.mime, destination.second)
                    )
                } catch (failure: CancellationException) {
                    throw failure
                } catch (failure: Exception) {
                    model.notice = failure.message
                }
            }
        }
    }
    val files = rememberLauncherForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) { picked(it) }
    val photos =
        rememberLauncherForActivityResult(ActivityResultContracts.PickMultipleVisualMedia(MAX_ATTACHMENTS)) {
            picked(it, true)
        }
    Box {
        TextButton(
            onClick = {
                target = model.profileId to model.snapshot.currentDraftKey()
                menu = true
            },
            enabled = model.conversation.composer.canEdit,
        ) {
            Text("+")
        }
        DropdownMenu(menu, { menu = false }) {
            DropdownMenuItem(
                text = { Text("Photo Library") },
                onClick = {
                    menu = false
                    photos.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly))
                },
            )
            DropdownMenuItem(
                text = { Text("Choose Files") },
                onClick = {
                    menu = false
                    files.launch(arrayOf("*/*"))
                },
            )
        }
    }
}

private data class ImportedAttachment(val file: File, val name: String, val mime: String)

private fun importAttachment(context: Context, uri: Uri): ImportedAttachment {
    val name =
        context.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
            if (cursor.moveToFirst()) cursor.getString(0) else null
        } ?: "Attachment"
    val mime = context.contentResolver.getType(uri) ?: "application/octet-stream"
    val directory = File(context.filesDir, "draft-attachments").apply { mkdirs() }
    val file = File(directory, UUID.randomUUID().toString())
    try {
        context.contentResolver.openInputStream(uri)?.use { input ->
            file.outputStream().use { output -> copyAttachment(input, output) }
        } ?: error("Unable to read attachment")
        return ImportedAttachment(file, name, mime)
    } catch (failure: Exception) {
        file.delete()
        throw failure
    }
}

private fun copyAttachment(input: java.io.InputStream, output: java.io.OutputStream) {
    val buffer = ByteArray(COPY_BUFFER_BYTES)
    var total = 0L
    while (true) {
        val count = input.read(buffer)
        if (count < 0) return
        total += count
        require(total <= MAX_FILE_BYTES) { "File exceeds 50 MiB" }
        output.write(buffer, 0, count)
    }
}

private fun importPhoto(context: Context, uri: Uri): ImportedAttachment {
    val options = android.graphics.BitmapFactory.Options().apply { inJustDecodeBounds = true }
    context.contentResolver.openInputStream(uri)?.use { android.graphics.BitmapFactory.decodeStream(it, null, options) }
    options.inJustDecodeBounds = false
    options.inSampleSize = 1
    while (maxOf(options.outWidth, options.outHeight) / options.inSampleSize > PHOTO_DECODE_EDGE) options
        .inSampleSize *= SAMPLE_STEP
    val image =
        context.contentResolver.openInputStream(uri)?.use {
            android.graphics.BitmapFactory.decodeStream(it, null, options)
        } ?: error("Unable to decode photo")
    val scale = minOf(1f, PHOTO_OUTPUT_EDGE / maxOf(image.width, image.height))
    val scaled =
        android.graphics.Bitmap.createScaledBitmap(
            image,
            (image.width * scale).toInt(),
            (image.height * scale).toInt(),
            true,
        )
    val file = File(File(context.filesDir, "draft-attachments").apply { mkdirs() }, "${UUID.randomUUID()}.jpg")
    try {
        file.outputStream().use {
            require(scaled.compress(android.graphics.Bitmap.CompressFormat.JPEG, JPEG_QUALITY, it)) {
                "Unable to encode photo"
            }
        }
        return ImportedAttachment(file, "Photo.jpg", "image/jpeg")
    } catch (failure: Exception) {
        file.delete()
        throw failure
    } finally {
        if (scaled !== image) scaled.recycle()
        image.recycle()
    }
}

@Composable
internal fun ConversationAttachmentStrip(
    model: AndroidAppModel,
    attachments: List<DraftAttachment>,
    editing: Boolean = false,
) {
    Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        attachments.forEach { attachment -> AttachmentTile(model, attachment, editing) }
    }
}

@Composable
@Suppress("LongMethod", "CyclomaticComplexMethod") // Render attachment state and native preview/save launchers.
private fun AttachmentTile(model: AndroidAppModel, attachment: DraftAttachment, editing: Boolean) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var downloaded by remember(model.profileId, attachment.id) { mutableStateOf<File?>(null) }
    var failure by remember { mutableStateOf<String?>(null) }
    var preview by remember { mutableStateOf(false) }
    val local = attachment.localPath.takeIf { it.isNotEmpty() }?.let(::File) ?: downloaded
    var bitmap by remember(local?.path) { mutableStateOf<android.graphics.Bitmap?>(null) }
    LaunchedEffect(local?.path) {
        if (attachment.kind == "image" && local != null)
            bitmap =
                withContext(Dispatchers.IO) {
                    val options = android.graphics.BitmapFactory.Options().apply { inJustDecodeBounds = true }
                    android.graphics.BitmapFactory.decodeFile(local.path, options)
                    options.inJustDecodeBounds = false
                    options.inSampleSize = 1
                    while (
                        maxOf(options.outWidth, options.outHeight) / options.inSampleSize > PHOTO_DECODE_EDGE
                    ) options.inSampleSize *= SAMPLE_STEP
                    android.graphics.BitmapFactory.decodeFile(local.path, options)
                }
    }
    LaunchedEffect(model.profileId, attachment.remoteId) {
        if (attachment.kind == "image" && local == null && attachment.remoteId != null) {
            val file = File(context.cacheDir, "attachment-${UUID.randomUUID()}")
            try {
                model.downloadAttachment(attachment.remoteId!!, file.path)
                downloaded = file
            } catch (error: CancellationException) {
                file.delete()
                throw error
            } catch (error: Exception) {
                file.delete()
                failure = error.message
            }
        }
    }
    val cachedFile = downloaded
    DisposableEffect(cachedFile) { onDispose { cachedFile?.delete() } }
    val previewBitmap = bitmap
    DisposableEffect(previewBitmap) { onDispose { previewBitmap?.recycle() } }
    val save =
        rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument(attachment.mimeType)) { uri ->
            if (uri != null)
                scope.launch {
                    val file = File(context.cacheDir, "attachment-${UUID.randomUUID()}")
                    try {
                        model.downloadAttachment(attachment.remoteId ?: error("Attachment is uploading"), file.path)
                        withContext(Dispatchers.IO) {
                            context.contentResolver.openOutputStream(uri)?.use { output ->
                                file.inputStream().use { it.copyTo(output) }
                            } ?: error("Unable to save file")
                        }
                    } catch (error: CancellationException) {
                        throw error
                    } catch (error: Exception) {
                        failure = error.message
                    } finally {
                        file.delete()
                    }
                }
        }
    Column(Modifier.width(88.dp)) {
        if (bitmap != null)
            Image(
                bitmap!!.asImageBitmap(),
                attachment.name,
                Modifier.size(72.dp).clickable { preview = true },
                contentScale = ContentScale.Crop,
            )
        else
            TextButton(onClick = { if (attachment.remoteId != null) save.launch(attachment.name) }) {
                Text(if (attachment.kind == "image") "Photo" else "File")
            }
        Text(attachment.name, style = MaterialTheme.typography.labelSmall, maxLines = 1)
        if (attachment.status == "uploading") CircularProgressIndicator(Modifier.size(14.dp))
        if (editing && attachment.status == "failed")
            TextButton(onClick = { model.perform(Intent.RetryAttachment(attachment.id)) }) { Text("Retry") }
        if (editing) TextButton(onClick = { model.perform(Intent.RemoveAttachment(attachment.id)) }) { Text("Remove") }
        (failure ?: attachment.error)?.let {
            Text(it, style = MaterialTheme.typography.labelSmall, color = AppTheme.color("errorForeground"))
        }
    }
    if (preview && bitmap != null)
        Dialog({ preview = false }) {
            Image(bitmap!!.asImageBitmap(), attachment.name, contentScale = ContentScale.Fit)
        }
}

@file:Suppress("TooGenericExceptionCaught") // Downloads surface heterogeneous I/O errors; cancellation is rethrown.

package dev.remoteagent.mobile

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.AttachFile
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.Description
import androidx.compose.material.icons.outlined.Image
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import dev.remoteagent.core.Attachment
import dev.remoteagent.core.AttachmentKind
import dev.remoteagent.core.DraftAttachment
import dev.remoteagent.core.Intent
import java.io.File
import java.util.UUID
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

private const val MAX_ATTACHMENTS = 100
private const val PREVIEW_EDGE = 1024

/** Photo library or files, attached to `draftKey` (the composer or one answer). */
@Composable
internal fun AttachmentButton(model: AndroidAppModel, draftKey: String, enabled: Boolean, compact: Boolean = false) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var menu by remember { mutableStateOf(false) }
    var target by remember { mutableStateOf(model.profileId to draftKey) }
    fun picked(uris: List<Uri>) {
        val destination = target
        if (uris.isEmpty()) return
        scope.launch {
            val existing = model.snapshot.draftAttachments(destination.second)
            val prepared = withContext(Dispatchers.IO) { prepareAttachments(context, uris, existing) }
            if (model.profileId != destination.first) {
                prepared.files.forEach { File(it.path).delete() }
                return@launch
            }
            prepared.error?.let { model.notice = it }
            if (prepared.files.isNotEmpty()) model.perform(Intent.AttachFiles(destination.second, prepared.files))
        }
    }
    val files = rememberLauncherForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) { picked(it) }
    val photos =
        rememberLauncherForActivityResult(ActivityResultContracts.PickMultipleVisualMedia(MAX_ATTACHMENTS)) {
            picked(it)
        }
    Box {
        IconButton(
            onClick = {
                target = model.profileId to draftKey
                menu = true
            },
            enabled = enabled,
            modifier = Modifier.size(44.dp),
        ) {
            Icon(
                if (compact) Icons.Outlined.AttachFile else Icons.Outlined.Add,
                "Attach",
                tint = AppTheme.colors.icon,
                modifier = Modifier.size(22.dp),
            )
        }
        AnchoredMenu(menu, { menu = false }) {
            DropdownMenuItem(
                text = { Text("Photo Library", style = AppTheme.footnote) },
                leadingIcon = { Icon(Icons.Outlined.Image, null) },
                onClick = {
                    menu = false
                    photos.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly))
                },
            )
            DropdownMenuItem(
                text = { Text("Choose Files", style = AppTheme.footnote) },
                leadingIcon = { Icon(Icons.Outlined.Description, null) },
                onClick = {
                    menu = false
                    files.launch(arrayOf("*/*"))
                },
            )
        }
    }
}

private fun decodePreview(path: String): Bitmap? {
    val options = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeFile(path, options)
    options.inJustDecodeBounds = false
    var sample = 1
    while (maxOf(options.outWidth, options.outHeight) / sample > PREVIEW_EDGE) sample *= 2
    options.inSampleSize = sample
    return BitmapFactory.decodeFile(path, options)
}

/** A local preview, or the Host attachment downloaded into the cache while shown. */
@Composable
private fun rememberAttachmentImage(model: AndroidAppModel, localPath: String?, remoteId: String?): Bitmap? {
    val context = LocalContext.current
    var bitmap by remember(localPath, remoteId) { mutableStateOf<Bitmap?>(null) }
    LaunchedEffect(model.profileId, localPath, remoteId) {
        if (!localPath.isNullOrEmpty()) {
            bitmap = withContext(Dispatchers.IO) { decodePreview(localPath) }
            return@LaunchedEffect
        }
        val id = remoteId ?: return@LaunchedEffect
        val file = File(context.cacheDir, "attachment-${UUID.randomUUID()}")
        try {
            model.downloadAttachment(id, file.path)
            bitmap = withContext(Dispatchers.IO) { decodePreview(file.path) }
        } catch (error: CancellationException) {
            throw error
        } catch (_: Exception) {
            bitmap = null
        } finally {
            file.delete()
        }
    }
    val current = bitmap
    DisposableEffect(current) { onDispose { current?.recycle() } }
    return current
}

@Composable
private fun ImagePreviewDialog(bitmap: Bitmap, name: String, onDismiss: () -> Unit) {
    Dialog(onDismiss) {
        Image(bitmap.asImageBitmap(), name, Modifier.clickable(onClick = onDismiss), contentScale = ContentScale.Fit)
    }
}

/** A sent message's images (180 x 140, radius 14) and files. */
@Composable
internal fun MessageAttachments(model: AndroidAppModel, attachments: List<Attachment>, alignEnd: Boolean) {
    if (attachments.isEmpty()) return
    Row(
        Modifier.horizontalScroll(rememberScrollState()),
        horizontalArrangement = Arrangement.spacedBy(7.dp, if (alignEnd) Alignment.End else Alignment.Start),
    ) {
        attachments.forEach { attachment ->
            if (attachment.kind == AttachmentKind.IMAGE) RemoteImage(model, attachment, 180.dp, 140.dp)
            else FileChip(attachment.name)
        }
    }
}

@Composable
private fun RemoteImage(model: AndroidAppModel, attachment: Attachment, width: Dp, height: Dp) {
    val bitmap = rememberAttachmentImage(model, null, attachment.id)
    var preview by remember { mutableStateOf(false) }
    Box(
        Modifier.size(width, height).clip(RoundedCornerShape(14.dp)).background(AppTheme.colors.subtleStrong).clickable(
            enabled = bitmap != null
        ) {
            preview = true
        },
        contentAlignment = Alignment.Center,
    ) {
        if (bitmap != null)
            Image(bitmap.asImageBitmap(), attachment.name, Modifier.fillMaxSize(), contentScale = ContentScale.Crop)
        else CircularProgressIndicator(Modifier.size(16.dp), color = AppTheme.colors.iconMuted, strokeWidth = 2.dp)
    }
    if (preview && bitmap != null) ImagePreviewDialog(bitmap, attachment.name) { preview = false }
}

@Composable
private fun FileChip(name: String) {
    Row(
        Modifier.background(AppTheme.colors.subtleStrong, RoundedCornerShape(12.dp))
            .padding(horizontal = 10.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Icon(Icons.Outlined.Description, null, Modifier.size(16.dp), tint = AppTheme.colors.iconMuted)
        Text(
            name,
            Modifier.widthIn(max = 160.dp),
            style = AppTheme.caption,
            color = AppTheme.colors.foreground,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

/** The composer's attachment strip: thumbnails with remove, retry for failed uploads. */
@Composable
internal fun DraftAttachmentStrip(
    model: AndroidAppModel,
    attachments: List<DraftAttachment>,
    draftKey: String?,
    thumbnail: Dp = 56.dp,
) {
    if (attachments.isEmpty()) return
    Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        attachments.forEach { attachment -> DraftTile(model, attachment, draftKey, thumbnail) }
    }
}

@Composable
private fun DraftTile(model: AndroidAppModel, attachment: DraftAttachment, draftKey: String?, size: Dp) {
    val colors = AppTheme.colors
    val bitmap =
        if (attachment.kind == "image") rememberAttachmentImage(model, attachment.localPath, attachment.remoteId)
        else null
    Box(Modifier.size(size)) {
        Box(
            Modifier.fillMaxSize().clip(RoundedCornerShape(8.dp)).background(colors.subtleStrong),
            contentAlignment = Alignment.Center,
        ) {
            if (bitmap != null)
                Image(bitmap.asImageBitmap(), attachment.name, Modifier.fillMaxSize(), contentScale = ContentScale.Crop)
            else
                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                    Icon(Icons.Outlined.Description, null, Modifier.size(18.dp), tint = colors.iconMuted)
                    Text(attachment.name, Modifier.padding(horizontal = 4.dp), style = AppTheme.micro, maxLines = 1)
                }
            when (attachment.status) {
                "uploading" ->
                    CircularProgressIndicator(
                        Modifier.size(18.dp),
                        color = colors.primaryForeground,
                        strokeWidth = 2.dp,
                    )
                "failed" ->
                    IconButton(onClick = { model.perform(Intent.RetryAttachment(draftKey, attachment.id)) }) {
                        Icon(Icons.Outlined.Refresh, attachment.error ?: "Retry", tint = colors.dangerForeground)
                    }
            }
        }
        Box(
            Modifier.align(Alignment.TopEnd)
                .padding(2.dp)
                .size(18.dp)
                .background(colors.foreground.copy(alpha = 0.7f), CircleShape)
                .clickable { model.perform(Intent.RemoveAttachment(draftKey, attachment.id)) },
            contentAlignment = Alignment.Center,
        ) {
            Icon(Icons.Outlined.Close, "Remove ${attachment.name}", Modifier.size(12.dp), tint = colors.screen)
        }
    }
}

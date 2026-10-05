package dev.remoteagent.mobile

import android.graphics.BitmapFactory
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import com.revenuecat.placeholder.PlaceholderDefaults
import com.revenuecat.placeholder.placeholder
import dev.remoteagent.core.Attachment
import dev.remoteagent.core.DownloadFile
import dev.remoteagent.core.DraftKey
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Outcome
import java.io.File
import java.util.Base64
import java.util.UUID
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

private const val THUMBNAIL_MAX_PIXELS = 512

@Composable
internal fun ImageSkeleton(modifier: Modifier, label: String = "画像を読み込み中", tag: String = "image.loading.skeleton") {
    Box(
        modifier
            .placeholder(
                enabled = true,
                color = MaterialTheme.colorScheme.surfaceVariant,
                shape = RoundedCornerShape(10.dp),
                highlight = PlaceholderDefaults.pulse,
            )
            .semantics { contentDescription = label }
            .testTag(tag)
    )
}

@Composable
internal fun DraftAttachments(
    attachments: List<Attachment>,
    draftKey: DraftKey,
    perform: (Intent, (Result<Outcome>) -> Unit) -> Unit,
) {
    if (attachments.isEmpty()) return
    Row(
        Modifier.horizontalScroll(rememberScrollState()).padding(8.dp),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        attachments.forEachIndexed { index, attachment ->
            Box {
                if (attachment.isImage) {
                    AttachmentThumbnail(
                        attachment.path,
                        attachment.name,
                        perform,
                        Modifier.size(120.dp)
                            .clip(RoundedCornerShape(12.dp))
                            .background(MaterialTheme.colorScheme.surfaceVariant),
                        contentScale = ContentScale.Crop,
                    )
                } else Text(attachment.name, Modifier.padding(end = 48.dp, top = 12.dp))
                IconButton(
                    onClick = { perform(Intent.RemoveAttachment(draftKey, index.toUInt())) {} },
                    modifier =
                        Modifier.size(48.dp).align(Alignment.TopEnd).offset(x = 4.dp, y = -4.dp).semantics {
                            contentDescription = "${attachment.name}を外す"
                        },
                ) {
                    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.TopEnd) {
                        Icon(
                            painterResource(R.drawable.ic_close),
                            contentDescription = null,
                            tint = Color.Black,
                            modifier = Modifier.size(18.dp).background(Color.White, CircleShape).padding(3.dp),
                        )
                    }
                }
            }
        }
    }
}

@Composable
internal fun AttachmentThumbnail(
    path: String,
    name: String,
    perform: (Intent, (Result<Outcome>) -> Unit) -> Unit,
    modifier: Modifier,
    contentScale: ContentScale = ContentScale.Fit,
) {
    val context = LocalContext.current
    var bitmap by remember(path) { mutableStateOf<androidx.compose.ui.graphics.ImageBitmap?>(null) }
    var failed by remember(path) { mutableStateOf(false) }
    LaunchedEffect(path) {
        val target = File(context.cacheDir, "thumbnail-${UUID.randomUUID()}")
        try {
            if (path.startsWith("data:image/")) {
                withContext(Dispatchers.IO) { target.writeBytes(Base64.getDecoder().decode(path.substringAfter(','))) }
            } else {
                kotlinx.coroutines.suspendCancellableCoroutine { continuation ->
                    perform(Intent.DownloadFile(DownloadFile(path, target.path))) { result ->
                        if (continuation.isActive) continuation.resumeWith(result.map { Unit }) else target.delete()
                    }
                }
            }
            bitmap =
                withContext(Dispatchers.IO) {
                    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
                    BitmapFactory.decodeFile(target.path, bounds)
                    val options =
                        BitmapFactory.Options().apply {
                            inSampleSize = 1
                            while (maxOf(bounds.outWidth, bounds.outHeight) / inSampleSize > THUMBNAIL_MAX_PIXELS) {
                                inSampleSize *= 2
                            }
                        }
                    checkNotNull(BitmapFactory.decodeFile(target.path, options)).asImageBitmap()
                }
        } catch (error: kotlinx.coroutines.CancellationException) {
            throw error
        } catch (_: Exception) {
            failed = true
        } finally {
            target.delete()
        }
    }
    Box(modifier, contentAlignment = Alignment.Center) {
        val loaded = bitmap
        if (loaded != null) Image(loaded, name, Modifier.fillMaxSize(), contentScale = contentScale)
        else if (failed) Text("画像を表示できません", style = MaterialTheme.typography.labelSmall)
        else ImageSkeleton(Modifier.fillMaxSize())
    }
}

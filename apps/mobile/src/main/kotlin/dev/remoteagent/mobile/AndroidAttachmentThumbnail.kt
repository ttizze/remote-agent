package dev.remoteagent.mobile

import android.graphics.BitmapFactory
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.size
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
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.DownloadFile
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Outcome
import java.io.File
import java.util.Base64
import java.util.UUID
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

private const val THUMBNAIL_MAX_PIXELS = 512

@Composable
internal fun AttachmentThumbnail(
    path: String, name: String, perform: (Intent, (Result<Outcome>) -> Unit) -> Unit,
    modifier: Modifier = Modifier.size(104.dp),
) {
    val context = LocalContext.current
    var bitmap by remember(path) { mutableStateOf<androidx.compose.ui.graphics.ImageBitmap?>(null) }
    var failed by remember(path) { mutableStateOf(false) }
    LaunchedEffect(path) {
        val target = File(context.cacheDir, "thumbnail-${UUID.randomUUID()}")
        try {
            if (path.startsWith("data:image/")) {
                withContext(Dispatchers.IO) {
                    target.writeBytes(Base64.getDecoder().decode(path.substringAfter(',')))
                }
            } else {
                kotlinx.coroutines.suspendCancellableCoroutine { continuation ->
                    perform(Intent.DownloadFile(DownloadFile(path, target.path))) { result ->
                        if (continuation.isActive) continuation.resumeWith(result.map { Unit })
                        else target.delete()
                    }
                }
            }
            bitmap = withContext(Dispatchers.IO) {
                val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
                BitmapFactory.decodeFile(target.path, bounds)
                val options = BitmapFactory.Options().apply {
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
        if (loaded != null) Image(loaded, name, Modifier.fillMaxSize(), contentScale = ContentScale.Fit)
        else Text(if (failed) "画像を表示できません" else "読み込み中…", style = MaterialTheme.typography.labelSmall)
    }
}

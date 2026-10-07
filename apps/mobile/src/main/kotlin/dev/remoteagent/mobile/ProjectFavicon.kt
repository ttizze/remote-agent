package dev.remoteagent.mobile

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Canvas
import android.util.LruCache
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Folder
import androidx.compose.material3.Icon
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.unit.Dp
import com.caverock.androidsvg.SVG
import dev.remoteagent.core.ProjectIconView
import dev.remoteagent.core.Snapshot
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/** The snapshot the screens draw, for leaf views that only read core. */
internal val LocalSnapshot = staticCompositionLocalOf { Snapshot.empty() }

private const val ICON_CACHE_ENTRIES = 64
private const val ICON_RADIUS_RATIO = 0.16f

/** Vector icons are drawn at this many pixels a side, enough for the largest icon on dense screens. */
private const val VECTOR_ICON_PIXELS = 192

/** A decoded icon; `image` is null when the bytes are not an image. */
private class DecodedIcon(val image: ImageBitmap?)

/** Decoded project icons by content hash. */
private object ProjectIconCache {
    private val icons = LruCache<String, DecodedIcon>(ICON_CACHE_ENTRIES)

    fun cached(hash: String): DecodedIcon? = synchronized(icons) { icons.get(hash) }

    fun decode(hash: String, icon: ProjectIconView?): DecodedIcon =
        DecodedIcon(icon?.let(::bitmap)?.asImageBitmap()).also { synchronized(icons) { icons.put(hash, it) } }

    private fun bitmap(icon: ProjectIconView): Bitmap? =
        if (icon.mimeType == "image/svg+xml") runCatching { vector(icon.data) }.getOrNull()
        else BitmapFactory.decodeByteArray(icon.data, 0, icon.data.size)

    /** Renders an SVG into a square bitmap, scaled to fit like the raster icons. */
    private fun vector(data: ByteArray): Bitmap {
        val svg = SVG.getFromString(data.decodeToString())
        if (svg.documentViewBox == null && svg.documentWidth > 0 && svg.documentHeight > 0)
            svg.setDocumentViewBox(0f, 0f, svg.documentWidth, svg.documentHeight)
        svg.setDocumentWidth("100%")
        svg.setDocumentHeight("100%")
        return Bitmap.createBitmap(VECTOR_ICON_PIXELS, VECTOR_ICON_PIXELS, Bitmap.Config.ARGB_8888).also {
            svg.renderToCanvas(Canvas(it))
        }
    }
}

/** The project's icon from the Host, or the folder glyph. The bytes are read only for a hash not yet decoded. */
@Composable
internal fun ProjectFavicon(projectId: String, size: Dp, modifier: Modifier = Modifier) {
    val snapshot = LocalSnapshot.current
    val hash = remember(snapshot, projectId) { snapshot.projectIconHash(projectId) }
    val latest by rememberUpdatedState(snapshot)
    val icon by
        produceState(hash?.let(ProjectIconCache::cached), hash) {
            if (hash == null) value = null
            else if (value == null)
                value =
                    withContext(Dispatchers.Default) {
                        ProjectIconCache.cached(hash)
                            ?: ProjectIconCache.decode(hash, latest.projectIcon(projectId)?.takeIf { it.hash == hash })
                    }
        }
    val bitmap = icon?.image
    if (bitmap != null)
        Image(
            bitmap,
            null,
            modifier.size(size).clip(RoundedCornerShape(size * ICON_RADIUS_RATIO)),
            contentScale = ContentScale.Fit,
        )
    else Icon(Icons.Outlined.Folder, null, modifier.size(size), tint = AppTheme.colors.iconMuted)
}

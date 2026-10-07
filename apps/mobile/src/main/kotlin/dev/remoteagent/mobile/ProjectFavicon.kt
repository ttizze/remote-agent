package dev.remoteagent.mobile

import android.graphics.BitmapFactory
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
import dev.remoteagent.core.Snapshot
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/** The snapshot the screens draw, for leaf views that only read core. */
internal val LocalSnapshot = staticCompositionLocalOf { Snapshot.empty() }

private const val ICON_CACHE_ENTRIES = 64
private const val ICON_RADIUS_RATIO = 0.16f

/** A decoded icon; `image` is null when the platform cannot decode the bytes (such as SVG). */
private class DecodedIcon(val image: ImageBitmap?)

/** Decoded project icons by content hash. */
private object ProjectIconCache {
    private val icons = LruCache<String, DecodedIcon>(ICON_CACHE_ENTRIES)

    fun cached(hash: String): DecodedIcon? = synchronized(icons) { icons.get(hash) }

    fun decode(hash: String, data: ByteArray?): DecodedIcon =
        DecodedIcon(data?.let { BitmapFactory.decodeByteArray(it, 0, it.size) }?.asImageBitmap()).also {
            synchronized(icons) { icons.put(hash, it) }
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
                            ?: ProjectIconCache.decode(
                                hash,
                                latest.projectIcon(projectId)?.takeIf { it.hash == hash }?.data,
                            )
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

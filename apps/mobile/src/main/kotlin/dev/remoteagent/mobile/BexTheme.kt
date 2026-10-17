package dev.remoteagent.mobile

import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color

private const val BEX_PRIMARY = 0xFFB5A0F5
private const val BEX_ON_PRIMARY = 0xFF1D1532
private const val BEX_PRIMARY_CONTAINER = 0xFF352B4D
private const val BEX_ON_PRIMARY_CONTAINER = 0xFFE9DFFF
private const val BEX_BACKGROUND = 0xFF111316
private const val BEX_ON_BACKGROUND = 0xFFF0F1F3
private const val BEX_SURFACE = 0xFF111316
private const val BEX_ON_SURFACE = 0xFFF0F1F3
private const val BEX_SURFACE_VARIANT = 0xFF20242B
private const val BEX_ON_SURFACE_VARIANT = 0xFF989DA8
private const val BEX_OUTLINE = 0xFF565D69
private const val BEX_OUTLINE_VARIANT = 0xFF30353F
private const val BEX_TERTIARY = 0xFF83DDB0

private val BexColors =
    darkColorScheme(
        primary = Color(BEX_PRIMARY),
        onPrimary = Color(BEX_ON_PRIMARY),
        primaryContainer = Color(BEX_PRIMARY_CONTAINER),
        onPrimaryContainer = Color(BEX_ON_PRIMARY_CONTAINER),
        background = Color(BEX_BACKGROUND),
        onBackground = Color(BEX_ON_BACKGROUND),
        surface = Color(BEX_SURFACE),
        onSurface = Color(BEX_ON_SURFACE),
        surfaceVariant = Color(BEX_SURFACE_VARIANT),
        onSurfaceVariant = Color(BEX_ON_SURFACE_VARIANT),
        outline = Color(BEX_OUTLINE),
        outlineVariant = Color(BEX_OUTLINE_VARIANT),
        tertiary = Color(BEX_TERTIARY),
    )

@Composable
internal fun BexTheme(content: @Composable () -> Unit) {
    MaterialTheme(colorScheme = BexColors, content = content)
}

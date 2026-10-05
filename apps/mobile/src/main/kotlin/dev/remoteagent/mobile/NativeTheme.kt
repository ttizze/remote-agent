package dev.remoteagent.mobile

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp
import dev.remoteagent.core.ThemePlatform
import dev.remoteagent.core.nativePalette

private const val OPAQUE_ALPHA = 0xff000000L

internal fun paletteColor(rgb: UInt) = Color(OPAQUE_ALPHA or rgb.toLong())

private val nativeFont =
    FontFamily(
        Font(R.font.dm_sans_regular, FontWeight.Normal),
        Font(R.font.dm_sans_medium, FontWeight.Medium),
        Font(R.font.dm_sans_bold, FontWeight.Bold),
    )
private val nativeTypography =
    Typography().let { base ->
        base.copy(
            displayLarge = base.displayLarge.copy(fontFamily = nativeFont),
            displayMedium = base.displayMedium.copy(fontFamily = nativeFont),
            displaySmall = base.displaySmall.copy(fontFamily = nativeFont),
            headlineLarge = base.headlineLarge.copy(fontFamily = nativeFont),
            headlineMedium = base.headlineMedium.copy(fontFamily = nativeFont),
            headlineSmall = base.headlineSmall.copy(fontFamily = nativeFont),
            titleLarge = base.titleLarge.copy(fontFamily = nativeFont),
            titleMedium = base.titleMedium.copy(fontFamily = nativeFont),
            titleSmall = base.titleSmall.copy(fontFamily = nativeFont),
            bodyLarge =
                base.bodyLarge.copy(
                    fontFamily = nativeFont,
                    fontSize = 16.sp,
                    lineHeight = 23.sp,
                    letterSpacing = 0.sp,
                ),
            bodyMedium = base.bodyMedium.copy(fontFamily = nativeFont, letterSpacing = 0.sp),
            bodySmall = base.bodySmall.copy(fontFamily = nativeFont, letterSpacing = 0.sp),
            labelLarge = base.labelLarge.copy(fontFamily = nativeFont),
            labelMedium = base.labelMedium.copy(fontFamily = nativeFont),
            labelSmall = base.labelSmall.copy(fontFamily = nativeFont),
        )
    }

@Composable
internal fun NativeTheme(content: @Composable () -> Unit) {
    val dark = isSystemInDarkTheme()
    val colors = nativePalette(ThemePlatform.ANDROID, dark)
    val base = if (dark) darkColorScheme() else lightColorScheme()
    MaterialTheme(
        colorScheme =
            base.copy(
                primary = paletteColor(colors.primary),
                onPrimary = Color.White,
                background = paletteColor(colors.background),
                onBackground = paletteColor(colors.foreground),
                surface = paletteColor(colors.surface),
                onSurface = paletteColor(colors.foreground),
                surfaceVariant = paletteColor(colors.userBubble),
                surfaceContainer = paletteColor(colors.composer),
                onSurfaceVariant = paletteColor(colors.muted),
                outline = paletteColor(colors.border),
                error = paletteColor(colors.error),
            ),
        typography = nativeTypography,
        content = content,
    )
}

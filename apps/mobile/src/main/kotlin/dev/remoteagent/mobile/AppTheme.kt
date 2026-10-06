package dev.remoteagent.mobile

import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp
import dev.remoteagent.core.StatusTone
import dev.remoteagent.core.theme

internal object AppTheme {
    private val palette = theme(true)
    val fonts =
        FontFamily(
            Font(R.font.dmsans_regular),
            Font(R.font.dmsans_medium, FontWeight.Medium),
            Font(R.font.dmsans_bold, FontWeight.Bold),
        )

    fun color(role: String) = Color(android.graphics.Color.parseColor(palette.colors[role] ?: "#f5f5f5"))

    fun status(tone: StatusTone) =
        color(
            when (tone) {
                StatusTone.MUTED -> "textMuted"
                StatusTone.INFO -> "updateForeground"
                StatusTone.WARNING -> "warningForeground"
                StatusTone.INPUT -> "inputForeground"
                StatusTone.ERROR -> "errorForeground"
                StatusTone.SUCCESS -> "successForeground"
            }
        )

    val colors =
        darkColorScheme(
            primary = color("mobilePrimaryText"),
            onPrimary = color("accentForeground"),
            background = color("canvas"),
            onBackground = color("text"),
            surface = color("surface"),
            onSurface = color("text"),
            surfaceVariant = color("mobileGroupedCard"),
            onSurfaceVariant = color("textMuted"),
            outline = color("border"),
            error = color("errorForeground"),
            errorContainer = color("errorSurface"),
            secondary = color("textMuted"),
        )

    private fun style(size: Int, line: Int, weight: FontWeight = FontWeight.Normal) =
        TextStyle(fontFamily = fonts, fontWeight = weight, fontSize = size.sp, lineHeight = line.sp)

    // These sizes and line heights are the fixed typography specification.
    @Suppress("MagicNumber")
    val typography =
        Typography(
            bodyLarge = style(16, 23),
            bodyMedium = style(14, 19),
            bodySmall = style(12, 16),
            labelLarge = style(13, 17, FontWeight.Medium),
            labelMedium = style(12, 16),
            labelSmall = style(11, 14),
            titleSmall = style(14, 19, FontWeight.Medium),
            titleMedium = style(18, 23, FontWeight.Medium),
            titleLarge = style(21, 28, FontWeight.Bold),
            headlineSmall = style(26, 32, FontWeight.Bold),
            headlineMedium = style(30, 36, FontWeight.Bold),
        )
}

@Composable
internal fun AppMaterialTheme(content: @Composable () -> Unit) {
    MaterialTheme(colorScheme = AppTheme.colors, typography = AppTheme.typography, content = content)
}

@file:Suppress("MagicNumber") // The type scale is a fixed token set.

package dev.remoteagent.mobile

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp
import dev.remoteagent.core.theme

/** `#rrggbb` or `#rrggbbaa`, as the core theme writes colors. */
internal fun parseThemeColor(value: String): Color {
    val hex = value.removePrefix("#")
    require(value.startsWith("#") && (hex.length == 6 || hex.length == 8)) { "Unexpected color $value" }
    fun channel(index: Int) = hex.substring(index * 2, index * 2 + 2).toInt(16)
    return Color(channel(0), channel(1), channel(2), if (hex.length == 8) channel(3) else 255)
}

/** The core's mobile palette, status hues and terminal colors. */
internal class Palette(val dark: Boolean, tokens: Map<String, String>) {
    private val colors = tokens.mapValues { parseThemeColor(it.value) }

    private fun token(name: String) = requireNotNull(colors[name]) { "The theme has no $name" }

    val screen = token("mobileScreen")
    val sheet = token("mobileSheet")
    val card = token("mobileCard")
    val groupedCard = token("mobileGroupedCard")
    val cardAlt = token("mobileCardAlt")
    val threadSelected = token("mobileSelected")
    val rowHover = token("mobileRowHover")
    val composerPanel = token("mobileComposerPanel")
    val composerSurface = token("mobileComposerSurface")
    val composerBorder = token("mobileComposerBorder")
    val foreground = token("mobileForeground")
    val foregroundSecondary = token("mobileForegroundSecondary")
    val foregroundMuted = token("mobileForegroundMuted")
    val foregroundTertiary = token("mobileForegroundTertiary")
    val border = token("mobileBorder")
    val borderSubtle = token("mobileBorderSubtle")
    val separator = token("mobileSeparator")
    val subtle = token("mobileSubtle")
    val subtleStrong = token("mobileSubtleStrong")
    val primary = token("mobilePrimary")
    val primaryForeground = token("mobilePrimaryForeground")
    val primaryText = token("mobilePrimaryText")
    val secondary = token("mobileSecondary")
    val secondaryForeground = token("mobileSecondaryForeground")
    val warning = token("mobileWarning")
    val warningBorder = token("mobileWarningBorder")
    val warningForeground = token("mobileWarningForeground")
    val danger = token("mobileDanger")
    val dangerBorder = token("mobileDangerBorder")
    val dangerForeground = token("mobileDangerForeground")
    val update = token("mobileUpdate")
    val updateForeground = token("mobileUpdateForeground")
    val input = token("mobileInput")
    val inputBorder = token("mobileInputBorder")
    val placeholder = token("mobilePlaceholder")
    val icon = token("mobileIcon")
    val iconMuted = token("mobileIconMuted")
    val header = token("mobileHeader")
    val headerForeground = token("mobileHeaderForeground")
    val mdLink = token("mobileMarkdownLink")
    val mdCodeBackground = token("mobileMarkdownCode")
    val mdBlockquoteBorder = token("mobileMarkdownBlockquoteBorder")
    val mdHr = token("mobileMarkdownRule")
    val userBubble = token("mobileUserBubble")
    val userBubbleForeground = token("mobileUserBubbleForeground")
    val backdrop = token("mobileBackdrop")
    val drawer = token("mobileDrawer")
    val chevron = token("mobileChevron")
    val sky = token("statusSky")
    val indigo = token("statusIndigo")
    val emerald = token("statusEmerald")
    val emeraldIcon = token("statusEmeraldIcon")
    val rose = token("statusRose")
    val roseText = token("statusRoseText")
    val amber = token("statusAmber")
    val violet = token("statusViolet")
    val teal = token("statusTeal")
    val terminalBackground = token("terminalBackground")
    val terminalForeground = token("terminalForeground")
    val terminalCursor = token("terminalCursor")
}

private val LightPalette by lazy { Palette(false, theme(false).colors) }
private val DarkPalette by lazy { Palette(true, theme(true).colors) }

internal val LocalPalette = staticCompositionLocalOf { LightPalette }

internal object AppTheme {
    val fonts =
        FontFamily(
            Font(R.font.dmsans_regular),
            Font(R.font.dmsans_medium, FontWeight.Medium),
            Font(R.font.dmsans_bold, FontWeight.Bold),
            Font(R.font.dmsans_bold, FontWeight.ExtraBold),
        )

    /** Android rows and code use the platform monospace face. */
    val mono = FontFamily.Monospace

    val colors: Palette
        @Composable get() = LocalPalette.current

    private fun style(size: Int, line: Int, weight: FontWeight = FontWeight.Normal) =
        TextStyle(fontFamily = fonts, fontWeight = weight, fontSize = size.sp, lineHeight = line.sp)

    // micro, caption, label, footnote, body, headline, title, largeTitle, display.
    val micro = style(11, 14)
    val caption = style(12, 16)
    val label = style(13, 17)
    val footnote = style(14, 19)
    val body = style(16, 23)
    val headline = style(18, 23)
    val title = style(21, 28)
    val largeTitle = style(26, 32)
    val display = style(30, 36)

    val typography =
        Typography(
            bodyLarge = body,
            bodyMedium = footnote,
            bodySmall = caption,
            labelLarge = label.copy(fontWeight = FontWeight.Medium),
            labelMedium = caption,
            labelSmall = micro,
            titleSmall = footnote.copy(fontWeight = FontWeight.Medium),
            titleMedium = headline.copy(fontWeight = FontWeight.Medium),
            titleLarge = title.copy(fontWeight = FontWeight.Bold),
            headlineSmall = largeTitle.copy(fontWeight = FontWeight.Bold),
            headlineMedium = display.copy(fontWeight = FontWeight.Bold),
        )
}

@Composable
internal fun AppMaterialTheme(content: @Composable () -> Unit) {
    val palette = if (isSystemInDarkTheme()) DarkPalette else LightPalette
    val scheme =
        (if (palette.dark) darkColorScheme() else lightColorScheme()).copy(
            primary = palette.primary,
            onPrimary = palette.primaryForeground,
            background = palette.screen,
            onBackground = palette.foreground,
            surface = palette.screen,
            onSurface = palette.foreground,
            surfaceVariant = palette.groupedCard,
            onSurfaceVariant = palette.foregroundSecondary,
            surfaceContainer = palette.groupedCard,
            surfaceContainerLow = palette.sheet,
            surfaceContainerHigh = palette.cardAlt,
            outline = palette.border,
            outlineVariant = palette.border,
            error = palette.dangerForeground,
            errorContainer = palette.danger,
            secondary = palette.foregroundSecondary,
            secondaryContainer = palette.secondary,
            onSecondaryContainer = palette.secondaryForeground,
        )
    CompositionLocalProvider(LocalPalette provides palette) {
        MaterialTheme(colorScheme = scheme, typography = AppTheme.typography, content = content)
    }
}

@file:Suppress("MagicNumber") // Palette values and the type scale are fixed tokens.

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

/** The stock mobile palette, with the Android frame tone on the header. */
internal data class Palette(
    val dark: Boolean,
    val screen: Color,
    val sheet: Color,
    val card: Color,
    val groupedCard: Color,
    val cardAlt: Color,
    val threadSelected: Color,
    val rowHover: Color,
    val composerPanel: Color,
    val composerSurface: Color,
    val composerBorder: Color,
    val foreground: Color,
    val foregroundSecondary: Color,
    val foregroundMuted: Color,
    val foregroundTertiary: Color,
    val border: Color,
    val borderSubtle: Color,
    val separator: Color,
    val subtle: Color,
    val subtleStrong: Color,
    val primary: Color,
    val primaryForeground: Color,
    val primaryText: Color,
    val secondary: Color,
    val secondaryForeground: Color,
    val warning: Color,
    val warningBorder: Color,
    val warningForeground: Color,
    val danger: Color,
    val dangerBorder: Color,
    val dangerForeground: Color,
    val update: Color,
    val updateForeground: Color,
    val input: Color,
    val inputBorder: Color,
    val placeholder: Color,
    val icon: Color,
    val iconMuted: Color,
    val header: Color,
    val headerForeground: Color,
    val mdLink: Color,
    val mdCodeBackground: Color,
    val mdBlockquoteBorder: Color,
    val mdHr: Color,
    val userBubble: Color,
    val userBubbleForeground: Color,
    val backdrop: Color,
    val drawer: Color,
    val chevron: Color,
    val indigo: Color,
    val sky: Color,
    val emerald: Color,
    val rose: Color,
    val violet: Color,
    val terminalBackground: Color,
    val terminalForeground: Color,
    val terminalCursor: Color,
)

private fun rgba(red: Int, green: Int, blue: Int, alpha: Float) = Color(red, green, blue, (alpha * 255).toInt())

private fun hex(value: Long) = Color(0xFF000000 or value)

private val LightPalette =
    Palette(
        dark = false,
        screen = hex(0xfcfcfc),
        sheet = hex(0xfcfcfc),
        card = hex(0xffffff),
        groupedCard = hex(0xf4f4f5),
        cardAlt = hex(0xfcfcfc),
        threadSelected = hex(0xffffff),
        rowHover = hex(0xf4f4f5),
        composerPanel = hex(0xfcfcfc),
        composerSurface = rgba(244, 244, 245, 0.94f),
        composerBorder = rgba(228, 228, 231, 0.8f),
        foreground = hex(0x27272a),
        foregroundSecondary = hex(0x6f6f79),
        foregroundMuted = hex(0x6f6f79),
        foregroundTertiary = hex(0x71717b),
        border = hex(0xe4e4e7),
        borderSubtle = rgba(228, 228, 231, 0.7f),
        separator = rgba(228, 228, 231, 0.55f),
        subtle = hex(0xfafafa),
        subtleStrong = hex(0xfafafa),
        primary = hex(0x1b4ed8),
        primaryForeground = hex(0xffffff),
        primaryText = hex(0x1b4ed8),
        secondary = hex(0xfafafa),
        secondaryForeground = hex(0x27272a),
        warning = hex(0xfcf4e8),
        warningBorder = rgba(254, 154, 0, 0.32f),
        warningForeground = hex(0xbb4d00),
        danger = hex(0xfcebec),
        dangerBorder = rgba(251, 44, 54, 0.32f),
        dangerForeground = hex(0xc10007),
        update = hex(0xe0e6f7),
        updateForeground = hex(0x1b4ed8),
        input = hex(0xffffff),
        inputBorder = hex(0xd4d4d8),
        placeholder = hex(0x6f6f79),
        icon = hex(0x27272a),
        iconMuted = hex(0x71717b),
        header = hex(0xf4f4f5),
        headerForeground = hex(0x27272a),
        mdLink = hex(0x1b4ed8),
        mdCodeBackground = hex(0xffffff),
        mdBlockquoteBorder = hex(0xe4e4e7),
        mdHr = hex(0xe4e4e7),
        userBubble = hex(0xefeff1),
        userBubbleForeground = hex(0x27272a),
        backdrop = rgba(0, 0, 0, 0.22f),
        drawer = hex(0xfafafa),
        chevron = rgba(113, 113, 123, 0.42f),
        indigo = hex(0x4f46e5),
        sky = hex(0x0284c7),
        emerald = hex(0x047857),
        rose = hex(0xe11d48),
        violet = hex(0x7c3aed),
        terminalBackground = hex(0xfcfcfc),
        terminalForeground = hex(0x27272a),
        terminalCursor = hex(0x26384e),
    )

private val DarkPalette =
    Palette(
        dark = true,
        screen = hex(0x0a0a0a),
        sheet = hex(0x0a0a0a),
        card = hex(0x111111),
        groupedCard = hex(0x1a1b1b),
        cardAlt = hex(0x111111),
        threadSelected = hex(0x1a1b1b),
        rowHover = hex(0x141414),
        composerPanel = hex(0x0a0a0a),
        composerSurface = rgba(26, 27, 27, 0.9f),
        composerBorder = rgba(25, 25, 25, 0.8f),
        foreground = hex(0xf5f5f5),
        foregroundSecondary = hex(0x838383),
        foregroundMuted = hex(0x838383),
        foregroundTertiary = hex(0x818181),
        border = hex(0x191919),
        borderSubtle = rgba(25, 25, 25, 0.7f),
        separator = rgba(25, 25, 25, 0.55f),
        subtle = hex(0x111111),
        subtleStrong = hex(0x111111),
        primary = hex(0x346bf1),
        primaryForeground = hex(0xffffff),
        primaryText = hex(0x4b7cf3),
        secondary = hex(0x111111),
        secondaryForeground = hex(0xf5f5f5),
        warning = hex(0x312108),
        warningBorder = rgba(254, 154, 0, 0.32f),
        warningForeground = hex(0xffb900),
        danger = hex(0x301214),
        dangerBorder = rgba(251, 65, 74, 0.32f),
        dangerForeground = hex(0xff6467),
        update = hex(0x121b34),
        updateForeground = hex(0x51a2ff),
        input = hex(0x111111),
        inputBorder = hex(0x1e1e1e),
        placeholder = hex(0x838383),
        icon = hex(0xf5f5f5),
        iconMuted = hex(0x818181),
        header = hex(0x141414),
        headerForeground = hex(0xf1f3f7),
        mdLink = hex(0x3b70f1),
        mdCodeBackground = hex(0x111111),
        mdBlockquoteBorder = hex(0x191919),
        mdHr = hex(0x191919),
        userBubble = hex(0x161616),
        userBubbleForeground = hex(0xf5f5f5),
        backdrop = rgba(0, 0, 0, 0.48f),
        drawer = hex(0x000000),
        chevron = rgba(129, 129, 129, 0.42f),
        indigo = hex(0xa5b4fc),
        sky = hex(0x38bdf8),
        emerald = hex(0x6ee7b7),
        rose = hex(0xfb7185),
        violet = hex(0xa78bfa),
        terminalBackground = hex(0x0a0a0a),
        terminalForeground = hex(0xf5f5f5),
        terminalCursor = hex(0xb4cbff),
    )

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

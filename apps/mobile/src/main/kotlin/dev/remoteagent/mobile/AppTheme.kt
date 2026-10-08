@file:Suppress("MagicNumber") // The type scale is a fixed token set.

package dev.remoteagent.mobile

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp
import dev.remoteagent.core.MobileAppearance
import dev.remoteagent.core.MobileColorScheme
import dev.remoteagent.core.mobileAssignTheme
import dev.remoteagent.core.mobileThemeColors
import dev.remoteagent.core.mobileTypography
import dev.remoteagent.core.normalizeMobileAppearance
import java.util.Locale

/** `#rrggbb` or `#rrggbbaa`, as the core theme writes colors. */
internal fun parseThemeColor(value: String): Color {
    val hex = value.removePrefix("#")
    require(value.startsWith("#") && (hex.length == 6 || hex.length == 8)) {
        "Unexpected color $value"
    }
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

private val LightPalette by lazy { Palette(false, mobileThemeColors(null, false)) }

internal val LocalPalette = staticCompositionLocalOf { LightPalette }

internal object AppTheme {
    var appearance by mutableStateOf(MobileAppearanceSettings())
        private set

    private var loaded = false

    fun ensureLoaded(context: android.content.Context) {
        if (!loaded) {
            appearance = MobileAppearanceSettings.load(context)
            loaded = true
        }
    }

    fun update(context: android.content.Context, next: MobileAppearanceSettings) {
        appearance = next.normalized()
        appearance.save(context)
    }

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

    private fun resolvedTypography() = mobileTypography(appearance.toCore())

    private fun fontSize(size: Double, resolved: dev.remoteagent.core.MobileTypography): Double =
        when (size) {
            11.0 -> resolved.microFontSize
            12.0 -> resolved.captionFontSize
            13.0 -> resolved.labelFontSize
            14.0 -> resolved.footnoteFontSize
            16.0 -> resolved.bodyFontSize
            18.0 -> resolved.headlineFontSize
            21.0 -> resolved.titleFontSize
            26.0 -> resolved.largeTitleFontSize
            30.0 -> resolved.displayFontSize
            else -> size * resolved.baseFontSize / 16.0
        }

    private fun lineHeight(
        size: Double,
        line: Double,
        resolved: dev.remoteagent.core.MobileTypography,
    ): Double =
        when (line) {
            14.0 -> resolved.microLineHeight
            16.0 -> resolved.captionLineHeight
            17.0 -> resolved.labelLineHeight
            19.0 -> resolved.footnoteLineHeight
            23.0 -> if (size == 18.0) resolved.headlineLineHeight else resolved.bodyLineHeight
            28.0 -> resolved.titleLineHeight
            32.0 -> resolved.largeTitleLineHeight
            36.0 -> resolved.displayLineHeight
            else -> line * resolved.baseFontSize / 16.0
        }

    private fun style(
        size: Double,
        line: Double,
        weight: FontWeight = FontWeight.Normal,
    ): TextStyle {
        val resolved = resolvedTypography()
        return TextStyle(
            fontFamily = fonts,
            fontWeight = weight,
            fontSize = fontSize(size, resolved).sp,
            lineHeight = lineHeight(size, line, resolved).sp,
        )
    }

    // micro, caption, label, footnote, body, headline, title, largeTitle, display.
    val micro: TextStyle
        @Composable get() = style(11.0, 14.0)

    val caption: TextStyle
        @Composable get() = style(12.0, 16.0)

    val label: TextStyle
        @Composable get() = style(13.0, 17.0)

    val footnote: TextStyle
        @Composable get() = style(14.0, 19.0)

    val body: TextStyle
        @Composable get() = style(16.0, 23.0)

    val headline: TextStyle
        @Composable get() = style(18.0, 23.0)

    val title: TextStyle
        @Composable get() = style(21.0, 28.0)

    val largeTitle: TextStyle
        @Composable get() = style(26.0, 32.0)

    val display: TextStyle
        @Composable get() = style(30.0, 36.0)

    private val typographyMetrics
        get() = resolvedTypography()

    val codeFontSize: Float
        get() = typographyMetrics.codeFontSize.toFloat()

    val codeLineHeight: Float
        get() = typographyMetrics.codeLineHeight.toFloat()

    val codeLineNumberFontSize: Float
        get() = typographyMetrics.codeLineNumberFontSize.toFloat()

    val markdownCodeFontSize: Float
        get() = typographyMetrics.markdownCodeFontSize.toFloat()

    val codeWordWrap: Boolean
        get() = appearance.codeWordWrap

    val terminalFontSize: Double
        get() = typographyMetrics.terminalFontSize

    fun markdownSize(header: Int?): Float {
        val resolved = typographyMetrics
        return when (header) {
            null -> resolved.markdownBodyFontSize
            1 -> resolved.markdownH1FontSize
            2 -> resolved.markdownH2FontSize
            3 -> resolved.markdownH3FontSize
            else -> resolved.markdownH4FontSize
        }.toFloat()
    }

    val markdownCodeLineHeight: Float
        get() = typographyMetrics.markdownCodeLineHeight.toFloat()

    val markdownBodyLineHeight: Float
        get() = typographyMetrics.markdownBodyLineHeight.toFloat()

    @Composable
    fun typography() =
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

private fun dynamicThemeTokens(scheme: ColorScheme): Map<String, String> =
    mapOf(
        "mobileScreen" to scheme.background.toHex(),
        "mobileSheet" to scheme.surface.toHex(),
        "mobileCard" to scheme.surfaceContainerHigh.toHex(),
        "mobileGroupedCard" to scheme.surfaceContainer.toHex(),
        "mobileCardAlt" to scheme.surfaceContainerHighest.toHex(),
        "mobileComposerPanel" to scheme.surface.toHex(),
        "mobileComposerSurface" to scheme.surfaceContainer.toHex(),
        "mobileComposerBorder" to scheme.outlineVariant.toHex(),
        "mobileForeground" to scheme.onBackground.toHex(),
        "mobileForegroundSecondary" to scheme.onSurfaceVariant.toHex(),
        "mobileForegroundMuted" to scheme.onSurfaceVariant.toHex(),
        "mobileForegroundTertiary" to scheme.outline.toHex(),
        "mobileBorder" to scheme.outline.toHex(),
        "mobileBorderSubtle" to scheme.outlineVariant.toHex(),
        "mobileSeparator" to scheme.outlineVariant.toHex(),
        "mobileSubtle" to scheme.surfaceContainerLow.toHex(),
        "mobileSubtleStrong" to scheme.surfaceContainerHigh.toHex(),
        "mobilePrimary" to scheme.primary.toHex(),
        "mobilePrimaryForeground" to scheme.onPrimary.toHex(),
        "mobilePrimaryText" to scheme.primary.toHex(),
        "mobileSecondary" to scheme.secondaryContainer.toHex(),
        "mobileSecondaryForeground" to scheme.onSecondaryContainer.toHex(),
        "mobileMarkdownLink" to scheme.primary.toHex(),
        "mobileMarkdownCode" to scheme.surfaceContainerHighest.toHex(),
        "mobileMarkdownBlockquoteBorder" to scheme.outlineVariant.toHex(),
        "mobileMarkdownRule" to scheme.outlineVariant.toHex(),
        "mobileUserBubble" to scheme.primaryContainer.toHex(),
        "mobileUserBubbleForeground" to scheme.onPrimaryContainer.toHex(),
        "mobileDrawer" to scheme.surfaceContainerLow.toHex(),
        "mobileChevron" to scheme.onSurfaceVariant.toHex(),
        "mobileWarning" to scheme.tertiaryContainer.toHex(),
        "mobileWarningBorder" to scheme.outlineVariant.toHex(),
        "mobileWarningForeground" to scheme.onTertiaryContainer.toHex(),
        "mobileDanger" to scheme.errorContainer.toHex(),
        "mobileDangerBorder" to scheme.outlineVariant.toHex(),
        "mobileDangerForeground" to scheme.error.toHex(),
        "terminalBackground" to scheme.surfaceContainerLowest.toHex(),
        "terminalForeground" to scheme.onSurface.toHex(),
        "terminalCursor" to scheme.primary.toHex(),
        "mobileBackdrop" to scheme.scrim.toHex(),
    )

private fun materialColorScheme(palette: Palette, dynamicScheme: ColorScheme?): ColorScheme =
    (dynamicScheme ?: if (palette.dark) darkColorScheme() else lightColorScheme()).copy(
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

@Composable
internal fun AppMaterialTheme(content: @Composable () -> Unit) {
    val context = LocalContext.current
    AppTheme.ensureLoaded(context)
    val systemDark = isSystemInDarkTheme()
    val dark =
        when (AppTheme.appearance.colorScheme) {
            MobileColorScheme.SYSTEM -> systemDark
            MobileColorScheme.LIGHT -> false
            MobileColorScheme.DARK -> true
        }
    val dynamic = AppTheme.appearance.themeFor(dark) == "material-you"
    val dynamicScheme =
        if (dynamic) {
            if (dark) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context)
        } else null
    val baseTokens = mobileThemeColors(AppTheme.appearance.themeFor(dark), dark)
    val tokens = dynamicScheme?.let { baseTokens + dynamicThemeTokens(it) } ?: baseTokens
    val palette = Palette(dark, tokens)
    val scheme = materialColorScheme(palette, dynamicScheme)
    CompositionLocalProvider(LocalPalette provides palette) {
        MaterialTheme(colorScheme = scheme, typography = AppTheme.typography(), content = content)
    }
}

private fun Color.toHex(): String {
    val argb = toArgb()
    return String.format(
        Locale.ROOT,
        "#%02x%02x%02x%02x",
        (argb shr 16) and 0xff,
        (argb shr 8) and 0xff,
        argb and 0xff,
        (argb ushr 24) and 0xff,
    )
}

internal data class MobileAppearanceSettings(
    val colorScheme: MobileColorScheme = MobileColorScheme.SYSTEM,
    val theme: String? = null,
    val lightTheme: String? = null,
    val darkTheme: String? = null,
    val baseFontSize: Int = 16,
    val codeFontSize: Int? = null,
    val terminalFontSize: Double? = null,
    val codeWordWrap: Boolean = false,
) {
    fun themeFor(dark: Boolean): String? = if (dark) darkTheme ?: theme else lightTheme ?: theme

    fun toCore() =
        MobileAppearance(
            colorScheme = colorScheme,
            theme = theme,
            lightTheme = lightTheme,
            darkTheme = darkTheme,
            baseFontSize = baseFontSize.toUInt(),
            codeFontSize = codeFontSize?.toUInt(),
            terminalFontSize = terminalFontSize,
            codeWordWrap = codeWordWrap,
        )

    fun normalized(): MobileAppearanceSettings = fromCore(normalizeMobileAppearance(toCore()))

    fun assigningTheme(dark: Boolean, themeId: String?): MobileAppearanceSettings =
        fromCore(mobileAssignTheme(toCore(), dark, themeId))

    fun resolvedTerminalFontSize(): Double = mobileTypography(toCore()).terminalFontSize

    fun save(context: android.content.Context) {
        context
            .getSharedPreferences("mobile-appearance", android.content.Context.MODE_PRIVATE)
            .edit()
            .putString("colorScheme", colorScheme.name)
            .putString("theme", theme)
            .putString("lightTheme", lightTheme)
            .putString("darkTheme", darkTheme)
            .putInt("baseFontSize", baseFontSize)
            .putInt("codeFontSize", codeFontSize ?: 0)
            .putString("terminalFontSize", terminalFontSize?.toString())
            .putBoolean("codeWordWrap", codeWordWrap)
            .apply()
    }

    companion object {
        private fun fromCore(value: MobileAppearance): MobileAppearanceSettings =
            MobileAppearanceSettings(
                colorScheme = value.colorScheme,
                theme = value.theme,
                lightTheme = value.lightTheme,
                darkTheme = value.darkTheme,
                baseFontSize = value.baseFontSize.toInt(),
                codeFontSize = value.codeFontSize?.toInt(),
                terminalFontSize = value.terminalFontSize,
                codeWordWrap = value.codeWordWrap,
            )

        fun load(context: android.content.Context): MobileAppearanceSettings {
            val prefs =
                context.getSharedPreferences(
                    "mobile-appearance",
                    android.content.Context.MODE_PRIVATE,
                )
            return MobileAppearanceSettings(
                    colorScheme =
                        runCatching {
                                MobileColorScheme.valueOf(
                                    prefs.getString("colorScheme", null) ?: "SYSTEM"
                                )
                            }
                            .getOrDefault(MobileColorScheme.SYSTEM),
                    theme = prefs.getString("theme", null),
                    lightTheme = prefs.getString("lightTheme", null),
                    darkTheme = prefs.getString("darkTheme", null),
                    baseFontSize = prefs.getInt("baseFontSize", 16),
                    codeFontSize = prefs.getInt("codeFontSize", 0).takeIf { it != 0 },
                    terminalFontSize = prefs.getString("terminalFontSize", null)?.toDoubleOrNull(),
                    codeWordWrap = prefs.getBoolean("codeWordWrap", false),
                )
                .normalized()
        }
    }
}

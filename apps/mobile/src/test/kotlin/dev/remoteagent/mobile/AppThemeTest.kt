package dev.remoteagent.mobile

import androidx.compose.ui.graphics.Color
import dev.remoteagent.core.theme
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

class AppThemeTest {
    @Test
    fun readsOpaqueAndTranslucentColors() {
        assertEquals(Color(0x1b, 0x4e, 0xd8), parseThemeColor("#1b4ed8"))
        assertEquals(Color(0xfe, 0x9a, 0x00, 0x51), parseThemeColor("#fe9a0051"))
        assertThrows(IllegalArgumentException::class.java) { parseThemeColor("1b4ed8") }
        assertThrows(IllegalArgumentException::class.java) { parseThemeColor("#fff") }
    }

    @Test
    fun bothCorePalettesCarryEveryMobileToken() {
        val light = Palette(false, theme(false).colors)
        val dark = Palette(true, theme(true).colors)
        assertEquals(parseThemeColor(theme(false).colors.getValue("mobileScreen")), light.screen)
        assertEquals(parseThemeColor(theme(true).colors.getValue("statusTeal")), dark.teal)
    }
}

package dev.remoteagent.mobile

import dev.remoteagent.core.TerminalTab
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class ScreenRulesTest {
    private fun tab(id: String) = TerminalTab(id, id, id, "", true, false, false, "/repo", "Ready")

    @Test
    fun aClosedTerminalFallsBackToTheNearestLowerThenHigher() {
        val tabs = listOf(tab("term-1"), tab("term-2"), tab("term-4"), tab("setup-dev"))
        assertEquals("term-2", fallbackTerminal(tabs, "term-4"))
        assertEquals("term-2", fallbackTerminal(tabs, "term-3"))
        assertEquals("term-2", fallbackTerminal(tabs, "term-1"))
        assertNull(fallbackTerminal(listOf(tab("term-1")), "term-1"))
    }

    @Test
    fun theTerminalMenuNamesTheLastFolder() {
        assertEquals("app", folderName("/Users/me/app/"))
        assertEquals("app", folderName("app"))
        assertNull(folderName("/"))
    }
}

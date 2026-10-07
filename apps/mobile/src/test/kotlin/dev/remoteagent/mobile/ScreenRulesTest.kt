package dev.remoteagent.mobile

import dev.remoteagent.core.DiffScopeChoice
import dev.remoteagent.core.GitDiffView
import dev.remoteagent.core.TerminalTab
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class ScreenRulesTest {
    private fun tab(id: String) = TerminalTab(id, id, id, "", true, false, false, "Ready")

    @Test
    fun aClosedTerminalFallsBackToTheNearestLowerThenHigher() {
        val tabs = listOf(tab("term-1"), tab("term-2"), tab("term-4"), tab("setup-dev"))
        assertEquals("term-2", fallbackTerminal(tabs, "term-4"))
        assertEquals("term-2", fallbackTerminal(tabs, "term-3"))
        assertEquals("term-2", fallbackTerminal(tabs, "term-1"))
        assertNull(fallbackTerminal(listOf(tab("term-1")), "term-1"))
    }

    @Test
    fun anEmptyDiffSaysWhatItCompares() {
        val git = GitDiffView(true, false, null, "main", null, false, null, emptyList(), false)
        assertEquals("main ... HEAD", reviewEmptyDetail(DiffScopeChoice.Branch, git))
        assertEquals("Base branch unavailable", reviewEmptyDetail(DiffScopeChoice.Branch, git.copy(baseRef = null)))
        assertEquals("Staged, unstaged, and untracked files", reviewEmptyDetail(DiffScopeChoice.Unstaged, git))
        assertEquals("This diff is empty.", reviewEmptyDetail(DiffScopeChoice.LatestTurn, git))
    }
}

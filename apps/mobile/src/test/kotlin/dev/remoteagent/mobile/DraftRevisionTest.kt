package dev.remoteagent.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class DraftRevisionTest {
    @Test
    fun olderReceiptDoesNotUnlockNewerEdit() {
        val edits = DraftRevision()
        val first = edits.edit("a")
        val second = edits.edit("ab")
        assertEquals("a", second.second)
        assertFalse(edits.acknowledge(first.first))
        assertEquals(second.first, edits.pending)
        assertTrue(edits.acknowledge(second.first))
        assertNull(edits.pending)
    }

    @Test
    fun hostSwitchRejectsOldReceipt() {
        val edits = DraftRevision()
        val old = edits.edit("old host")
        edits.reset()
        val new = edits.edit("new host")
        assertFalse(edits.acknowledge(old.first))
        assertEquals(new.first, edits.pending)
    }
}

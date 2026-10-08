package dev.remoteagent.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class MarkdownTest {
    @Test
    fun context_links_use_the_record_id_after_the_last_path_separator() {
        assertEquals("terminal-42", markdownContextId("context://v1/terminal/terminal-42"))
        assertEquals("file-7", markdownContextId("context://v1/file/file-7"))
    }

    @Test
    fun links_without_a_matching_context_chip_fall_back_to_the_normal_opener() {
        assertNull(markdownContextChip("context://v1/file/missing", emptyList()))
    }
}

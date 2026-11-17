package dev.remoteagent.mobile

import androidx.test.annotation.UiThreadTest
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.termux.terminal.TerminalEmulator
import com.termux.terminal.TerminalSession
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class NativeTerminalRestoreTest {
    @Test
    @UiThreadTest
    fun checkpointRestorationDiscardsIncompleteParserState() {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val prefixes =
            listOf(
                "\u001b]0;unfinished".toByteArray(),
                "\u001b]0;unfinished\u001b".toByteArray(),
                "\u001b]1337;File=inline=1:".toByteArray(),
                "\u001b[31;".toByteArray(),
                byteArrayOf(0xe6.toByte(), 0x97.toByte()),
            )
        val checkpoint =
            "\u001bc\u001b[?25l\u001b[31;44m日本語\u001b[0m\r\nrestored\u001b[2;4H\u001b[6n".toByteArray()
        for ((columns, rows) in listOf(80 to 24, 37 to 8)) {
            for (prefix in prefixes) {
                val transport = RecordingTransport()
                val terminal = NativeTerminal(instrumentation.targetContext, transport)
                assertTrue(terminal.feed(1, "\u001b[32;45mold\r\n".repeat(30).toByteArray(), 0, 0))
                assertTrue(terminal.feed(2, prefix, 0, 0))
                assertTrue(terminal.feed(3, checkpoint, columns, rows))

                val fresh = NativeTerminal(instrumentation.targetContext, transport)
                fresh.session.emulator.resize(columns, rows, 8, 16)
                fresh.session.feed(checkpoint)

                val restored = terminal.session.emulator
                assertEquals("日本語", restored.getSelectedText(0, 0, 5, 0))
                assertEquals("restored", restored.getSelectedText(0, 1, 7, 1))
                assertEquals(1, restored.cursorRow)
                assertEquals(3, restored.cursorCol)
                assertFalse(restored.isCursorEnabled)
                assertSameState(fresh.session.emulator, restored)
                assertEquals(0, transport.writes)
                assertEquals(0, transport.resizes)
            }
        }
    }

    @Test
    @UiThreadTest
    fun ordinaryChunksKeepParserStateAndRejectedCheckpointsDoNotResetIt() {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val transport = RecordingTransport()
        val terminal = NativeTerminal(instrumentation.targetContext, transport)
        assertTrue(terminal.feed(1, "\u001b]0;live".toByteArray(), 0, 0))
        assertFalse(terminal.feed(1, "\u001bcignored".toByteArray(), 37, 8))
        assertFalse(terminal.feed(0, "\u001bcignored".toByteArray(), 37, 8))
        assertTrue(terminal.feed(2, " title\u0007visible".toByteArray(), 0, 0))

        val emulator = terminal.session.emulator
        assertEquals("live title", emulator.title)
        assertEquals("visible", emulator.getSelectedText(0, 0, 6, 0))
        assertEquals(80, emulator.mColumns)
        assertEquals(24, emulator.mRows)
        assertEquals(0, transport.writes)
        assertEquals(0, transport.resizes)
    }

    private fun assertSameState(expected: TerminalEmulator, actual: TerminalEmulator) {
        assertEquals(expected.mColumns, actual.mColumns)
        assertEquals(expected.mRows, actual.mRows)
        assertEquals(expected.cursorRow, actual.cursorRow)
        assertEquals(expected.cursorCol, actual.cursorCol)
        assertEquals(expected.cursorStyle, actual.cursorStyle)
        assertEquals(expected.isCursorEnabled, actual.isCursorEnabled)
        assertEquals(expected.isAlternateBufferActive, actual.isAlternateBufferActive)
        assertEquals(expected.title, actual.title)
        assertArrayEquals(expected.mColors.mCurrentColors, actual.mColors.mCurrentColors)
        val expectedScreen = expected.screen
        val actualScreen = actual.screen
        assertEquals(expectedScreen.activeTranscriptRows, actualScreen.activeTranscriptRows)
        assertEquals(expectedScreen.transcriptText, actualScreen.transcriptText)
        for (row in -expectedScreen.activeTranscriptRows until expected.mRows) {
            assertEquals(expectedScreen.getLineWrap(row), actualScreen.getLineWrap(row))
            for (column in 0 until expected.mColumns) {
                assertEquals(expectedScreen.getStyleAt(row, column), actualScreen.getStyleAt(row, column))
            }
        }
    }

    private class RecordingTransport : TerminalSession.Transport {
        var writes = 0
        var resizes = 0

        override fun write(data: ByteArray) {
            writes++
        }

        override fun resize(columns: Int, rows: Int) {
            resizes++
        }
    }
}

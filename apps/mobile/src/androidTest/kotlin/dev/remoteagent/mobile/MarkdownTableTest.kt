package dev.remoteagent.mobile

import android.graphics.Bitmap
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.width
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.captureToImage
import androidx.compose.ui.test.getUnclippedBoundsInRoot
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performSemanticsAction
import androidx.compose.ui.test.performTouchInput
import androidx.compose.ui.test.swipeLeft
import androidx.compose.ui.text.TextLayoutResult
import androidx.compose.ui.unit.dp
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

class MarkdownTableTest {
    @get:Rule val compose = createComposeRule()

    @Test
    fun sharedTableRendersEveryCellAfterStreamingAndReopening() {
        val fixture = InstrumentationRegistry.getInstrumentation().context.assets.open("table.md")
            .bufferedReader().use { it.readText() }
        val body = mutableStateOf(fixture.substringBefore("| Codex"))
        val visible = mutableStateOf(true)
        compose.setContent {
            MaterialTheme {
                Surface {
                    Column(Modifier.width(360.dp)) {
                        if (visible.value) ConversationBody(body.value)
                    }
                }
            }
        }
        compose.onNodeWithTag("markdown.cell.0.0.0").assertTextEquals("構成")
        compose.runOnIdle { body.value = fixture }
        verifyTable()
        compose.runOnIdle { visible.value = false }
        compose.waitForIdle()
        compose.runOnIdle { visible.value = true }
        verifyTable()
    }

    @Test
    fun sharedDocumentRendersProseAndResolvesReferences() {
        val fixture = InstrumentationRegistry.getInstrumentation().context.assets.open("document.md")
            .bufferedReader().use { it.readText() }
        compose.setContent {
            MaterialTheme { Surface { Column(Modifier.width(360.dp)) { ConversationBody(fixture) } } }
        }
        for (text in listOf("見出し", "3. 最初", "4. 次", "☑ 完了", "☐ 未完了", "引用")) {
            compose.onNodeWithText(text).assertIsDisplayed()
        }
        val layouts = mutableListOf<TextLayoutResult>()
        compose.onNodeWithText("前の 太字と 強調、取消、参照。")
            .performSemanticsAction(SemanticsActions.GetTextLayoutResult) { it(layouts) }
        val text = layouts.single().layoutInput.text
        assertTrue(text.spanStyles.any {
            text.text.substring(it.start, it.end) == "強調" &&
                it.item.fontWeight == androidx.compose.ui.text.font.FontWeight.Bold &&
                it.item.fontStyle == androidx.compose.ui.text.font.FontStyle.Italic
        })
        assertEquals("https://example.com/reference",
            (text.getLinkAnnotations(0, text.length).single().item as androidx.compose.ui.text.LinkAnnotation.Url).url)
        layouts.clear()
        compose.onNodeWithText("![コード内](not-an-image.png)\n| table | stays code |")
            .performSemanticsAction(SemanticsActions.GetTextLayoutResult) { it(layouts) }
        assertEquals(androidx.compose.ui.text.font.FontFamily.Monospace, layouts.single().layoutInput.style.fontFamily)
        assertTrue(layouts.single().layoutInput.text.spanStyles.all { it.item.fontFamily == null })
        compose.onNodeWithText("後の 参照。").assertExists()
        capture("markdown-document.png")
    }

    private fun verifyTable() {
        val first = compose.onNodeWithTag("markdown.cell.0.1.0")
        val second = compose.onNodeWithTag("markdown.cell.0.2.0")
        val header = compose.onNodeWithTag("markdown.cell.0.0.0")
        val headerLayouts = mutableListOf<TextLayoutResult>()
        header.performSemanticsAction(SemanticsActions.GetTextLayoutResult) { it(headerLayouts) }
        assertEquals(androidx.compose.ui.text.font.FontWeight.Bold,
            headerLayouts.single().layoutInput.text.spanStyles.single().item.fontWeight)
        first.assertIsDisplayed().assertTextEquals("Codexハーネス＋Claude接続")
        second.assertIsDisplayed().assertTextEquals("Codex／Claude Codeを並列接続")
        assertEquals(header.getUnclippedBoundsInRoot().left, first.getUnclippedBoundsInRoot().left)
        assertTrue(second.getUnclippedBoundsInRoot().top >= first.getUnclippedBoundsInRoot().bottom)
        capture("markdown-table-left.png")
        compose.onNodeWithTag("markdown.table.0").performTouchInput { swipeLeft() }
        compose.onNodeWithTag("markdown.table.0").performTouchInput { swipeLeft() }
        val burden = compose.onNodeWithTag("markdown.cell.0.1.2")
        burden.assertIsDisplayed()
            .assertTextEquals("通信変換、モデルの挙動、サブスク認証との適合を検証する必要")
        compose.onNodeWithTag("markdown.cell.0.2.2").assertTextEquals("両者の機能差をBexが吸収する必要")
        val layouts = mutableListOf<TextLayoutResult>()
        burden.performSemanticsAction(SemanticsActions.GetTextLayoutResult) { it(layouts) }
        assertTrue(layouts.single().lineCount > 1)
        assertFalse(layouts.single().hasVisualOverflow)
        assertEquals(first.getUnclippedBoundsInRoot().top, burden.getUnclippedBoundsInRoot().top)
        capture("markdown-table-right.png")
    }

    private fun capture(name: String) {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        File(context.filesDir, name).outputStream().use {
            compose.onRoot().captureToImage().asAndroidBitmap().compress(Bitmap.CompressFormat.PNG, 100, it)
        }
    }
}

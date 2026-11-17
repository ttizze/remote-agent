package dev.remoteagent.mobile

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.ContextWrapper
import android.content.Intent as AndroidIntent
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.width
import androidx.compose.material3.Surface
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.click
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performSemanticsAction
import androidx.compose.ui.test.performTouchInput
import androidx.compose.ui.text.TextLayoutResult
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Outcome
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

class MarkdownLinkTest {
    @get:Rule val compose = createComposeRule()

    @Test
    fun tapsOnWrappedWebsiteAndPullRequestLinksLaunchTheirExactUrls() {
        val launched = mutableListOf<AndroidIntent>()
        val text = "Android版を getbex.dev で公開しました。PR #65へ反映済みです。"
        show(
            "Android版を [getbex.dev](https://getbex.dev) で公開しました。[PR #65](https://github.com/ttizze/remote-agent/pull/65)へ反映済みです。",
            { launched.add(it) },
        )
        tapText(text, "getbex.dev")
        tapText(text, "PR #65")
        compose.runOnIdle {
            assertEquals(
                listOf("https://getbex.dev", "https://github.com/ttizze/remote-agent/pull/65"),
                launched.map { it.dataString },
            )
            assertTrue(launched.all { it.action == AndroidIntent.ACTION_VIEW })
            assertTrue(launched.all { AndroidIntent.CATEGORY_BROWSABLE in it.categories.orEmpty() })
        }
    }

    @Test
    fun linkInsideTableLaunchesTheBrowser() {
        val launched = mutableListOf<AndroidIntent>()
        show("| 配布 |\n| --- |\n| [Android APK](https://getbex.dev/#download) |", { launched.add(it) })
        tapText("Android APK", "Android APK")
        compose.runOnIdle { assertEquals("https://getbex.dev/#download", launched.single().dataString) }
    }

    @Test
    fun unavailableBrowserShowsTheUrlAndCopyAction() {
        show("[getbex.dev](https://getbex.dev)", { throw ActivityNotFoundException() })
        tapText("getbex.dev", "getbex.dev")
        compose.onNodeWithText("リンクを開けません").assertExists()
        compose.onNodeWithText("対応するブラウザーやアプリを確認してください。\nhttps://getbex.dev").assertExists()
        compose.onNodeWithText("リンクをコピー").performClick()
        compose.onNodeWithText("閉じる").performClick()
        compose.onNodeWithText("リンクを開けません").assertDoesNotExist()
    }

    @Test
    fun workspaceLinkStillUsesAuthenticatedHostResolutionAndFileRead() {
        val launched = mutableListOf<AndroidIntent>()
        val intents = mutableListOf<Intent>()
        val resolvedPath = "/host/notes.md"
        compose.setContent {
            val base = LocalContext.current
            CompositionLocalProvider(LocalContext provides LinkContext(base) { launched.add(it) }) {
                AppTheme {
                    ConversationBlocks(dev.remoteagent.core.markdownBlocks("[notes.md](/repo/notes.md)"), "/repo") {
                        intent,
                        complete ->
                        intents.add(intent)
                        complete(
                            if (intent is Intent.ResolveFileReference)
                                Result.success(Outcome.FileReferenceResolved(resolvedPath, false))
                            else Result.failure(IllegalStateException("fixture read"))
                        )
                    }
                }
            }
        }
        // Inline AndroidViews can be merged into the paragraph's accessibility
        // node. Locate the chip's layout directly and dispatch a real touch.
        compose
            .onNodeWithTag("markdown.file./repo/notes.md", useUnmergedTree = true)
            .assertIsDisplayed()
            .performTouchInput { click() }
        compose.waitForIdle()
        compose.onNodeWithText("fixture read").assertExists()
        compose.runOnIdle {
            assertTrue(launched.isEmpty())
            assertEquals(2, intents.size)
            assertEquals("/repo/notes.md", (intents[0] as Intent.ResolveFileReference).v1.path)
            assertEquals(resolvedPath, (intents[1] as Intent.ReadFile).v1.path)
        }
    }

    private fun show(markdown: String, launch: (AndroidIntent) -> Unit) {
        compose.setContent {
            val base = LocalContext.current
            CompositionLocalProvider(LocalContext provides LinkContext(base, launch)) {
                AppTheme {
                    Surface {
                        Column(Modifier.width(240.dp)) {
                            ConversationBlocks(dev.remoteagent.core.markdownBlocks(markdown), "", null)
                        }
                    }
                }
            }
        }
    }

    private fun tapText(paragraph: String, label: String) {
        val node = compose.onNodeWithText(paragraph)
        val layouts = mutableListOf<TextLayoutResult>()
        node.performSemanticsAction(SemanticsActions.GetTextLayoutResult) { it(layouts) }
        val offset = paragraph.indexOf(label) + label.length / 2
        val position = layouts.single().getBoundingBox(offset).center
        node.performTouchInput { click(position) }
    }
}

private class LinkContext(base: Context, private val launch: (AndroidIntent) -> Unit) : ContextWrapper(base) {
    override fun startActivity(intent: AndroidIntent) = launch(intent)
}

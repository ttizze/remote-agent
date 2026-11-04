package dev.remoteagent.mobile

import android.content.ContextWrapper
import android.content.Intent as AndroidIntent
import androidx.compose.material3.Surface
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsNotDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.uiautomator.By
import androidx.test.uiautomator.UiDevice
import androidx.test.uiautomator.Until
import dev.remoteagent.core.FileContent
import dev.remoteagent.core.Intent
import dev.remoteagent.core.MarkdownFileKind
import dev.remoteagent.core.MarkdownFileReference
import dev.remoteagent.core.Outcome
import java.io.File
import java.io.IOException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

/** T3's file surfaces keep each read alive independently and scroll the rendered document. */
class FileViewerTest {
    @get:Rule val compose = createComposeRule()

    @Test
    fun openingAndClosingAChildKeepsItsParentMarkdownAndDoesNotDropTheChild() {
        val reads = mutableListOf<String>()
        show("Parent body\n\n[child.md](child.md)", reads)
        compose.onNodeWithText("Parent body").assertIsDisplayed()
        val device = UiDevice.getInstance(InstrumentationRegistry.getInstrumentation())
        val chip = device.wait(Until.findObject(By.text("child.md")), NATIVE_MENU_WAIT_MILLIS)
        assertNotNull("native file chip", chip)
        chip!!.click()
        compose.waitForIdle()
        compose.onNodeWithText("Child body").assertIsDisplayed()
        compose.onNodeWithText("Parent body").assertExists()
        device.pressBack()
        compose.waitForIdle()
        compose.onNodeWithText("Child body").assertDoesNotExist()
        compose.onNodeWithText("Parent body").assertIsDisplayed()
        compose.runOnIdle { assertEquals(listOf("/repo/parent.md", "/repo/child.md"), reads) }
    }

    @Test
    fun renderedMarkdownCanScrollToItsLastParagraph() {
        show((1..100).joinToString("\n\n") { "Paragraph $it" } + "\n\nLast paragraph", mutableListOf())
        compose.onNodeWithText("Paragraph 1").assertIsDisplayed()
        compose.onNodeWithText("Last paragraph").assertIsNotDisplayed()
        compose.onNodeWithText("Last paragraph").performScrollTo().assertIsDisplayed()
    }

    @Test
    fun truncatedTextSharesTheFullResolvedFileInsteadOfItsPreview() {
        val opened = mutableListOf<AndroidIntent>()
        val downloads = mutableListOf<File>()
        val original = "preview\n" + "x".repeat(1024 * 1024) + "full file tail"
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        compose.setContent {
            val activity = LocalContext.current
            val exportContext = remember {
                object : ContextWrapper(activity) {
                    override fun startActivity(intent: AndroidIntent) {
                        opened.add(intent)
                    }
                }
            }
            AppTheme {
                CompositionLocalProvider(LocalContext provides exportContext) {
                    ConversationFileViewer(
                        MarkdownFileReference("large.txt", "large.txt", MarkdownFileKind.FILE, null, null),
                        source = "large.txt",
                        workspaceRoot = "/repo",
                        perform = { intent, complete ->
                            when (intent) {
                                is Intent.ResolveFileReference ->
                                    complete(
                                        Result.success(Outcome.FileReferenceResolved("/repo/nested/large.txt", false))
                                    )
                                is Intent.ReadFile ->
                                    complete(
                                        Result.success(
                                            Outcome.FileRead(
                                                FileContent(
                                                    intent.v1.path,
                                                    "prefix",
                                                    "preview",
                                                    original.length.toULong(),
                                                    true,
                                                )
                                            )
                                        )
                                    )
                                is Intent.DownloadFile -> {
                                    assertEquals("/repo/nested/large.txt", intent.v1.source)
                                    downloads.add(File(intent.v1.destination).apply { writeText(original) })
                                    complete(Result.success(Outcome.Applied))
                                }
                                else -> error("unexpected request: $intent")
                            }
                        },
                        dismiss = {},
                    )
                }
            }
        }
        try {
            compose.onNodeWithText("preview").assertIsDisplayed()
            compose.onNodeWithText("ダウンロード").performClick()
            compose.runOnIdle {
                val chooser = opened.single()
                assertEquals(AndroidIntent.ACTION_CHOOSER, chooser.action)
                val share =
                    checkNotNull(chooser.getParcelableExtra(AndroidIntent.EXTRA_INTENT, AndroidIntent::class.java))
                assertEquals(AndroidIntent.ACTION_SEND, share.action)
                val uri =
                    checkNotNull(share.getParcelableExtra(AndroidIntent.EXTRA_STREAM, android.net.Uri::class.java))
                context.contentResolver.openInputStream(uri).use { stream ->
                    assertEquals(original, checkNotNull(stream).bufferedReader().readText())
                }
            }
        } finally {
            downloads.forEach { it.delete() }
        }
    }

    @Test
    fun videoRetriesTransferAndOpensNativeViewerWithAReadableContentUri() {
        val requests = mutableListOf<Intent>()
        val opened = mutableListOf<AndroidIntent>()
        val downloads = mutableListOf<File>()
        var dismissed = false
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        compose.setContent {
            val activity = LocalContext.current
            val previewContext = remember {
                object : ContextWrapper(activity) {
                    override fun startActivity(intent: AndroidIntent) {
                        opened.add(intent)
                    }
                }
            }
            AppTheme {
                CompositionLocalProvider(LocalContext provides previewContext) {
                    ConversationFileViewer(
                        MarkdownFileReference("/host/clip.MP4", "clip.MP4", MarkdownFileKind.FILE, null, null),
                        perform = { intent, complete ->
                            requests.add(intent)
                            check(intent is Intent.DownloadFile) { "Video must bypass text reads: $intent" }
                            val target = File(intent.v1.destination)
                            downloads.add(target)
                            if (downloads.size == 1) {
                                target.writeBytes(byteArrayOf(0))
                                complete(Result.failure(IOException("transfer interrupted")))
                            } else {
                                target.outputStream().use { stream -> repeat(1025) { stream.write(ByteArray(1024)) } }
                                complete(Result.success(Outcome.Applied))
                            }
                        },
                        dismiss = { dismissed = true },
                    )
                }
            }
        }
        try {
            compose.onNodeWithText("transfer interrupted").assertIsDisplayed()
            compose.runOnIdle {
                assertTrue(opened.isEmpty())
                assertTrue(!downloads.first().exists())
            }
            compose.onNodeWithText("再試行").performClick()
            compose.runOnIdle {
                assertEquals(2, requests.size)
                assertEquals("/host/clip.MP4", (requests.last() as Intent.DownloadFile).v1.source)
                val intent = opened.single()
                assertEquals(AndroidIntent.ACTION_VIEW, intent.action)
                assertEquals("video/mp4", intent.type)
                assertEquals("content", intent.data?.scheme)
                assertTrue(intent.flags and AndroidIntent.FLAG_GRANT_READ_URI_PERMISSION != 0)
                context.contentResolver.openInputStream(checkNotNull(intent.data)).use { stream ->
                    assertEquals(1025 * 1024, checkNotNull(stream).readBytes().size)
                }
                assertTrue(dismissed)
            }
        } finally {
            downloads.forEach { it.delete() }
        }
    }

    private fun show(parent: String, reads: MutableList<String>) {
        compose.setContent {
            AppTheme {
                Surface {
                    ConversationFileViewer(
                        MarkdownFileReference("/repo/parent.md", "parent.md", MarkdownFileKind.MARKDOWN, null, null),
                        perform = { intent, complete ->
                            when (intent) {
                                is Intent.ResolveFileReference ->
                                    complete(Result.success(Outcome.FileReferenceResolved(intent.v1.path, false)))
                                is Intent.ReadFile -> {
                                    val path = intent.v1.path
                                    reads.add(path)
                                    val text = if (path.endsWith("parent.md")) parent else "Child body"
                                    complete(
                                        Result.success(
                                            Outcome.FileRead(
                                                FileContent(path, "r1", text, text.length.toULong(), false)
                                            )
                                        )
                                    )
                                }
                                else -> error("unexpected viewer request: $intent")
                            }
                        },
                        dismiss = {},
                    )
                }
            }
        }
        compose.waitForIdle()
    }
}

private const val NATIVE_MENU_WAIT_MILLIS = 5_000L

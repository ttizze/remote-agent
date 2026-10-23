package dev.remoteagent.mobile

import android.graphics.Bitmap
import androidx.compose.foundation.layout.Column
import androidx.compose.material3.Text
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.test.captureToImage
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithContentDescription
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.platform.app.InstrumentationRegistry
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.ProviderKind
import dev.remoteagent.core.SessionRef
import dev.remoteagent.core.Snapshot
import java.io.File
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

/** Synthetic content only: verifies that channel sessions use the real native conversation components. */
class NativeMainConversationTest {
    @get:Rule val compose = createComposeRule()

    @Test
    fun coordinatorShowsNativeConversationAndRoutesComposerToTheOriginalChannel() {
        val identity = JSONObject("""{"provider":"claude","id":"chan_fixture"}""")
        val key = JSONObject().put("Session", JSONObject().put("session", identity))
        val session = SessionRef(ProviderKind.CLAUDE, "chan_fixture")
        val thread =
            JSONObject()
                .put("id", identity)
                .put("name", "メイン会話 · BEX")
                .put("canAcceptDirectInput", true)
                .put("status", "idle")
                .put(
                    "capabilities",
                    JSONObject("""{"fork":false,"additionalInput":true,"rename":false,"modelChange":false}"""),
                )
                .put(
                    "turns",
                    JSONArray()
                        .put(
                            JSONObject()
                                .put("id", "human")
                                .put("status", "completed")
                                .put("items", JSONArray().put(messageItem("question", "実装方針を教えて", true)))
                        )
                        .put(
                            JSONObject()
                                .put("id", "assistant")
                                .put("status", "completed")
                                .put("items", JSONArray().put(messageItem("answer", "## 実装方針\n\nBEXの会話画面で読み書きできます。")))
                        ),
                )
        val state =
            JSONObject(Snapshot.empty().serialize().decodeToString())
                .put("navigation", JSONObject().put("thread_id", identity).put("draft_key", key).put("cwd", ""))
                .put("conversations", JSONArray().put(JSONArray().put(identity).put(thread)))
                .put(
                    "drafts",
                    JSONArray()
                        .put(
                            JSONArray()
                                .put(key)
                                .put(
                                    JSONObject()
                                        .put("text", "続けてください")
                                        .put("attachments", JSONArray())
                                        .put("model", JSONObject("""{"provider":"claude","id":"claude-opus"}"""))
                                )
                        ),
                )
        Snapshot.restore(state.toString().encodeToByteArray()).use { snapshot ->
            val projection = projectConversationRows(snapshot, snapshot.conversation(session), null)
            val intents = mutableListOf<Intent>()
            val perform: (Intent, (Result<Outcome>) -> Unit) -> Unit = { intent, done ->
                intents.add(intent)
                done(Result.success(Outcome.Applied))
            }
            compose.setContent {
                BexTheme {
                    Column {
                        ConversationHeader("メイン会話 · BEX", {}, {})
                        ThreadDetailScreen(snapshot, projection, perform, null) { onSend ->
                            ThreadComposer(snapshot, perform, onSend) { Text("添付操作") }
                        }
                    }
                }
            }
            compose.onNodeWithText("メイン会話 · BEX").assertIsDisplayed()
            compose.onNodeWithText("BEXの会話画面で読み書きできます。").assertIsDisplayed()
            compose.onNodeWithText("続けてください").assertIsDisplayed()
            compose.onNodeWithText("プロジェクトの設定を使用").assertIsDisplayed()
            compose.onNodeWithText("添付操作").assertDoesNotExist()
            compose.onNodeWithTag("model.settings").assertDoesNotExist()
            compose.onAllNodesWithContentDescription("分岐").assertCountEquals(0)
            compose.onAllNodesWithContentDescription("コピー").assertCountEquals(2)
            val instrumentation = InstrumentationRegistry.getInstrumentation()
            val screenshot = compose.onRoot().captureToImage().asAndroidBitmap()
            val colors = mutableSetOf<Int>()
            for (y in 0 until screenshot.height step 8) {
                for (x in 0 until screenshot.width step 8) colors.add(screenshot.getPixel(x, y))
            }
            assertTrue("Native conversation screenshot must contain rendered content", colors.size > 4)
            File(instrumentation.targetContext.getExternalFilesDir(null), "native-main-conversation.png").outputStream().use {
                screenshot.compress(Bitmap.CompressFormat.PNG, 100, it)
            }
            screenshot.recycle()
            compose.onNodeWithContentDescription("送信").performClick()
            compose.runOnIdle {
                val sent = intents.filterIsInstance<Intent.Submit>().single()
                assertEquals(session, sent.threadId)
                assertTrue(sent.clientUserMessageId.isNotBlank())
            }
        }
    }
}

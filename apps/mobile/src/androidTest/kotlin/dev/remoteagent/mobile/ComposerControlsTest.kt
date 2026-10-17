package dev.remoteagent.mobile

import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import dev.remoteagent.core.Attachment
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Snapshot
import java.io.IOException
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

class ComposerControlsTest {
    @get:Rule val compose = createComposeRule()

    @Test
    fun existingConversationKeepsItsProviderAndDispatchesModelAndEffortChoices() {
        val identity = JSONObject("""{"provider":"codex","id":"fixture"}""")
        val key = JSONObject().put("Session", JSONObject().put("session", identity))
        val state =
            JSONObject(Snapshot.empty().serialize().decodeToString())
                .put("navigation", JSONObject().put("thread_id", identity).put("draft_key", key).put("cwd", "/fixture"))
                .put(
                    "models",
                    JSONArray(
                        """[
              {"id":"one","model":{"provider":"codex","id":"one"},"displayName":"Fixture One","defaultReasoningEffort":"low","supportedReasoningEfforts":[{"reasoningEffort":"low"},{"reasoningEffort":"high"}]},
              {"id":"gpt-6.1-sol","model":{"provider":"codex","id":"gpt-6.1-sol"},"displayName":"GPT-6.1","defaultReasoningEffort":"low","supportedReasoningEfforts":[]},
              {"id":"claude","model":{"provider":"claude","id":"claude"},"displayName":"Other Provider","defaultReasoningEffort":"low","supportedReasoningEfforts":[]}
            ]"""
                    ),
                )
                .put(
                    "drafts",
                    JSONArray()
                        .put(
                            JSONArray()
                                .put(key)
                                .put(
                                    JSONObject(
                                        """{"text":"draft","attachments":[],"model":{"provider":"codex","id":"one"},"effort":"low"}"""
                                    )
                                )
                        ),
                )
        Snapshot.restore(state.toString().encodeToByteArray()).use { snapshot ->
            val intents = mutableListOf<Intent>()
            compose.setContent { MaterialTheme { ModelPickerButton(snapshot, { intent, _ -> intents.add(intent) }) } }
            compose.onNodeWithTag("model.settings").performClick()
            compose.onNodeWithText("Other Provider").assertDoesNotExist()
            compose.onNodeWithText("GPT-6.1").performClick()
            compose.onNodeWithText("高").performClick()
            compose.runOnIdle {
                assertTrue(intents.any { it is Intent.LoadModels })
                assertTrue(intents.any { it is Intent.SelectModel && it.model.id == "gpt-6.1-sol" })
                assertTrue(intents.any { it is Intent.SelectEffort })
            }
        }
    }

    @Test
    fun attachmentOpensAuthenticatedPreviewAndShowsTransferFailure() {
        Snapshot.empty().use { snapshot ->
            val intents = mutableListOf<Intent>()
            compose.setContent {
                MaterialTheme {
                    DraftAttachments(
                        listOf(Attachment("/fixture/notes.txt", "notes.txt", false)),
                        snapshot.navigation().draftKey,
                        { intent, complete ->
                            intents.add(intent)
                            complete(Result.failure(IOException("preview transfer unavailable")))
                        },
                    )
                }
            }
            compose.onNodeWithText("タップしてプレビュー", substring = true).performClick()
            compose.onNodeWithText("preview transfer unavailable").assertIsDisplayed()
            compose.runOnIdle { assertTrue(intents.any { it is Intent.DownloadFile }) }
        }
    }
}

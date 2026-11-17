package dev.remoteagent.mobile

import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import dev.remoteagent.core.Attachment
import dev.remoteagent.core.DraftKey
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.Presentation
import dev.remoteagent.core.ProviderKind
import dev.remoteagent.core.SessionRef
import dev.remoteagent.core.Snapshot
import java.io.IOException
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

class ComposerControlsTest {
    @get:Rule val compose = createComposeRule()

    @Test
    fun existingConversationOffersProviderSwitchingAndDispatchesSettingsChoices() {
        val identity = JSONObject("""{"provider":"codex","id":"fixture"}""")
        val key = JSONObject().put("Session", JSONObject().put("session", identity))
        val state =
            JSONObject(Snapshot.empty().serialize().decodeToString())
                .put("connected", true)
                .put("navigation", JSONObject().put("thread_id", identity).put("draft_key", key).put("cwd", "/fixture"))
                .put(
                    "models",
                    JSONArray(
                        """[
              {"id":"one","model":{"provider":"codex","id":"one"},"displayName":"Fixture One", "settings": {"defaultReasoningEffort":"low","supportedReasoningEfforts":[{"reasoningEffort":"low"},{"reasoningEffort":"high"}]}},
              {"id":"gpt-6.1-sol","model":{"provider":"codex","id":"gpt-6.1-sol"},"displayName":"GPT-6.1", "settings": {"defaultReasoningEffort":"low","supportedReasoningEfforts":[]}},
              {"id":"claude","model":{"provider":"claude","id":"claude"},"displayName":"Other Provider", "settings": {"defaultReasoningEffort":"low","supportedReasoningEfforts":[]}}
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
            val presentation = Presentation.fromSnapshot(snapshot, "")
            val perform: (Intent, (Result<Outcome>) -> Unit) -> Unit = { intent, _ -> intents.add(intent) }
            compose.setContent {
                MaterialTheme {
                    ModelPickerButton(presentation.composer(), perform) { selected, dismiss ->
                        ModelPickerDialog(presentation.settings(), selected, perform, dismiss)
                    }
                }
            }
            compose.onNodeWithTag("model.settings").performClick()
            compose.onNodeWithText("この設定を新規チャットの既定にする").assertDoesNotExist()
            compose.onNodeWithText("完了").performClick()
            compose.runOnIdle { assertEquals(listOf(Intent.RefreshModels), intents) }
            compose.onNodeWithTag("model.settings").performClick()
            compose.onNodeWithText("Other Provider").performClick()
            compose.onNodeWithText("GPT-6.1").performClick()
            compose.onNodeWithText("高").performClick()
            compose.runOnIdle {
                assertTrue(intents.any { it is Intent.RefreshModels })
                assertTrue(intents.any { it is Intent.SelectModel && it.model.provider == ProviderKind.CLAUDE })
                assertTrue(intents.any { it is Intent.SelectModel && it.model.id == "gpt-6.1-sol" })
                assertEquals(
                    Intent.SelectEffort(DraftKey.Session(SessionRef(ProviderKind.CODEX, "fixture")), "high"),
                    intents.last(),
                )
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
                        dev.remoteagent.core.Presentation.fromSnapshot(snapshot, "")
                            .conversationContext()
                            .navigation()
                            .draftKey,
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

package dev.remoteagent.mobile

import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Interrupt
import dev.remoteagent.core.OperationPhase
import dev.remoteagent.core.ProviderKind
import dev.remoteagent.core.SessionRef
import dev.remoteagent.core.Snapshot
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

class ComposerStopTest {
    @get:Rule val compose = createComposeRule()

    private fun fixture(status: String, draft: String = ""): Snapshot {
        val identity = JSONObject("""{"provider":"codex","id":"thread"}""")
        val key = JSONObject().put("Session", JSONObject().put("session", identity))
        val state =
            JSONObject(Snapshot.empty().serialize().decodeToString())
                .put("connected", true)
                .put("navigation", JSONObject().put("thread_id", identity).put("draft_key", key).put("cwd", "/fixture"))
                .put(
                    "drafts",
                    JSONArray()
                        .put(JSONArray().put(key).put(JSONObject().put("text", draft).put("attachments", JSONArray()))),
                )
                .put(
                    "conversations",
                    JSONArray()
                        .put(
                            JSONArray()
                                .put(identity)
                                .put(
                                    JSONObject()
                                        .put("id", identity)
                                        .put(
                                            "turns",
                                            JSONArray()
                                                .put(
                                                    JSONObject()
                                                        .put("id", "active")
                                                        .put("status", status)
                                                        .put("items", JSONArray())
                                                ),
                                        )
                                )
                        ),
                )
        return Snapshot.restore(state.toString().encodeToByteArray())
    }

    @Test
    fun stopPhaseDisablesActionAndFailureCanRetry() {
        val intents = mutableListOf<Intent>()
        var phase by mutableStateOf<OperationPhase?>(null)
        compose.setContent {
            MaterialTheme {
                ComposerStopButton(
                    Interrupt(SessionRef(ProviderKind.CODEX, "thread"), "active"),
                    phase,
                    phase !is OperationPhase.Running,
                    { intent, _ ->
                        intents.add(intent)
                        phase = OperationPhase.Running
                    },
                )
            }
        }
        compose.onNodeWithTag("composer.stop").assertIsDisplayed().assertIsEnabled().performClick()
        compose.onNodeWithTag("composer.stop").assertIsNotEnabled()
        compose.runOnIdle {
            val stop = intents.single() as Intent.Interrupt
            assertEquals("thread", stop.v1.threadId.id)
            assertEquals("active", stop.v1.turnId)
            phase = OperationPhase.Failed("offline")
        }
        compose.onNodeWithText("停止できませんでした。再試行してください。").assertIsDisplayed()
        compose.onNodeWithTag("composer.stop").assertIsEnabled().performClick()
        compose.runOnIdle { assertEquals(2, intents.size) }
    }

    @Test
    fun runningTurnKeepsStopAvailableWhileADraftIsBeingEdited() {
        fixture("running", "keep this draft").use { snapshot ->
            compose.setContent { MaterialTheme { ComposerFixture(snapshot, { _, _ -> }, {}) {} } }
            compose.onNodeWithTag("composer.stop").assertIsDisplayed().assertIsEnabled()
            compose.runOnIdle {
                assertEquals(
                    "keep this draft",
                    snapshot
                        .draft(
                            dev.remoteagent.core.Presentation.fromSnapshot(snapshot, "")
                                .conversationContext()
                                .navigation()
                                .draftKey
                        )
                        .text,
                )
            }
        }
    }

    @Test
    fun finishedTurnReturnsToSendControl() {
        fixture("interrupted").use { snapshot ->
            compose.setContent { MaterialTheme { ComposerFixture(snapshot, { _, _ -> }, {}) {} } }
            compose.onNodeWithTag("composer.stop").assertDoesNotExist()
            compose.onNodeWithContentDescription("送信", useUnmergedTree = true).assertIsDisplayed()
            compose.runOnIdle {
                assertTrue(
                    snapshot
                        .draft(
                            dev.remoteagent.core.Presentation.fromSnapshot(snapshot, "")
                                .conversationContext()
                                .navigation()
                                .draftKey
                        )
                        .text
                        .isEmpty()
                )
            }
        }
    }
}

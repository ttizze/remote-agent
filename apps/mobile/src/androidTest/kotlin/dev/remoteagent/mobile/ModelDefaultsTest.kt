package dev.remoteagent.mobile

import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import dev.remoteagent.core.AgentStore
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.Snapshot
import java.io.IOException
import kotlinx.coroutines.runBlocking
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

class ModelDefaultsTest {
    @get:Rule val compose = createComposeRule()

    private fun fixture(): Snapshot {
        val key = JSONObject().put("Local", JSONObject().put("key", "new:/original"))
        val state =
            JSONObject(Snapshot.empty().serialize().decodeToString())
                .put("navigation", JSONObject().put("draft_key", key).put("cwd", "/original"))
                .put(
                    "models",
                    JSONArray(
                        """[{"id":"sol","model":{"provider":"codex","id":"sol"},"displayName":"Sol", "settings": {
                        "defaultReasoningEffort":"low","supportedReasoningEfforts":[{"reasoningEffort":"low"},{"reasoningEffort":"medium"}],
                        "serviceTiers":[{"id":"priority","name":"Fast","fast":true}]}}]"""
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
                                        """{"text":"keep draft","attachments":[],"model":{"provider":"codex","id":"sol"},
                                "effort":"medium","service_tier":"priority"}"""
                                    )
                                )
                        ),
                )
        return Snapshot.restore(state.toString().encodeToByteArray())
    }

    @Test
    fun savedPickerSettingsSurviveRestartAndSeedNewChats() = runBlocking {
        fixture().use { selected ->
            val store = AgentStore.offline(selected.serializeLocalState(), null)
            try {
                compose.setContent {
                    MaterialTheme {
                        ModelPickerButton(
                            selected,
                            { intent, complete ->
                                complete(runBlocking { runCatching { store.dispatch(intent).wait() } })
                            },
                        )
                    }
                }
                compose.onNodeWithTag("model.settings").performClick()
                compose.onNodeWithTag("model.save-defaults").performScrollTo().performClick()
                compose.onNodeWithText("新規チャットの既定に保存しました").performScrollTo().assertIsDisplayed()
                val restored = AgentStore.offline(store.snapshot().serializeLocalState(), null)
                try {
                    restored.dispatch(Intent.NewChat("/another-project")).wait()
                    val snapshot = restored.snapshot()
                    val draft = snapshot.draft(snapshot.navigation().draftKey)
                    assertEquals("sol", draft.model?.id)
                    assertEquals("medium", draft.effort)
                    assertEquals("priority", draft.serviceTier)
                    assertEquals("keep draft", snapshot.draft(selected.navigation().draftKey).text)
                } finally {
                    restored.shutdown()
                }
            } finally {
                store.shutdown()
            }
        }
    }

    @Test
    fun failedSaveStopsAndDoesNotReportSuccess() {
        fixture().use { selected ->
            val intents = mutableListOf<Intent>()
            compose.setContent {
                MaterialTheme {
                    ModelPickerButton(
                        selected,
                        { intent, complete ->
                            intents.add(intent)
                            if (intent is Intent.SaveDraftModelDefaults)
                                complete(Result.failure(IOException("保存できません")))
                            else complete(Result.success(Outcome.Applied))
                        },
                    )
                }
            }
            compose.onNodeWithTag("model.settings").performClick()
            compose.onNodeWithTag("model.save-defaults").performScrollTo().performClick()
            compose.onNodeWithText("保存できません").performScrollTo().assertIsDisplayed()
            compose.onNodeWithText("新規チャットの既定に保存しました").assertDoesNotExist()
            compose.runOnIdle { assertTrue(intents.none { it is Intent.SelectNewChatModel }) }
        }
    }
}

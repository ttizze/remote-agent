package dev.remoteagent.mobile

import androidx.compose.foundation.layout.Column
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import dev.remoteagent.core.AgentStore
import dev.remoteagent.core.Snapshot
import kotlinx.coroutines.runBlocking
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Rule
import org.junit.Test

class ConversationNavigationTest {
    @get:Rule val compose = createComposeRule()

    @Test
    fun topAndLatestReachBothEndsWithAnOversizedFinalMessage() = runBlocking<Unit> {
        val turns = JSONArray()
        repeat(20) { index ->
            val items = JSONArray()
                .put(JSONObject().put("id", "user-$index").put("type", "userMessage")
                    .put("content", JSONArray().put(JSONObject()
                        .put("type", "inputText").put("text", "Question $index"))))
                .put(JSONObject().put("id", "answer-$index").put("type", "agentMessage")
                    .put("text", if (index == 19) "Long final answer\n\n".repeat(80) else "Answer $index"))
            turns.put(JSONObject().put("id", "turn-$index").put("status", "completed").put("items", items))
        }
        val persisted = JSONObject(Snapshot.empty().serialize().decodeToString())
            .put("navigation", JSONObject()
                .put("thread_id", "thread").put("draft_key", "thread").put("cwd", "/fixture"))
            .put("conversations_v2", JSONObject()
                .put("thread", JSONObject().put("id", "thread").put("turns", turns)))
        val store = AgentStore.offline(persisted.toString().encodeToByteArray())
        try {
            val snapshot = store.snapshot()
            val projection = projectConversationRows(snapshot, snapshot.conversation("thread"), null)
            compose.setContent {
                MaterialTheme {
                    Column {
                        var topRequest by remember { mutableStateOf(0) }
                        ConversationHeader("Fixture", {}) { topRequest += 1 }
                        ThreadDetailScreen(snapshot, projection, { _, _ -> }, {}, scrollToTopRequest = topRequest) {}
                    }
                }
            }
            val latest = compose.onNodeWithContentDescription("最新のメッセージへ")
            compose.waitForIdle()
            latest.assertDoesNotExist()
            compose.onNodeWithContentDescription("会話の先頭へ").performClick()
            compose.onNodeWithText("Question 0").assertIsDisplayed()
            latest.assertIsDisplayed().performClick()
            compose.waitForIdle()
            latest.assertDoesNotExist()
            compose.onNodeWithContentDescription("会話の先頭へ").performClick()
            compose.onNodeWithText("Question 0").assertIsDisplayed()
        } finally {
            store.shutdown()
        }
    }
}

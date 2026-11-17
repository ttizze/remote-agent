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
import dev.remoteagent.core.ConversationRowContent
import dev.remoteagent.core.ProviderKind
import dev.remoteagent.core.SessionRef
import dev.remoteagent.core.Snapshot
import kotlinx.coroutines.runBlocking
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Rule
import org.junit.Test

class ConversationNavigationTest {
    @get:Rule val compose = createComposeRule()

    @Test
    fun expandingActivityKeepsItsHeaderVisibleWhileFollowingLatest() =
        runBlocking<Unit> {
            val items = JSONArray()
            repeat(40) { index ->
                items.put(
                    JSONObject(
                        """{"id":"command-$index","status":"completed","body":{"inline":{"body":{"commandExecution":{"command":"command-$index","output":"done","exitCode":0}}}}}"""
                    )
                )
            }
            val session = SessionRef(ProviderKind.CODEX, "activity")
            val identity = JSONObject("""{"provider":"codex","id":"activity"}""")
            val thread =
                JSONObject()
                    .put("id", identity)
                    .put(
                        "turns",
                        JSONArray().put(JSONObject().put("id", "turn").put("status", "completed").put("items", items)),
                    )
            val state =
                JSONObject(Snapshot.empty().serialize().decodeToString())
                    .put(
                        "navigation",
                        JSONObject()
                            .put("thread_id", identity)
                            .put("cwd", "/fixture")
                            .put("draft_key", JSONObject().put("Session", JSONObject().put("session", identity))),
                    )
                    .put("conversations", JSONArray().put(JSONArray().put(identity).put(thread)))
            Snapshot.restore(state.toString().encodeToByteArray()).use { snapshot ->
                val projection = requireNotNull(projectConversationRows(snapshot))
                val summary =
                    projection
                        .turnRows()
                        .map { it.data().content }
                        .filterIsInstance<ConversationRowContent.ActivityHeader>()
                        .single()
                        .activity
                        .activitySummary
                compose.setContent { MaterialTheme { ThreadDetailFixture(snapshot, projection, { _, _ -> }) {} } }
                compose.waitForIdle()
                val header = compose.onNodeWithText(summary, substring = true)
                header.assertIsDisplayed().performClick()
                compose.waitForIdle()
                header.assertIsDisplayed()
                // Core's T3 timeline has a separate disclosure for the tool group.
                compose.onNodeWithText("40 件のコマンドを実行しました").assertIsDisplayed().performClick()
                compose.waitForIdle()
                header.assertIsDisplayed()
                compose.onNodeWithText("command-0", substring = true).assertIsDisplayed()
            }
        }

    @Test
    fun topAndLatestReachBothEndsWithAnOversizedFinalMessage() =
        runBlocking<Unit> {
            val turns = JSONArray()
            repeat(20) { index ->
                val items =
                    JSONArray()
                        .put(messageItem("user-$index", "Question $index", user = true))
                        .put(
                            messageItem(
                                "answer-$index",
                                if (index == 19) "Long final answer\n\n".repeat(80) else "Answer $index",
                            )
                        )
                turns.put(JSONObject().put("id", "turn-$index").put("status", "completed").put("items", items))
            }
            val session = SessionRef(ProviderKind.CODEX, "thread")
            val identity = JSONObject("""{"provider":"codex","id":"thread"}""")
            val persisted =
                JSONObject(Snapshot.empty().serialize().decodeToString())
                    .put(
                        "navigation",
                        JSONObject()
                            .put("thread_id", identity)
                            .put("draft_key", JSONObject().put("Session", JSONObject().put("session", identity)))
                            .put("cwd", "/fixture"),
                    )
                    .put(
                        "conversations",
                        JSONArray()
                            .put(JSONArray().put(identity).put(JSONObject().put("id", identity).put("turns", turns))),
                    )
            Snapshot.restore(persisted.toString().encodeToByteArray()).use { snapshot ->
                val projection = projectConversationRows(snapshot)
                compose.setContent {
                    MaterialTheme {
                        Column {
                            var topRequest by remember { mutableStateOf(0) }
                            ConversationHeader("Fixture", {}) { topRequest += 1 }
                            ThreadDetailFixture(
                                snapshot,
                                projection,
                                { _, _ -> },
                                scrollToTopRequest = topRequest,
                            ) {}
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
            }
        }
}

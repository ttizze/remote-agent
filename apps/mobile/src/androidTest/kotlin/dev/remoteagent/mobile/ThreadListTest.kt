package dev.remoteagent.mobile

import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.semantics.ProgressBarRangeInfo
import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.hasProgressBarRangeInfo
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import dev.remoteagent.core.Snapshot
import org.json.JSONObject
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

class ThreadListTest {
    @get:Rule val compose = createComposeRule()

    @Test
    fun mergeMarksCoexistWithRunningAndUnreadUsingTheCoreAdapter() {
        val persisted = JSONObject(Snapshot.empty().serialize().decodeToString())
            .put("threads", JSONObject("""{
                "data":[
                    {"id":"running","name":"Running worktree","worktreeMerged":true,"status":{"type":"active"}},
                    {"id":"unread","name":"Unread worktree","worktreeMerged":true}
                ],
                "projects":[],"moreProjectIds":[],"hasMoreChats":false,"hasMoreProjects":false
            }"""))
            .put("activity", JSONObject("""{"active":{"running":true},"unread":["unread"]}"""))
        // Render a live snapshot: opening an offline Store intentionally clears running status.
        Snapshot.restore(persisted.toString().encodeToByteArray()).use { snapshot ->
            val list = snapshot.threadList()
            assertTrue(list!!.threads.first().active)
            compose.setContent {
                MaterialTheme { ThreadListScreen(list, snapshot.listQuery(), {}, {}, {}) }
            }
            compose.onNodeWithText("Running worktree").assertIsDisplayed()
            compose.onNodeWithText("● 完了・未確認").assertIsDisplayed()
            val marks = compose.onAllNodesWithContentDescription("main にマージ済み", useUnmergedTree = true)
            marks.assertCountEquals(2)
            val running = compose.onNode(hasProgressBarRangeInfo(ProgressBarRangeInfo.Indeterminate), useUnmergedTree = true)
            running.assertIsDisplayed()
            assertTrue(marks[0].fetchSemanticsNode().boundsInRoot.left > running.fetchSemanticsNode().boundsInRoot.right)
        }
    }
}

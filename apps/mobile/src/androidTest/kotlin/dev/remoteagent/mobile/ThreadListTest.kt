package dev.remoteagent.mobile

import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.semantics.ProgressBarRangeInfo
import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.hasProgressBarRangeInfo
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithContentDescription
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import dev.remoteagent.core.Intent
import dev.remoteagent.core.ProviderKind
import dev.remoteagent.core.SessionRef
import dev.remoteagent.core.Snapshot
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

class ThreadListTest {
    @get:Rule val compose = createComposeRule()

    @Test
    fun worktreeMarksCoexistWithRunningAndUnreadUsingTheCoreAdapter() {
        val persisted =
            JSONObject(Snapshot.empty().serialize().decodeToString())
                .put(
                    "threads",
                    JSONObject(
                        """{
                "data":[
                    {"id":{"provider":"codex","id":"running"},"name":"Running worktree","worktreeStatus":"merged","status":"running"},
                    {"id":{"provider":"codex","id":"unread"},"name":"Unread worktree","worktreeStatus":"merged"},
                    {"id":{"provider":"claude","id":"pending"},"name":"Pending worktree","worktreeStatus":"unmerged","status":"running"},
                    {"id":{"provider":"claude","id":"pending-unread"},"name":"Pending unread worktree","worktreeStatus":"unmerged"},
                    {"id":{"provider":"claude","id":"running"},"name":"Claude conversation"}
                ],
                "projects":[],"moreProjectIds":[],"hasMoreChats":false,"hasMoreProjects":false
            }"""
                    ),
                )
                .put(
                    "activity",
                    JSONObject(
                        """{"active":[[{"provider":"codex","id":"running"},true],[{"provider":"claude","id":"pending"},true]],"unread":[{"provider":"codex","id":"unread"},{"provider":"claude","id":"pending-unread"}]}"""
                    ),
                )
        // Render a live snapshot: opening an offline Store intentionally clears running status.
        Snapshot.restore(persisted.toString().encodeToByteArray()).use { snapshot ->
            val list = snapshot.threadList()
            assertTrue(list!!.threads.first().active)
            assertFalse(list.threads.single { it.title == "Claude conversation" }.active)
            val opened = mutableListOf<SessionRef>()
            compose.setContent {
                MaterialTheme {
                    ThreadListScreen(
                        list,
                        snapshot.listQuery(),
                        {},
                        {},
                        { opened.add((it as Intent.ReadThread).v1.threadId) },
                    )
                }
            }
            compose.onNodeWithText("Running worktree").assertIsDisplayed()
            compose.onAllNodesWithText("● 完了・未確認").assertCountEquals(2)
            val marks = compose.onAllNodesWithContentDescription("main にマージ済み", useUnmergedTree = true)
            marks.assertCountEquals(2)
            val running =
                compose.onAllNodes(hasProgressBarRangeInfo(ProgressBarRangeInfo.Indeterminate), useUnmergedTree = true)
            running.assertCountEquals(2)
            assertTrue(
                marks[0].fetchSemanticsNode().boundsInRoot.left > running[0].fetchSemanticsNode().boundsInRoot.right
            )
            val pending = compose.onAllNodesWithContentDescription("main に未反映の変更あり", useUnmergedTree = true)
            pending.assertCountEquals(2)
            assertTrue(
                pending[0].fetchSemanticsNode().boundsInRoot.left > running[1].fetchSemanticsNode().boundsInRoot.right
            )
            compose.onNodeWithText("Running worktree").performClick()
            compose.onNodeWithText("Claude conversation").assertIsDisplayed().performClick()
            assertEquals(
                listOf(SessionRef(ProviderKind.CODEX, "running"), SessionRef(ProviderKind.CLAUDE, "running")),
                opened,
            )
        }
    }
}

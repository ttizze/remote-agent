package dev.remoteagent.mobile

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.width
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.semantics.ProgressBarRangeInfo
import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.hasProgressBarRangeInfo
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithContentDescription
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.test.performTouchInput
import androidx.compose.ui.test.swipe
import androidx.compose.ui.unit.dp
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
    fun bothCoordinatorEntriesOpenTheOriginalSessionInTheBexConversationScreen() {
        val persisted = JSONObject(Snapshot.empty().serialize().decodeToString()).put(
            "threads", JSONObject("""{"data":[],"projects":[
                {"id":"claude-project:chan_example","name":"Cloud project","roots":[]}],
                "hasMore":false,"hasMoreProjects":false,"limit":5,"projectPages":{}}"""),
        )
        Snapshot.restore(persisted.toString().encodeToByteArray()).use { snapshot ->
            val original = snapshot.threadList()!!
            val list = original.copy(projects = original.projects.map { it.copy(expanded = true) })
            val intents = mutableListOf<Intent>()
            compose.setContent {
                BexTheme { ThreadListScreen(list, snapshot.listQuery(), false, { intents.add(it) }, {}) }
            }
            compose.onNodeWithText("プロジェクト").performClick()
            compose.onNodeWithContentDescription("Cloud projectのメイン会話を開く").performClick()
            compose.onNodeWithText("メイン会話を開く").assertIsDisplayed().performClick()
            compose.runOnIdle {
                val expected = SessionRef(ProviderKind.CLAUDE, "chan_example")
                assertEquals(listOf(expected, expected), intents.map { (it as Intent.ReadThread).v1.threadId })
                assertTrue(intents.all { (it as Intent.ReadThread).v1.open })
            }
        }
    }

    @Test
    fun recentChatsShowProjectNamesAndOpenTheOriginalConversation() {
        val persisted =
            JSONObject(Snapshot.empty().serialize().decodeToString())
                .put(
                    "threads",
                    JSONObject(
                        """{"data":[
                {"id":{"provider":"codex","id":"project-chat"},"name":"Project conversation","projectId":"bex","updatedAt":20},
                {"id":{"provider":"claude","id":"standalone"},"name":"Standalone conversation","updatedAt":10}],
                "projects":[{"id":"bex","name":"BEX project","roots":[]}],
                "hasMore":true,"hasMoreProjects":false,"limit":5,"projectPages":{}}"""
                    ),
                )
        Snapshot.restore(persisted.toString().encodeToByteArray()).use { snapshot ->
            val intents = mutableListOf<Intent>()
            compose.setContent {
                BexTheme {
                    ThreadListScreen(snapshot.recentThreadList(), snapshot.listQuery(), false, { intents.add(it) }, {})
                }
            }
            compose.onNodeWithText("Project conversation").assertIsDisplayed()
            compose.onNodeWithText("BEX project").assertIsDisplayed()
            compose.onNodeWithText("Standalone conversation").assertIsDisplayed()
            compose.onNodeWithText("Project conversation").performClick()
            assertEquals(
                SessionRef(ProviderKind.CODEX, "project-chat"),
                (intents.single() as Intent.ReadThread).v1.threadId,
            )
            compose.onNodeWithText("もっと見る").performClick()
            assertEquals(Intent.ExpandRecentChats, intents.last())
        }
    }

    @Test
    fun projectHandlesDragBothDirectionsAndCancelWithoutOpeningChats() {
        val persisted =
            JSONObject(Snapshot.empty().serialize().decodeToString())
                .put(
                    "threads",
                    JSONObject(
                        """{"data":[],"projects":[
                {"id":"a","name":"Alpha","roots":[]},
                {"id":"b","name":"Beta","roots":[]}],
                "hasMore":false,"hasMoreProjects":false,"limit":5,"projectPages":{}}"""
                    ),
                )
        Snapshot.restore(persisted.toString().encodeToByteArray()).use { snapshot ->
            val intents = mutableListOf<Intent>()
            compose.setContent {
                BexTheme {
                    ThreadListScreen(snapshot.threadList(), snapshot.listQuery(), false, { intents.add(it) }, {})
                }
            }
            compose.onNodeWithText("プロジェクト").performClick()
            val alpha = compose.onNodeWithContentDescription("Alphaの順番を変更。上下にドラッグ")
            val beta = compose.onNodeWithContentDescription("Betaの順番を変更。上下にドラッグ")
            val distance =
                beta.fetchSemanticsNode().boundsInRoot.center.y - alpha.fetchSemanticsNode().boundsInRoot.center.y
            compose.mainClock.autoAdvance = false
            alpha.performTouchInput {
                down(center)
                moveTo(center + Offset(0f, distance * 0.6f), 100)
            }
            compose.mainClock.advanceTimeBy(32)
            val carried = compose.onNodeWithTag("dragged-project", useUnmergedTree = true)
            val firstTop = carried.assertIsDisplayed().fetchSemanticsNode().boundsInRoot.top
            alpha.performTouchInput { moveTo(center + Offset(0f, distance * 0.6f + 20f), 50) }
            compose.mainClock.advanceTimeBy(32)
            val nextTop = carried.fetchSemanticsNode().boundsInRoot.top
            assertEquals(20f, nextTop - firstTop, 1f)
            compose.runOnIdle { assertTrue(intents.isEmpty()) }
            alpha.performTouchInput {
                moveTo(center + Offset(0f, distance + 10f), 100)
                up()
            }
            compose.mainClock.autoAdvance = true
            carried.assertDoesNotExist()
            compose.runOnIdle { assertEquals(Intent.ReorderProject("a", "b", true), intents.single()) }
            beta.performTouchInput { swipe(center, center - Offset(0f, distance + 10f), 500) }
            compose.runOnIdle { assertEquals(Intent.ReorderProject("b", "a", false), intents.last()) }
            alpha.performTouchInput {
                down(center)
                moveTo(center + Offset(0f, distance), 500)
                cancel()
            }
            carried.assertDoesNotExist()
            compose.runOnIdle { assertEquals(2, intents.size) }
        }
    }

    @Test
    fun workspaceTabsAndNewChatKeepExistingIntentNavigation() {
        Snapshot.empty().use { snapshot ->
            val intents = mutableListOf<Intent>()
            var openedHosts = false
            compose.setContent {
                BexTheme {
                    Box(Modifier.width(320.dp)) {
                        ThreadListScreen(
                            snapshot.threadList(),
                            snapshot.listQuery(),
                            false,
                            { intents.add(it) },
                            { openedHosts = true },
                            hostName = "Development PC",
                            connected = true,
                        )
                    }
                }
            }
            compose.onNodeWithText("ワークスペース").assertDoesNotExist()
            compose.onNodeWithContentDescription("接続中").assertIsDisplayed()
            val header =
                listOf(
                        compose.onNodeWithText("bex"),
                        compose.onNodeWithText("Development PC"),
                        compose.onNodeWithContentDescription("検索"),
                        compose.onNodeWithContentDescription("更新"),
                    )
                    .map { it.assertIsDisplayed().fetchSemanticsNode().boundsInRoot }
            header.forEach { assertEquals(header.first().center.y, it.center.y, 1f) }
            compose.onNodeWithText("Development PC").performClick()
            assertTrue(openedHosts)
            compose.onNodeWithText("プロジェクト").performClick()
            compose.onNodeWithText("最近のチャット").performClick()
            compose.onNodeWithText("新しいチャット").performClick()
            assertTrue(intents.last() is Intent.NewChat)
        }
    }

    @Test
    fun searchOpensOnDemandAndCloseClearsFilter() {
        Snapshot.empty().use { snapshot ->
            val intents = mutableListOf<Intent>()
            var query by mutableStateOf(snapshot.listQuery())
            compose.setContent {
                BexTheme {
                    ThreadListScreen(
                        snapshot.threadList(),
                        query,
                        false,
                        { intent ->
                            intents.add(intent)
                            if (intent is Intent.ListSessions) query = intent.v1.query
                        },
                        {},
                    )
                }
            }
            compose.onNodeWithText("チャットを検索").assertDoesNotExist()
            compose.onNodeWithContentDescription("検索").performClick()
            compose.onNodeWithText("チャットを検索").performTextInput("BEX")
            compose.waitUntil(5_000) {
                intents.filterIsInstance<Intent.ListSessions>().lastOrNull()?.v1?.query?.searchTerm == "BEX"
            }
            compose.onNodeWithContentDescription("検索を閉じる").performClick()
            compose.onNodeWithText("チャットを検索").assertDoesNotExist()
            compose.waitUntil(5_000) {
                intents.filterIsInstance<Intent.ListSessions>().lastOrNull()?.v1?.query?.searchTerm == ""
            }
            compose.onNodeWithContentDescription("検索").assertIsDisplayed()
        }
    }

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
                "projects":[],"hasMore":false,"hasMoreProjects":false,"limit":5,"projectPages":{}}"""
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
                        false,
                        { intent -> if (intent is Intent.ReadThread) opened.add(intent.v1.threadId) },
                        {},
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

package dev.remoteagent.mobile

import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextReplacement
import dev.remoteagent.core.Intent
import dev.remoteagent.core.ProjectSummary
import dev.remoteagent.core.Snapshot
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test

class ProjectActionsTest {
    @get:Rule val compose = createComposeRule()

    @Test
    fun removingAProjectDispatchesOnlyTheReversibleCoreAction() {
        val project = project()
        val intents = mutableListOf<Intent>()
        compose.setContent { MaterialTheme { ProjectActions(project, intents::add) } }
        compose.onNodeWithContentDescription("Alphaの操作").assertIsDisplayed().performClick()
        compose.onNodeWithText("一覧から削除").performClick()
        compose.runOnIdle { assertEquals(listOf(Intent.SetProjectHidden("a", true)), intents) }
    }

    @Test
    fun pinAndUnpinUseTheCurrentCoreProjection() {
        var project by mutableStateOf(project())
        val intents = mutableListOf<Intent>()
        compose.setContent { MaterialTheme { ProjectActions(project, intents::add) } }
        compose.onNodeWithContentDescription("Alphaの操作").performClick()
        compose.onNodeWithText("ピン留め").performClick()
        compose.runOnIdle { project = project.copy(pinned = true) }
        compose.onNodeWithContentDescription("Alphaの操作").performClick()
        compose.onNodeWithText("ピン留めを解除").performClick()
        compose.runOnIdle {
            assertEquals(
                listOf(Intent.SetProjectPinned("a", true), Intent.SetProjectPinned("a", false)),
                intents,
            )
        }
    }

    @Test
    fun renameValidatesUnicodeLengthPreservesTheSectionAndCanBeCancelled() {
        val project = project().copy(section = "Work")
        val intents = mutableListOf<Intent>()
        compose.setContent { MaterialTheme { ProjectActions(project, intents::add) } }
        compose.onNodeWithContentDescription("Alphaの操作").performClick()
        compose.onNodeWithText("名前を変更").performClick()
        val field = compose.onNodeWithText("プロジェクト名")
        field.performTextReplacement("   ")
        compose.onNodeWithText("保存").assertIsNotEnabled()
        field.performTextReplacement("a".repeat(101))
        compose.onNodeWithText("保存").assertIsNotEnabled()
        val name = "?".repeat(100)
        field.performTextReplacement(" $name ")
        compose.onNodeWithText("保存").performClick()
        compose.onNodeWithContentDescription("Alphaの操作").performClick()
        compose.onNodeWithText("名前を変更").performClick()
        compose.onNodeWithText("キャンセル").performClick()
        compose.runOnIdle { assertEquals(listOf(Intent.EditProject("a", name, "Work")), intents) }
    }

    @Test
    fun removedProjectsCanBeRestoredFromTheList() {
        val project = project()
        val intents = mutableListOf<Intent>()
        compose.setContent { MaterialTheme { HiddenProjects(listOf(project)) { _, intent -> intents.add(intent) } } }
        compose.onNodeWithText("Alphaを戻す").assertDoesNotExist()
        compose.onNodeWithText("削除したプロジェクト（1）").performClick()
        compose.onNodeWithText("一覧から削除しても、フォルダと会話履歴は残ります。").assertIsDisplayed()
        compose.onNodeWithText("Alphaを戻す").performClick()
        compose.runOnIdle { assertEquals(listOf(Intent.SetProjectHidden("a", false)), intents) }
    }

    private fun project(): ProjectSummary {
        val persisted =
            JSONObject(Snapshot.empty().serialize().decodeToString())
                .put(
                    "threads",
                    JSONObject(
                        """{"data":[],"projects":[{"id":"a","name":"Alpha","roots":[{"path":"C:/alpha"}]}],
                        "hasMore":false,"hasMoreProjects":false,"limit":5,"projectPages":{}}"""
                    ),
                )
        return Snapshot.restore(persisted.toString().encodeToByteArray()).use { it.threadList()!!.projects.single() }
    }
}

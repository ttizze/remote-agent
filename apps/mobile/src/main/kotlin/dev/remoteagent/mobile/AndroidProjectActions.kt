package dev.remoteagent.mobile

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.Intent
import dev.remoteagent.core.ProjectSummary

private const val PROJECT_NAME_MAX_LENGTH = 100

@Composable
internal fun ProjectActions(project: ProjectSummary, perform: (Intent) -> Unit) {
    var expanded by remember(project.id) { mutableStateOf(false) }
    var editing by rememberSaveable(project.id) { mutableStateOf(false) }
    Box {
        IconButton(onClick = { expanded = true }) {
            Icon(painterResource(R.drawable.ic_more_vert), "${project.name}の操作")
        }
        DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            DropdownMenuItem(
                text = { Text(if (project.pinned) "ピン留めを解除" else "ピン留め") },
                onClick = {
                    expanded = false
                    perform(Intent.SetProjectPinned(project.id, !project.pinned))
                },
            )
            DropdownMenuItem(
                text = { Text("名前を変更") },
                onClick = {
                    expanded = false
                    editing = true
                },
            )
            HorizontalDivider()
            DropdownMenuItem(
                text = { Text("一覧から削除", color = MaterialTheme.colorScheme.error) },
                onClick = {
                    expanded = false
                    perform(Intent.SetProjectHidden(project.id, true))
                },
            )
        }
    }
    if (editing) ProjectNameDialog(project, perform) { editing = false }
}

@Composable
private fun ProjectNameDialog(
    project: ProjectSummary,
    perform: (Intent) -> Unit,
    dismiss: () -> Unit,
) {
    var name by rememberSaveable(project.id) { mutableStateOf(project.name) }
    val trimmed = name.trim()
    val valid = trimmed.isNotEmpty() && trimmed.codePointCount(0, trimmed.length) <= PROJECT_NAME_MAX_LENGTH
    AlertDialog(
        onDismissRequest = dismiss,
        title = { Text("名前を変更") },
        text = {
            OutlinedTextField(
                value = name,
                onValueChange = { name = it },
                label = { Text("プロジェクト名") },
                singleLine = true,
                isError = !valid,
                supportingText = { Text("1?100文字で入力してください") },
            )
        },
        confirmButton = {
            TextButton(
                enabled = valid,
                onClick = {
                    perform(Intent.EditProject(project.id, trimmed, project.section.orEmpty()))
                    dismiss()
                },
            ) {
                Text("保存")
            }
        },
        dismissButton = { TextButton(onClick = dismiss) { Text("キャンセル") } },
    )
}

@Composable
internal fun HiddenProjects(
    projects: List<ProjectSummary>,
    perform: (ProjectSummary, Intent) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    Column(Modifier.fillMaxWidth().padding(top = 12.dp)) {
        TextButton(onClick = { expanded = !expanded }) {
            Text(if (expanded) "削除したプロジェクトを閉じる" else "削除したプロジェクト（${projects.size}）")
        }
        if (expanded) {
            Text(
                "一覧から削除しても、フォルダと会話履歴は残ります。",
                Modifier.padding(horizontal = 12.dp, vertical = 4.dp),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            projects.forEach { project ->
                TextButton(onClick = { perform(project, Intent.SetProjectHidden(project.id, false)) }) {
                    Text("${project.name}を戻す", maxLines = 1, overflow = TextOverflow.Ellipsis)
                }
            }
        }
    }
}

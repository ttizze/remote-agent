package dev.remoteagent.mobile

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import dev.remoteagent.core.ComposerControls
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.WorkspaceSelection
import dev.remoteagent.core.WorkspaceStrategy

private const val MAX_BASE_REF_RESULTS = 8

@Composable
internal fun WorkspacePicker(controls: ComposerControls, perform: (Intent, (Result<Outcome>) -> Unit) -> Unit) {
    val selection = controls.workspace ?: return
    val language = LocalUiLanguage.current
    var base by remember { mutableStateOf(false) }
    val select: (WorkspaceStrategy) -> Unit = { workspace ->
        perform(Intent.SelectWorkspace(controls.navigation.draftKey, workspace)) {}
    }
    Row {
        WorkspaceModeMenu(selection, select)
        if (selection.mode == "worktree") {
            TextButton(onClick = { base = true }) { Text(selection.baseRef.ifEmpty { language.text("ベースブランチ") }) }
            TextButton(
                onClick = { select(WorkspaceStrategy.Worktree(selection.baseRef, !selection.startFromOrigin)) }
            ) {
                Text("${if (selection.startFromOrigin) "✓" else "○"} ${language.text("リモートから開始")}")
            }
        }
    }
    if (base) WorkspaceBaseDialog(selection, select) { base = false }
}

@Composable
private fun WorkspaceModeMenu(selection: WorkspaceSelection, select: (WorkspaceStrategy) -> Unit) {
    val language = LocalUiLanguage.current
    var menu by remember { mutableStateOf(false) }
    TextButton(onClick = { menu = true }) { Text(selection.label) }
    DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
        DropdownMenuItem(
            text = { Text(language.text("ローカル")) },
            onClick = {
                select(WorkspaceStrategy.Root)
                menu = false
            },
        )
        if (selection.isRepository) {
            DropdownMenuItem(
                text = { Text(language.text("ワークツリー")) },
                onClick = {
                    select(WorkspaceStrategy.Worktree(selection.baseRef, selection.startFromOrigin))
                    menu = false
                },
            )
            selection.worktrees.forEach { path ->
                DropdownMenuItem(
                    text = { Text(path) },
                    onClick = {
                        select(WorkspaceStrategy.ExistingWorktree(path))
                        menu = false
                    },
                )
            }
        }
    }
}

@Composable
private fun WorkspaceBaseDialog(
    selection: WorkspaceSelection,
    select: (WorkspaceStrategy) -> Unit,
    dismiss: () -> Unit,
) {
    val language = LocalUiLanguage.current
    var query by remember(selection.baseRef) { mutableStateOf(selection.baseRef) }
    fun choose(reference: String) {
        select(WorkspaceStrategy.Worktree(reference, selection.startFromOrigin))
        dismiss()
    }
    AlertDialog(
        onDismissRequest = dismiss,
        title = { Text(language.text("ベースブランチ")) },
        text = {
            Column {
                OutlinedTextField(query, { query = it }, label = { Text(language.text("ブランチ・タグ・コミット")) })
                selection.refs
                    .filter { query.isEmpty() || it.contains(query, ignoreCase = true) }
                    .take(MAX_BASE_REF_RESULTS)
                    .forEach { reference -> TextButton(onClick = { choose(reference) }) { Text(reference) } }
            }
        },
        confirmButton = {
            TextButton(onClick = { choose(query) }, enabled = query.isNotBlank()) { Text(language.text("選択")) }
        },
        dismissButton = { TextButton(onClick = dismiss) { Text(language.text("キャンセル")) } },
    )
}

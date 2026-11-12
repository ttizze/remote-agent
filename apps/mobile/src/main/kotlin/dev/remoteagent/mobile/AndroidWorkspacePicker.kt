package dev.remoteagent.mobile

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.material3.*
import androidx.compose.runtime.*
import dev.remoteagent.core.ComposerControls
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.WorkspaceStrategy

@Composable
internal fun WorkspacePicker(controls: ComposerControls, perform: (Intent, (Result<Outcome>) -> Unit) -> Unit) {
    val selection = controls.workspace ?: return
    val language = LocalUiLanguage.current
    var menu by remember { mutableStateOf(false) }
    var base by remember { mutableStateOf(false) }
    var query by remember(selection.baseRef) { mutableStateOf(selection.baseRef) }
    fun select(workspace: WorkspaceStrategy) {
        perform(Intent.SelectWorkspace(controls.navigation.draftKey, workspace)) {}
    }
    Column {
        Row {
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
            if (selection.mode == "worktree") {
                TextButton(onClick = { base = true }) { Text(selection.baseRef.ifEmpty { language.text("ベースブランチ") }) }
                TextButton(
                    onClick = { select(WorkspaceStrategy.Worktree(selection.baseRef, !selection.startFromOrigin)) }
                ) {
                    Text("${if (selection.startFromOrigin) "✓" else "○"} ${language.text("リモートから開始")}")
                }
            }
        }
        if (base)
            AlertDialog(
                onDismissRequest = { base = false },
                title = { Text(language.text("ベースブランチ")) },
                text = {
                    Column {
                        OutlinedTextField(query, { query = it }, label = { Text(language.text("ブランチ・タグ・コミット")) })
                        selection.refs
                            .filter { query.isEmpty() || it.contains(query, ignoreCase = true) }
                            .take(8)
                            .forEach { reference ->
                                TextButton(
                                    onClick = {
                                        select(WorkspaceStrategy.Worktree(reference, selection.startFromOrigin))
                                        base = false
                                    }
                                ) {
                                    Text(reference)
                                }
                            }
                    }
                },
                confirmButton = {
                    TextButton(
                        onClick = {
                            select(WorkspaceStrategy.Worktree(query, selection.startFromOrigin))
                            base = false
                        },
                        enabled = query.isNotBlank(),
                    ) {
                        Text(language.text("選択"))
                    }
                },
                dismissButton = { TextButton(onClick = { base = false }) { Text(language.text("キャンセル")) } },
            )
    }
}

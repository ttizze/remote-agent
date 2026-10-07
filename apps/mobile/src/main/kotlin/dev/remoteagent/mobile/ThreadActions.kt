package dev.remoteagent.mobile

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.Intent
import dev.remoteagent.core.MoveDestination
import dev.remoteagent.core.MoveDirection
import dev.remoteagent.core.OrderSection
import dev.remoteagent.core.ThreadAction
import dev.remoteagent.core.ThreadMenuAction
import dev.remoteagent.core.ThreadMenuConfirmation
import dev.remoteagent.core.ThreadMenuItem
import dev.remoteagent.core.ThreadMenuItemId

/** Dialogs a thread menu opens: rename, custom snooze and confirmations. */
@Stable
internal class ThreadActions {
    var renaming by mutableStateOf<Pair<String, String>?>(null)
    var snoozing by mutableStateOf<String?>(null)
    var confirming by mutableStateOf<Triple<String, ThreadMenuConfirmation, () -> Unit>?>(null)
    var arranging by mutableStateOf(false)
}

@Composable internal fun rememberThreadActions() = remember { ThreadActions() }

/** Android titles the branch item with the branch name. */
internal fun menuLabel(item: ThreadMenuItem): String {
    val action = item.action
    return if (item.id is ThreadMenuItemId.NewThreadOnBranch && action is ThreadMenuAction.NewThreadOnBranch)
        "New thread on ${action.branch}"
    else item.label
}

/** Runs a menu action for `threadId`. `onLeave` runs when the thread leaves the list (archive, delete). */
@Suppress("CyclomaticComplexMethod", "LongParameterList")
internal fun runThreadMenuAction(
    model: AndroidAppModel,
    state: ThreadActions,
    threadId: String,
    title: String,
    pinned: Boolean,
    action: ThreadMenuAction,
    copy: (String) -> Unit,
    onLeave: () -> Unit = {},
) {
    when (action) {
        is ThreadMenuAction.Thread -> {
            model.perform(Intent.Thread(threadId, action.action))
            if (action.action is ThreadAction.Delete || action.action is ThreadAction.Archive) onLeave()
        }
        is ThreadMenuAction.FilterProject -> model.perform(Intent.FilterProject(action.projectId))
        is ThreadMenuAction.NewThreadOnBranch ->
            model.newThreadOnBranch(action.projectId, action.branch, action.worktreePath)
        ThreadMenuAction.CustomSnooze -> state.snoozing = threadId
        ThreadMenuAction.StartRename -> state.renaming = threadId to title
        is ThreadMenuAction.OpenProjectSettings -> model.navigate(Route.Settings(action.projectId))
        is ThreadMenuAction.CopyPath -> action.path?.let(copy)
        is ThreadMenuAction.CopyBranch -> copy(action.branch)
        is ThreadMenuAction.CopyThreadId -> copy(action.threadId)
        ThreadMenuAction.Arrange -> state.arranging = true
        is ThreadMenuAction.Move ->
            model.perform(
                Intent.MoveThread(
                    threadId,
                    if (pinned) OrderSection.PINNED else OrderSection.ACTIVE,
                    if (action.direction == MoveDirection.UP) MoveDestination.Up else MoveDestination.Down,
                )
            )
    }
}

/** A menu item, behind its confirmation when core asks for one. */
@Suppress("LongParameterList")
internal fun selectThreadMenuItem(
    model: AndroidAppModel,
    state: ThreadActions,
    threadId: String,
    title: String,
    pinned: Boolean,
    item: ThreadMenuItem,
    copy: (String) -> Unit,
    onLeave: () -> Unit = {},
) {
    val action = item.action ?: return
    val run = { runThreadMenuAction(model, state, threadId, title, pinned, action, copy, onLeave) }
    val confirmation = item.confirmation
    if (confirmation != null) state.confirming = Triple(item.label, confirmation, run) else run()
}

@Composable
internal fun ThreadActionDialogs(model: AndroidAppModel, state: ThreadActions) {
    state.renaming?.let { (id, current) ->
        RenameDialog(current, { state.renaming = null }) { title ->
            model.perform(Intent.Thread(id, ThreadAction.Rename(title)))
        }
    }
    state.snoozing?.let { id ->
        CustomSnoozeDialog(onClose = { state.snoozing = null }) { until ->
            model.perform(Intent.Thread(id, ThreadAction.Snooze(until)))
        }
    }
    state.confirming?.let { (label, confirmation, run) ->
        ConfirmDialog(
            confirmation,
            label,
            onConfirm = {
                state.confirming = null
                run()
            },
            onDismiss = { state.confirming = null },
        )
    }
}

@Composable
private fun RenameDialog(current: String, onDismiss: () -> Unit, onRename: (String) -> Unit) {
    var title by remember { mutableStateOf(current) }
    AlertDialog(
        onDismissRequest = onDismiss,
        containerColor = AppTheme.colors.cardAlt,
        shape = RoundedCornerShape(28.dp),
        title = { Text("Rename thread", style = AppTheme.title) },
        text = { Column { SettingsField(title, { title = it }, "Thread title", Modifier.fillMaxWidth()) } },
        confirmButton = {
            TextButton(
                onClick = {
                    if (title.isNotBlank()) {
                        onRename(title.trim())
                        onDismiss()
                    }
                }
            ) {
                Text("Save", color = AppTheme.colors.foreground)
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel", color = AppTheme.colors.foreground) } },
    )
}

@Composable
internal fun rememberCopy(): (String) -> Unit {
    val context = LocalContext.current
    return remember(context) { { text -> copyText(context, text) } }
}

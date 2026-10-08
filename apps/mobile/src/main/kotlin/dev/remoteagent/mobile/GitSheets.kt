@file:Suppress("LongMethod", "CyclomaticComplexMethod", "MagicNumber")

package dev.remoteagent.mobile

import android.content.Intent as AndroidIntent
import android.net.Uri
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.AltRoute
import androidx.compose.material.icons.outlined.ArrowUpward
import androidx.compose.material.icons.outlined.CheckCircle
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material.icons.outlined.TextSnippet
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TextField
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.DefaultBranchActionCopy
import dev.remoteagent.core.GitAction
import dev.remoteagent.core.GitActionProgress
import dev.remoteagent.core.GitQuickActionKind
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Outcome

@Composable
internal fun GitOverviewSheet(model: AndroidAppModel, cwd: String, onDismiss: () -> Unit) {
    val context = LocalContext.current
    var pendingAction by remember { mutableStateOf<GitAction?>(null) }
    var confirmation by remember { mutableStateOf(false) }
    var checkoutReference by remember { mutableStateOf("") }
    var checkoutMode by remember { mutableStateOf("worktree") }
    var commitMessage by remember { mutableStateOf("") }
    var excludedFiles by remember { mutableStateOf<Set<String>>(emptySet()) }
    var editingFiles by remember { mutableStateOf(false) }
    var newBranchName by remember { mutableStateOf("") }
    var worktreeBaseBranch by remember { mutableStateOf("") }
    var worktreeBranchName by remember { mutableStateOf("") }
    var actionId by remember { mutableStateOf<String?>(null) }
    val status = model.snapshot.gitStatus(cwd)
    val localRefs = model.snapshot.gitRefs(cwd).filter { !it.isRemote }
    val menu = model.snapshot.gitMenu(cwd)
    val allFiles = status?.workingTree ?: emptyList()
    val selectedFiles = allFiles.filter { it.path !in excludedFiles }
    val allFilesSelected = excludedFiles.isEmpty()
    val progress = actionId?.let { model.snapshot.gitAction(it) }
    val copy = pendingAction?.let { action ->
        val actionName = action.name.lowercase()
        if (model.snapshot.gitRequiresDefaultBranchConfirmation(cwd, actionName)) {
            model.snapshot.gitDefaultBranchActionCopy(cwd, actionName, action.includesCommit())
        } else null
    }

    LaunchedEffect(cwd) {
        excludedFiles = emptySet()
        editingFiles = false
        if (cwd.isNotEmpty()) {
            model.perform(Intent.SubscribeVcsStatus(cwd))
            model.perform(Intent.LoadVcsRefs(cwd, ""))
            model.perform(Intent.RefreshVcsStatus(cwd))
        }
    }
    BottomSheet(onDismiss, "Git", skipPartiallyExpanded = true) {
        GitStatusCard(status, onRefresh = { model.perform(Intent.RefreshVcsStatus(cwd)) })
        GitFileSelection(
            files = allFiles,
            selectedPaths = excludedFiles,
            editing = editingFiles,
            onEditingChanged = { editingFiles = it },
            onReset = { excludedFiles = emptySet() },
            onToggle = { path ->
                excludedFiles =
                    if (path in excludedFiles) {
                        excludedFiles - path
                    } else {
                        excludedFiles + path
                    }
            },
        )
        Column(Modifier.fillMaxWidth()) {
            menu.forEach { item ->
                DropdownMenuItem(
                    text = { Text(item.label) },
                    leadingIcon = { Icon(item.action.icon(), null) },
                    onClick = {
                        val action = item.action
                        if (action == GitAction.OPEN_PR) {
                            status?.pullRequestUrl?.let { url ->
                                context.startActivity(AndroidIntent(AndroidIntent.ACTION_VIEW, Uri.parse(url)))
                            }
                        } else {
                            pendingAction = action
                            val actionName = action.name.lowercase()
                            confirmation = model.snapshot.gitRequiresDefaultBranchConfirmation(cwd, actionName)
                            if (!confirmation)
                                runGitAction(
                                    model,
                                    cwd,
                                    action,
                                    commitMessage,
                                    if (allFilesSelected) null else selectedFiles.map { it.path },
                                    false,
                                ) {
                                    actionId = it
                                }
                        }
                    },
                    enabled = !item.disabled && !(selectedFiles.isEmpty() && item.action.includesCommit()),
                )
            }
        }
        TextField(
            value = commitMessage,
            onValueChange = { commitMessage = it },
            modifier = Modifier.fillMaxWidth().padding(horizontal = 20.dp),
            label = { Text("Commit message (optional)") },
            minLines = 2,
            maxLines = 4,
        )
        val quick = model.snapshot.gitQuickAction(cwd)
        if (quick?.kind == GitQuickActionKind.PULL) {
            TextButton(onClick = { model.perform(Intent.PullVcs(cwd)) }) { Text("Pull") }
        }
        GitBranchSection(
            model = model,
            cwd = cwd,
            status = status,
            refs = localRefs,
            newBranchName = newBranchName,
            onNewBranchNameChanged = { newBranchName = it },
            worktreeBaseBranch = worktreeBaseBranch.ifEmpty { status?.refName ?: "main" },
            onWorktreeBaseBranchChanged = { worktreeBaseBranch = it },
            worktreeBranchName = worktreeBranchName,
            onWorktreeBranchNameChanged = { worktreeBranchName = it },
            onRefresh = {
                model.perform(Intent.LoadVcsRefs(cwd, ""))
                model.perform(Intent.RefreshVcsStatus(cwd))
            },
        )
        GitPullRequestThreadSection(
            reference = checkoutReference,
            mode = checkoutMode,
            onReferenceChanged = { checkoutReference = it },
            onModeChanged = { checkoutMode = it },
            onCheckout = {
                val reference = checkoutReference.trim()
                if (reference.isNotEmpty()) {
                    model.perform(
                        Intent.PreparePullRequestThread(cwd, reference, checkoutMode, model.snapshot.selectedThreadId())
                    ) { result ->
                        val prepared = (result.getOrNull() as? Outcome.GitPullRequestThreadPrepared)?.result
                        if (prepared != null) {
                            val project = model.snapshot.selectedProjectId() ?: "chats"
                            model.perform(Intent.NewThreadOnBranch(project, prepared.branch, prepared.worktreePath))
                            onDismiss()
                        }
                    }
                }
            },
        )
        progress?.let { GitProgress(it) }
    }
    if (confirmation) {
        GitConfirmationDialog(
            copy,
            onContinue = {
                confirmation = false
                pendingAction?.let { action ->
                    runGitAction(
                        model,
                        cwd,
                        action,
                        commitMessage,
                        if (allFilesSelected) null else selectedFiles.map { it.path },
                        false,
                    ) {
                        actionId = it
                    }
                }
                pendingAction = null
            },
            onFeatureBranch = {
                confirmation = false
                pendingAction?.let { action ->
                    runGitAction(
                        model,
                        cwd,
                        action,
                        commitMessage,
                        if (allFilesSelected) null else selectedFiles.map { it.path },
                        true,
                    ) {
                        actionId = it
                    }
                }
                pendingAction = null
            },
            onDismiss = {
                confirmation = false
                pendingAction = null
            },
        )
    }
}

// Branch and worktree controls keep each independent field and callback explicit.
@Suppress("LongParameterList")
@Composable
private fun GitBranchSection(
    model: AndroidAppModel,
    cwd: String,
    status: dev.remoteagent.core.GitStatus?,
    refs: List<dev.remoteagent.core.GitRef>,
    newBranchName: String,
    onNewBranchNameChanged: (String) -> Unit,
    worktreeBaseBranch: String,
    onWorktreeBaseBranchChanged: (String) -> Unit,
    worktreeBranchName: String,
    onWorktreeBranchNameChanged: (String) -> Unit,
    onRefresh: () -> Unit,
) {
    Column(
        Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 8.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text("Branches & worktrees", fontWeight = FontWeight.Bold)
        if (status?.isRepo == false) {
            Button(onClick = { model.perform(Intent.InitRepository(cwd)) { onRefresh() } }) {
                Text("Initialize repository")
            }
        } else {
            TextField(
                value = newBranchName,
                onValueChange = onNewBranchNameChanged,
                modifier = Modifier.fillMaxWidth(),
                label = { Text("New branch name") },
                singleLine = true,
            )
            Button(
                onClick = {
                    val name = newBranchName.trim()
                    if (name.isNotEmpty()) {
                        model.perform(Intent.CreateVcsRef(cwd, name)) {
                            onNewBranchNameChanged("")
                            onRefresh()
                        }
                    }
                },
                enabled = newBranchName.trim().isNotEmpty(),
            ) {
                Text("Create & checkout")
            }

            TextField(
                value = worktreeBaseBranch,
                onValueChange = onWorktreeBaseBranchChanged,
                modifier = Modifier.fillMaxWidth(),
                label = { Text("Worktree base branch") },
                singleLine = true,
            )
            TextField(
                value = worktreeBranchName,
                onValueChange = onWorktreeBranchNameChanged,
                modifier = Modifier.fillMaxWidth(),
                label = { Text("New worktree branch") },
                singleLine = true,
            )
            Button(
                onClick = {
                    val base = worktreeBaseBranch.trim()
                    val branch = worktreeBranchName.trim()
                    if (base.isNotEmpty() && branch.isNotEmpty()) {
                        model.perform(Intent.CreateVcsWorktree(cwd, base, branch, base, null)) {
                            onWorktreeBranchNameChanged("")
                            onRefresh()
                        }
                    }
                },
                enabled = worktreeBaseBranch.trim().isNotEmpty() && worktreeBranchName.trim().isNotEmpty(),
            ) {
                Icon(Icons.AutoMirrored.Outlined.AltRoute, null)
                Text("Create worktree", Modifier.padding(start = 8.dp))
            }

            if (refs.isEmpty()) {
                Text("Loading branches…", color = AppTheme.colors.foregroundMuted)
            } else {
                refs.forEach { ref ->
                    Column(Modifier.fillMaxWidth()) {
                        DropdownMenuItem(
                            text = {
                                Column {
                                    Text(ref.name)
                                    Text(
                                        when {
                                            ref.current || ref.worktreePath == cwd -> "Checked out here"
                                            ref.worktreePath != null -> "Checked out in ${ref.worktreePath}"
                                            ref.isDefault -> "Default branch"
                                            else -> "Local branch"
                                        },
                                        style = AppTheme.caption,
                                        color = AppTheme.colors.foregroundMuted,
                                    )
                                }
                            },
                            onClick = { model.perform(Intent.SwitchVcsRef(cwd, ref.name)) { onRefresh() } },
                            enabled = !ref.current && ref.worktreePath == null,
                        )
                        ref.worktreePath
                            ?.takeIf { it != cwd }
                            ?.let { path ->
                                TextButton(
                                    onClick = {
                                        model.perform(Intent.RemoveVcsWorktree(cwd, path, false)) { onRefresh() }
                                    }
                                ) {
                                    Text("Remove worktree", color = AppTheme.colors.dangerForeground)
                                }
                            }
                    }
                }
            }
        }
    }
}

@Composable
private fun GitStatusCard(status: dev.remoteagent.core.GitStatus?, onRefresh: () -> Unit) {
    val colors = AppTheme.colors
    Column(
        Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 8.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Icon(Icons.AutoMirrored.Outlined.AltRoute, null, tint = colors.primaryText)
            Column(Modifier.weight(1f)) {
                Text(status?.refName ?: "Select ref", fontWeight = FontWeight.Bold)
                Text(statusSummary(status), style = AppTheme.caption, color = colors.foregroundMuted)
            }
            androidx.compose.material3.IconButton(onClick = onRefresh) {
                Icon(Icons.Outlined.Refresh, "Refresh Git status", tint = colors.iconMuted)
            }
        }
        status?.pullRequestUrl?.let { url ->
            Text(
                url,
                style = AppTheme.caption,
                color = colors.primaryText,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

// Selection editing keeps its state and callbacks explicit for Compose ownership.
@Suppress("LongParameterList")
@Composable
private fun GitFileSelection(
    files: List<dev.remoteagent.core.GitFileChange>,
    selectedPaths: Set<String>,
    editing: Boolean,
    onEditingChanged: (Boolean) -> Unit,
    onReset: () -> Unit,
    onToggle: (String) -> Unit,
) {
    val selected = files.filter { it.path !in selectedPaths }
    Column(
        Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 8.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text("Files", fontWeight = FontWeight.Bold)
                Text(
                    "${selected.size} selected · " +
                        "+${selected.sumOf { it.insertions }} / -${selected.sumOf { it.deletions }}",
                    style = AppTheme.caption,
                    color = AppTheme.colors.foregroundMuted,
                )
            }
            if (selectedPaths.isNotEmpty()) {
                TextButton(onClick = onReset) { Text("Reset") }
            }
            if (files.isNotEmpty()) {
                TextButton(onClick = { onEditingChanged(!editing) }) { Text(if (editing) "Done" else "Edit") }
            }
        }
        if (files.isEmpty()) {
            Text("No changed files are available to commit.", color = AppTheme.colors.foregroundMuted)
        } else if (editing) {
            files.forEach { file ->
                DropdownMenuItem(
                    text = {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Icon(
                                Icons.Outlined.CheckCircle,
                                null,
                                tint =
                                    if (file.path in selectedPaths) AppTheme.colors.foregroundMuted
                                    else AppTheme.colors.primaryText,
                            )
                            Text(file.path, Modifier.weight(1f), maxLines = 1, overflow = TextOverflow.Ellipsis)
                            Text("+${file.insertions}  -${file.deletions}", style = AppTheme.caption)
                        }
                    },
                    onClick = { onToggle(file.path) },
                )
            }
        } else {
            selected.take(3).forEach { file ->
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(file.path, Modifier.weight(1f), maxLines = 1, overflow = TextOverflow.Ellipsis)
                    Text("+${file.insertions}  -${file.deletions}", style = AppTheme.caption)
                }
            }
            if (selected.size > 3) {
                Text(
                    "+${selected.size - 3} more files",
                    style = AppTheme.caption,
                    color = AppTheme.colors.foregroundMuted,
                )
            }
        }
    }
}

@Composable
private fun GitPullRequestThreadSection(
    reference: String,
    mode: String,
    onReferenceChanged: (String) -> Unit,
    onModeChanged: (String) -> Unit,
    onCheckout: () -> Unit,
) {
    Column(Modifier.fillMaxWidth().padding(20.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text("Pull request thread", fontWeight = FontWeight.Bold)
        TextField(
            value = reference,
            onValueChange = onReferenceChanged,
            modifier = Modifier.fillMaxWidth(),
            label = { Text("PR number or URL") },
            singleLine = true,
        )
        Row(verticalAlignment = Alignment.CenterVertically) {
            RadioButton(selected = mode == "worktree", onClick = { onModeChanged("worktree") })
            Text("Worktree")
            RadioButton(selected = mode == "local", onClick = { onModeChanged("local") })
            Text("Current checkout")
        }
        Button(onClick = onCheckout, enabled = reference.trim().isNotEmpty()) {
            Icon(Icons.AutoMirrored.Outlined.AltRoute, null)
            Text("Open as thread", Modifier.padding(start = 8.dp))
        }
    }
}

@Composable
private fun GitProgress(progress: GitActionProgress) {
    Column(
        Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 8.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Text("Git action", fontWeight = FontWeight.Bold)
        Text(progress.label ?: progress.status.replace('_', ' ').replaceFirstChar { it.uppercase() })
        progress.output?.takeIf { it.isNotEmpty() }?.let { Text(it, style = AppTheme.caption) }
        progress.error?.let { Text(it, color = AppTheme.colors.dangerForeground) }
        if (progress.status == "started" || progress.status == "phase_started")
            CircularProgressIndicator(Modifier.padding(top = 4.dp).align(Alignment.Start), strokeWidth = 2.dp)
    }
}

@Composable
private fun GitConfirmationDialog(
    copy: DefaultBranchActionCopy?,
    onContinue: () -> Unit,
    onFeatureBranch: () -> Unit,
    onDismiss: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(copy?.title ?: "Run action on default ref?") },
        text = { Text(copy?.description ?: "Choose how to continue.") },
        confirmButton = { TextButton(onClick = onContinue) { Text(copy?.continueLabel ?: "Continue") } },
        dismissButton = { TextButton(onClick = onFeatureBranch) { Text("Feature branch") } },
    )
}

// Action execution passes core facts and its completion callback directly.
@Suppress("LongParameterList")
private fun runGitAction(
    model: AndroidAppModel,
    cwd: String,
    action: GitAction,
    commitMessage: String,
    filePaths: List<String>?,
    featureBranch: Boolean,
    onStarted: (String) -> Unit,
) {
    val id = "android-git-${System.currentTimeMillis()}"
    onStarted(id)
    model.perform(
        Intent.RunVcsAction(
            id,
            cwd,
            action.name.lowercase(),
            commitMessage.trim().ifEmpty { null },
            featureBranch,
            filePaths,
            model.snapshot.selectedThreadId(),
            model.snapshot.selectedProjectId(),
        )
    )
}

private fun GitAction.includesCommit(): Boolean =
    this == GitAction.COMMIT || this == GitAction.COMMIT_PUSH || this == GitAction.COMMIT_PUSH_PR

private fun GitAction.icon() =
    when (this) {
        GitAction.COMMIT -> Icons.Outlined.CheckCircle
        GitAction.PUSH -> Icons.Outlined.ArrowUpward
        GitAction.CREATE_PR,
        GitAction.COMMIT_PUSH_PR,
        GitAction.OPEN_PR -> Icons.Outlined.TextSnippet
        GitAction.COMMIT_PUSH -> Icons.Outlined.ArrowUpward
    }

private fun statusSummary(status: dev.remoteagent.core.GitStatus?): String =
    when {
        status == null -> "Git status is unavailable."
        !status.isRepo -> "This folder is not a Git repository."
        else ->
            buildList {
                    if (status.hasWorkingTreeChanges) add("${status.workingTree.size} changed")
                    if (status.aheadCount > 0uL) add("${status.aheadCount} ahead")
                    if (status.behindCount > 0uL) add("${status.behindCount} behind")
                    if (isEmpty()) add(if (status.hasUpstream) "Up to date" else "No upstream")
                }
                .joinToString(" · ")
    }

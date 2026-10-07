// Declarative native layout with the fixed mobile metrics; the conversation decisions are supplied by core.
@file:Suppress("TooManyFunctions", "LongMethod", "MagicNumber", "CyclomaticComplexMethod")

package dev.remoteagent.mobile

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.AltRoute
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material.icons.outlined.ChevronRight
import androidx.compose.material.icons.outlined.Computer
import androidx.compose.material.icons.outlined.Folder
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.BranchChoice
import dev.remoteagent.core.ComposerOptions
import dev.remoteagent.core.ComposerShortcuts
import dev.remoteagent.core.DraftHeroHeadlineKind
import dev.remoteagent.core.Intent
import dev.remoteagent.core.InteractionMode
import dev.remoteagent.core.NewThreadView
import dev.remoteagent.core.NewThreadWorkspaceView
import dev.remoteagent.core.ThreadWorkspaceMode

/** The new-thread draft: the hero headline with its project picker, workspace and branch, and the composer. */
@Composable
internal fun NewTaskScreen(model: AndroidAppModel) {
    val options = remember {
        ComposerOptions(compact = true, alternateModifier = false, shortcuts = ComposerShortcuts(null, null, null))
    }
    val view by rememberView(model.snapshot) { it.newThread(options) }
    var settings by remember { mutableStateOf(false) }
    var picking by remember { mutableStateOf(false) }
    val projectId = view?.projectId
    LaunchedEffect(projectId) { if (projectId != null) model.perform(Intent.SearchNewThreadBranches("")) }
    val workspace = view?.workspace
    BackHandler(picking) { picking = false }
    if (picking && workspace != null) {
        BranchPicker(model, workspace) { picking = false }
        return
    }
    ScreenScaffold("New thread", onBack = model::back) {
        val current = view ?: return@ScreenScaffold
        Column(Modifier.fillMaxSize().imePadding()) {
            Box(Modifier.weight(1f).fillMaxWidth(), contentAlignment = Alignment.Center) {
                if (current.showHero) Hero(model, current)
            }
            if (current.composer.controls.interactionToggle != null) {
                Row(
                    Modifier.padding(horizontal = 16.dp, vertical = 6.dp),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    ModeChip("Build", current.composer.controls.interactionMode == InteractionMode.DEFAULT) {
                        model.perform(Intent.SetInteractionMode(InteractionMode.DEFAULT))
                    }
                    ModeChip("Plan", current.composer.controls.interactionMode == InteractionMode.PLAN) {
                        model.perform(Intent.SetInteractionMode(InteractionMode.PLAN))
                    }
                }
            }
            workspace?.let { WorkspaceRow(model, it) { picking = true } }
            Composer(model, current.composer) { settings = true }
        }
        if (settings) ThreadSettingsSheet(model, current.composer) { settings = false }
    }
}

/** The workspace pill toggles the checkout and a new worktree; the branch pill opens the branch picker. */
@Composable
private fun WorkspaceRow(model: AndroidAppModel, workspace: NewThreadWorkspaceView, onPickBranch: () -> Unit) {
    val worktree = workspace.mode == ThreadWorkspaceMode.WORKTREE
    Row(
        Modifier.fillMaxWidth().padding(start = 20.dp, end = 20.dp, bottom = 4.dp),
        horizontalArrangement = Arrangement.spacedBy(4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        InlineControl(
            workspace.workspaceLabel,
            maxWidth = if (worktree) 148.dp else 220.dp,
            chevron = false,
            icon = { WorkspaceIcon(workspace.inWorktree || worktree) },
        ) {
            model.perform(
                Intent.SetNewThreadWorkspace(if (worktree) ThreadWorkspaceMode.LOCAL else ThreadWorkspaceMode.WORKTREE)
            )
        }
        InlineControl(
            workspace.branchLabel,
            maxWidth = 190.dp,
            chevron = true,
            icon = { ControlIcon(Icons.AutoMirrored.Outlined.AltRoute) },
            onClick = onPickBranch,
        )
    }
}

@Composable
private fun WorkspaceIcon(worktree: Boolean) {
    Box(Modifier.size(16.dp)) {
        ControlIcon(Icons.Outlined.Folder)
        if (worktree)
            Icon(
                Icons.AutoMirrored.Outlined.AltRoute,
                null,
                Modifier.size(9.dp).align(Alignment.BottomEnd).background(AppTheme.colors.composerPanel, CircleShape),
                tint = AppTheme.colors.iconMuted,
            )
    }
}

@Composable
private fun ControlIcon(icon: ImageVector) {
    Icon(icon, null, Modifier.size(16.dp), tint = AppTheme.colors.iconMuted)
}

/** A 44-tall inline pill over the composer: icon, label and an optional chevron. */
@Composable
private fun InlineControl(
    label: String,
    maxWidth: Dp,
    chevron: Boolean,
    icon: @Composable () -> Unit,
    onClick: (() -> Unit)?,
) {
    Row(
        Modifier.heightIn(min = 44.dp)
            .widthIn(max = maxWidth)
            .clip(RoundedCornerShape(12.dp))
            .then(if (onClick != null) Modifier.clickable(onClick = onClick) else Modifier)
            .padding(horizontal = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        icon()
        Text(
            label,
            Modifier.weight(1f, fill = false),
            style = AppTheme.footnote,
            fontWeight = FontWeight.Medium,
            color = AppTheme.colors.foregroundMuted,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
        if (chevron) Icon(Icons.Outlined.ChevronRight, null, Modifier.size(10.dp), tint = AppTheme.colors.iconMuted)
    }
}

private const val LOAD_MORE_SLACK = 3

/** "Branch" or "Base branch": search, start from origin for a worktree, and the branches. */
@Composable
private fun BranchPicker(model: AndroidAppModel, workspace: NewThreadWorkspaceView, onClose: () -> Unit) {
    val colors = AppTheme.colors
    var query by remember { mutableStateOf("") }
    var switching by remember { mutableStateOf(false) }
    var failure by remember { mutableStateOf<String?>(null) }
    val list = rememberLazyListState()
    val nearEnd by remember {
        derivedStateOf {
            val info = list.layoutInfo
            val last = info.visibleItemsInfo.lastOrNull()?.index ?: 0
            info.totalItemsCount > 0 && last >= info.totalItemsCount - LOAD_MORE_SLACK
        }
    }
    LaunchedEffect(query) { model.perform(Intent.SearchNewThreadBranches(query)) }
    LaunchedEffect(nearEnd, workspace.hasMoreBranches, workspace.branchesLoadingMore) {
        if (nearEnd && workspace.hasMoreBranches && !workspace.branchesLoadingMore)
            model.perform(Intent.LoadMoreNewThreadBranches)
    }
    failure?.let { message ->
        AlertDialog(
            onDismissRequest = { failure = null },
            containerColor = AppTheme.colors.cardAlt,
            shape = RoundedCornerShape(28.dp),
            title = { Text("Could not switch branch", style = AppTheme.title) },
            text = { Text(message, style = AppTheme.footnote, color = AppTheme.colors.foregroundSecondary) },
            confirmButton = {
                TextButton(onClick = { failure = null }) { Text("OK", color = AppTheme.colors.foreground) }
            },
        )
    }
    ScreenScaffold(workspace.branchRole, onBack = onClose) {
        Column(Modifier.fillMaxSize()) {
            SettingsField(
                query,
                { query = it },
                "Find a branch",
                Modifier.padding(start = 16.dp, end = 16.dp, top = 4.dp, bottom = 12.dp),
            )
            val branches = workspace.branches
            val status =
                when {
                    workspace.branchesLoading && branches.isEmpty() -> "Loading branches…"
                    workspace.branchError != null && branches.isEmpty() -> workspace.branchError
                    branches.isEmpty() && query.isNotEmpty() -> "No matching branches"
                    branches.isEmpty() -> "No branches available"
                    else -> null
                }
            LazyColumn(state = list, contentPadding = PaddingValues(16.dp)) {
                if (workspace.mode == ThreadWorkspaceMode.WORKTREE)
                    item(key = "origin") {
                        Row(
                            Modifier.fillMaxWidth()
                                .padding(bottom = 12.dp)
                                .background(colors.groupedCard, RoundedCornerShape(28.dp))
                                .heightIn(min = 56.dp)
                                .padding(horizontal = 16.dp, vertical = 12.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Text("Start from origin", Modifier.weight(1f), style = AppTheme.body)
                            AppSwitch(workspace.startFromOrigin) {
                                model.perform(Intent.SetNewThreadStartFromOrigin(it))
                            }
                        }
                    }
                status?.let { text ->
                    item(key = "status") {
                        Column(
                            Modifier.fillMaxWidth().padding(24.dp),
                            horizontalAlignment = Alignment.CenterHorizontally,
                            verticalArrangement = Arrangement.spacedBy(12.dp),
                        ) {
                            if (workspace.branchesLoading)
                                CircularProgressIndicator(
                                    Modifier.size(18.dp),
                                    color = colors.iconMuted,
                                    strokeWidth = 2.dp,
                                )
                            Text(
                                text,
                                style = AppTheme.footnote,
                                color = colors.foregroundMuted,
                                textAlign = TextAlign.Center,
                            )
                            if (workspace.branchError != null)
                                Text(
                                    "Try again",
                                    Modifier.clip(CircleShape)
                                        .background(colors.card)
                                        .clickable { model.perform(Intent.SearchNewThreadBranches(query)) }
                                        .padding(horizontal = 16.dp, vertical = 8.dp),
                                    style = AppTheme.footnote,
                                    fontWeight = FontWeight.Medium,
                                    color = colors.foreground,
                                )
                        }
                    }
                }
                itemsIndexed(branches, key = { _, branch -> branch.name }) { index, branch ->
                    val top = if (index == 0) 28.dp else 0.dp
                    val bottom = if (index == branches.lastIndex) 28.dp else 0.dp
                    BranchRow(branch, RoundedCornerShape(top, top, bottom, bottom), enabled = !switching) {
                        switching = true
                        model.perform(Intent.SelectNewThreadBranch(branch.name, branch.worktreePath)) { result ->
                            switching = false
                            result.exceptionOrNull()?.let { error ->
                                model.notice = null
                                failure = error.message ?: "The branch could not be checked out."
                            } ?: onClose()
                        }
                    }
                }
                if (workspace.branchesLoadingMore)
                    item(key = "more") {
                        Box(Modifier.fillMaxWidth().padding(16.dp), contentAlignment = Alignment.Center) {
                            CircularProgressIndicator(
                                Modifier.size(18.dp),
                                color = colors.iconMuted,
                                strokeWidth = 2.dp,
                            )
                        }
                    }
            }
        }
    }
}

@Composable
private fun BranchRow(branch: BranchChoice, shape: RoundedCornerShape, enabled: Boolean, onClick: () -> Unit) {
    val colors = AppTheme.colors
    val badge = branch.badge?.uppercase()
    Row(
        Modifier.fillMaxWidth()
            .clip(shape)
            .background(colors.groupedCard)
            .clickable(enabled = enabled, onClick = onClick)
            .heightIn(min = if (badge != null) 72.dp else 56.dp)
            .padding(horizontal = 16.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        Icon(Icons.AutoMirrored.Outlined.AltRoute, null, Modifier.size(24.dp), tint = colors.iconMuted)
        Column(Modifier.weight(1f)) {
            Text(
                branch.name,
                style = AppTheme.body,
                color = colors.foreground,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            badge?.let { Text(it, style = AppTheme.footnote, color = colors.foregroundMuted) }
        }
        if (branch.selected) Icon(Icons.Outlined.Check, null, Modifier.size(20.dp), tint = colors.primary)
    }
}

/**
 * The headline: "What should we work on?" with a "Choose a project" control for a draft without a project, else "What
 * should we build in <project>?" whose project opens "Choose project". The Host the task runs on follows.
 */
@Composable
private fun Hero(model: AndroidAppModel, view: NewThreadView) {
    val colors = AppTheme.colors
    val heading = AppTheme.largeTitle.copy(fontWeight = FontWeight.Medium, color = colors.foreground)
    val chooseProject = { model.navigate(Route.ChooseProject) }
    val environment =
        @Composable {
            model.snapshot.hostName()?.let { host ->
                InlineControl(
                    "on $host",
                    maxWidth = if (view.hero.kind == DraftHeroHeadlineKind.BUILD_IN) 260.dp else 170.dp,
                    chevron = false,
                    icon = { ControlIcon(Icons.Outlined.Computer) },
                    onClick = null,
                )
            }
        }
    Column(Modifier.padding(horizontal = 24.dp), horizontalAlignment = Alignment.CenterHorizontally) {
        if (view.hero.kind == DraftHeroHeadlineKind.BUILD_IN) {
            Text("What should we build", style = heading, textAlign = TextAlign.Center)
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("in ", style = heading)
                Text(
                    view.hero.projectLabel,
                    Modifier.widthIn(max = 250.dp)
                        .drawBehind {
                            val y = size.height - 1.dp.toPx()
                            drawLine(colors.foregroundMuted, Offset(0f, y), Offset(size.width, y), 1.dp.toPx())
                        }
                        .clickable(onClick = chooseProject),
                    style = heading,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                Text("?", style = heading)
            }
            Box(Modifier.padding(top = 24.dp)) { environment() }
        } else {
            Text(view.hero.headingLabel, style = heading, textAlign = TextAlign.Center)
            Row(Modifier.padding(top = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                InlineControl(
                    "Choose a project",
                    maxWidth = 220.dp,
                    chevron = true,
                    icon = { ControlIcon(Icons.Outlined.Folder) },
                    onClick = chooseProject,
                )
                environment()
            }
        }
    }
}

@Composable
private fun ModeChip(label: String, selected: Boolean, onClick: () -> Unit) {
    val colors = AppTheme.colors
    Text(
        label,
        Modifier.background(if (selected) colors.secondary else colors.screen, RoundedCornerShape(12.dp))
            .border(1.dp, colors.border, RoundedCornerShape(12.dp))
            .clickable(onClick = onClick)
            .padding(horizontal = 14.dp, vertical = 8.dp),
        style = AppTheme.footnote,
        fontWeight = FontWeight.Medium,
        color = if (selected) colors.foreground else colors.foregroundMuted,
    )
}

// Scheduled-task list and editor sections each own an independent UI responsibility.
@file:Suppress("TooManyFunctions")

package dev.remoteagent.mobile

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TextField
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.CatalogFilter
import dev.remoteagent.core.CatalogSheetItem
import dev.remoteagent.core.CatalogSheetOptions
import dev.remoteagent.core.CatalogSheetView
import dev.remoteagent.core.Intent
import dev.remoteagent.core.ScheduledTaskDraft
import dev.remoteagent.core.ScheduledTaskScheduleDraft
import dev.remoteagent.core.ScheduledTaskWorkspaceDraft
import dev.remoteagent.core.Snapshot
import dev.remoteagent.core.TraitControl

private val CATALOG_OPTIONS = CatalogSheetOptions(CatalogFilter.All, false, "", emptyList(), null)

@Suppress("LongMethod") // The screen owns selection, draft, save-error, and owner callback wiring.
@Composable
internal fun ScheduledTasksScreen(model: AndroidAppModel) {
    var selected by remember { mutableStateOf<String?>(null) }
    var draft by remember { mutableStateOf<ScheduledTaskDraft?>(null) }
    var saveError by remember { mutableStateOf<String?>(null) }
    val snapshot = model.snapshot
    val catalog by rememberView(snapshot, CATALOG_OPTIONS) { it.catalogSheet(CATALOG_OPTIONS) }
    val clearEditor = {
        selected = null
        draft = null
        saveError = null
    }
    LaunchedEffect(snapshot.revision()) {
        if (draft == null && selected != null) draft = snapshot.scheduledTaskDraft(selected)
    }
    ScreenScaffold("Scheduled tasks", onBack = model::back) {
        LazyColumn(contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(16.dp)) {
            item {
                ScheduledTaskListSection(
                    snapshot = snapshot,
                    onAction = { model.perform(it) },
                    onEdit = { taskId ->
                        selected = taskId
                        draft = snapshot.scheduledTaskDraft(taskId)
                        saveError = null
                    },
                    onDelete = {
                        model.perform(Intent.DeleteScheduledTask(it))
                        if (selected == it) clearEditor()
                    },
                    onCreate = {
                        selected = null
                        draft = snapshot.scheduledTaskDraft(null)
                        saveError = null
                    },
                )
            }
            draft?.let { current ->
                item {
                    val tasks = snapshot.scheduledTasks().tasks
                    val currentIsSelected = current.id != null && current.id == selected
                    ScheduledTaskEditor(
                        snapshot = snapshot,
                        current = current,
                        taskMissing = currentIsSelected && tasks.none { it.id == current.id },
                        saveError = saveError,
                        projects = snapshot.projects().map { it.id to it.name },
                        catalog = catalog,
                        onChange = { draft = it },
                        onAction = { model.perform(it) },
                        onSave = {
                            draft?.let { latest ->
                                model.perform(Intent.SaveScheduledTask(latest)) { result ->
                                    result.fold(
                                        onSuccess = { clearEditor() },
                                        onFailure = { error ->
                                            saveError = error.message ?: "Could not save scheduled task."
                                        },
                                    )
                                }
                            }
                        },
                    )
                }
            }
        }
    }
}

@Composable
private fun ScheduledTaskListSection(
    snapshot: Snapshot,
    onAction: (Intent) -> Unit,
    onEdit: (String) -> Unit,
    onDelete: (String) -> Unit,
    onCreate: () -> Unit,
) {
    val tasks = snapshot.scheduledTasks().tasks
    SectionCard("Automations") {
        tasks.forEachIndexed { index, task ->
            if (index > 0) HorizontalDivider(color = AppTheme.colors.border)
            Row(Modifier.fillMaxWidth().padding(16.dp)) {
                Column(Modifier.weight(1f)) {
                    Text(task.title, style = AppTheme.body, color = AppTheme.colors.foreground)
                    Text(
                        "${task.scheduleLabel} · ${task.lastRunStatus}",
                        style = AppTheme.caption,
                        color = AppTheme.colors.foregroundMuted,
                    )
                }
                TextButton(onClick = { onAction(Intent.SetScheduledTaskEnabled(task.id, !task.enabled)) }) {
                    Text(if (task.enabled) "Pause" else "Enable")
                }
                TextButton(onClick = { onAction(Intent.RunScheduledTaskNow(task.id)) }) { Text("Run now") }
                TextButton(onClick = { onDelete(task.id) }) { Text("Delete", color = AppTheme.colors.dangerForeground) }
            }
            TextButton(onClick = { onEdit(task.id) }, modifier = Modifier.padding(horizontal = 12.dp)) { Text("Edit") }
        }
        TextButton(onClick = onCreate) { Text("New scheduled task") }
    }
}

@Suppress("LongParameterList") // Keep immutable editor facts and owner callbacks explicit.
@Composable
private fun ScheduledTaskEditor(
    snapshot: Snapshot,
    current: ScheduledTaskDraft,
    taskMissing: Boolean,
    saveError: String?,
    projects: List<Pair<String, String>>,
    catalog: CatalogSheetView?,
    onChange: (ScheduledTaskDraft) -> Unit,
    onAction: (Intent) -> Unit,
    onSave: () -> Unit,
) {
    LaunchedEffect(current.projectId) { onAction(Intent.SearchScheduledTaskBranches(current.projectId, "")) }
    SectionCard(if (current.id == null) "New scheduled task" else "Edit scheduled task") {
        ScheduledTaskBasicsSection(current, projects, onChange)
        ScheduledTaskModelSection(snapshot, catalog, current, onChange)
        val runtimeChoices = snapshot.scheduledTaskRuntimeModes(current)
        Text("Runtime", modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp))
        Row(Modifier.padding(horizontal = 16.dp), horizontalArrangement = Arrangement.spacedBy(4.dp)) {
            runtimeChoices.forEach { choice ->
                FilterChip(
                    selected = current.runtimeMode == choice.mode,
                    onClick = { onChange(current.copy(runtimeMode = choice.mode)) },
                    label = { Text(choice.label) },
                )
            }
        }
        if (taskMissing) {
            Text(
                "This task was deleted elsewhere. Close this editor and start again.",
                color = AppTheme.colors.dangerForeground,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
            )
        }
        saveError?.let {
            Text(
                it,
                color = AppTheme.colors.dangerForeground,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
            )
        }
        ScheduledTaskScheduleFields(current, onChange)
        ScheduledTaskWorkspaceSection(snapshot, current, onChange, onAction)
        TextButton(onClick = onSave, enabled = !taskMissing, modifier = Modifier.padding(16.dp)) {
            Text("Save scheduled task")
        }
    }
}

@Composable
private fun ScheduledTaskBasicsSection(
    current: ScheduledTaskDraft,
    projects: List<Pair<String, String>>,
    onChange: (ScheduledTaskDraft) -> Unit,
) {
    TextField(
        current.title,
        { onChange(current.copy(title = it)) },
        label = { Text("Title") },
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 6.dp),
    )
    TextField(
        current.prompt,
        { onChange(current.copy(prompt = it)) },
        label = { Text("Prompt") },
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 6.dp),
    )
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 6.dp),
        verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
    ) {
        Text("Enabled", modifier = Modifier.weight(1f))
        Switch(checked = current.enabled, onCheckedChange = { onChange(current.copy(enabled = it)) })
    }
    var projectOpen by remember { mutableStateOf(false) }
    TextButton(onClick = { projectOpen = !projectOpen }) {
        Text("Project: " + (projects.firstOrNull { it.first == current.projectId }?.second ?: current.projectId))
    }
    if (projectOpen) {
        projects.forEach { (id, name) ->
            FilterChip(
                selected = id == current.projectId,
                onClick = {
                    projectOpen = false
                    onChange(current.copy(projectId = id))
                },
                label = { Text(name) },
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 2.dp),
            )
        }
    }
}

@Composable
private fun ScheduledTaskModelSection(
    snapshot: Snapshot,
    catalog: CatalogSheetView?,
    current: ScheduledTaskDraft,
    onChange: (ScheduledTaskDraft) -> Unit,
) {
    var modelOpen by remember { mutableStateOf(false) }
    TextButton(onClick = { modelOpen = !modelOpen }) {
        Text(
            "Model: ${current.instanceId.ifBlank { "Choose a provider" }} / " +
                current.model.ifBlank { "Choose a model" }
        )
    }
    if (modelOpen) {
        catalog?.items.orEmpty().forEach { item ->
            if (item is CatalogSheetItem.Model) {
                FilterChip(
                    selected = item.instanceId == current.instanceId && item.slug == current.model,
                    onClick = {
                        modelOpen = false
                        val sameModel = item.instanceId == current.instanceId && item.slug == current.model
                        onChange(
                            current.copy(
                                instanceId = item.instanceId,
                                driver = item.driver,
                                model = item.slug,
                                options = if (sameModel) current.options else emptyList(),
                            )
                        )
                    },
                    label = { Text(item.label) },
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 2.dp),
                )
            }
        }
    }
    ScheduledTaskTraitControls(snapshot, current, onChange)
}

@Composable
private fun ScheduledTaskTraitControls(
    snapshot: Snapshot,
    current: ScheduledTaskDraft,
    onChange: (ScheduledTaskDraft) -> Unit,
) {
    val traits by
        rememberView(snapshot, current.instanceId, current.model, current.options) { it.scheduledTaskTraits(current) }
    traits?.controls.orEmpty().forEach { control ->
        when (control) {
            is TraitControl.Select -> {
                Text(control.label, modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp))
                Row(Modifier.padding(horizontal = 16.dp), horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                    control.choices.forEach { choice ->
                        FilterChip(
                            selected = choice.id == control.selected,
                            onClick = { onChange(snapshot.selectScheduledTaskTrait(current, control.id, choice.id)) },
                            label = { Text(choice.label) },
                        )
                    }
                }
            }
            is TraitControl.Toggle -> {
                Row(
                    Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 6.dp),
                    verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
                ) {
                    Text(control.label, modifier = Modifier.weight(1f))
                    Switch(
                        checked = control.on,
                        onCheckedChange = { onChange(snapshot.toggleScheduledTaskTrait(current, control.id, it)) },
                    )
                }
            }
        }
    }
}

@Composable
private fun ScheduledTaskScheduleFields(current: ScheduledTaskDraft, onChange: (ScheduledTaskDraft) -> Unit) {
    val interval = current.schedule is ScheduledTaskScheduleDraft.Interval
    Row(Modifier.padding(horizontal = 16.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        FilterChip(
            selected = !interval,
            // Weekday IDs are fixed protocol values, so keep this suppression at the
            // expression that declares the Monday-through-Friday schedule.
            onClick =
                @Suppress("MagicNumber") {
                    onChange(
                        current.copy(
                            schedule = ScheduledTaskScheduleDraft.FixedTime("09:00", byteArrayOf(1, 2, 3, 4, 5))
                        )
                    )
                },
            label = { Text("Fixed time") },
        )
        FilterChip(
            selected = interval,
            onClick = { onChange(current.copy(schedule = ScheduledTaskScheduleDraft.Interval(900_000UL))) },
            label = { Text("Interval") },
        )
    }
    when (val schedule = current.schedule) {
        is ScheduledTaskScheduleDraft.Interval -> {
            TextField(
                (schedule.everyMs / 60_000UL).toString(),
                { value ->
                    value.toULongOrNull()?.let {
                        onChange(current.copy(schedule = ScheduledTaskScheduleDraft.Interval(it * 60_000UL)))
                    }
                },
                label = { Text("Interval in minutes") },
                modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 6.dp),
            )
        }
        is ScheduledTaskScheduleDraft.FixedTime -> {
            TextField(
                schedule.timeOfDay,
                { onChange(current.copy(schedule = ScheduledTaskScheduleDraft.FixedTime(it, schedule.weekdays))) },
                label = { Text("Local time (HH:MM)") },
                modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 6.dp),
            )
            ScheduledTaskWeekdayFields(current, schedule, onChange)
        }
    }
}

@Composable
private fun ScheduledTaskWeekdayFields(
    current: ScheduledTaskDraft,
    schedule: ScheduledTaskScheduleDraft.FixedTime,
    onChange: (ScheduledTaskDraft) -> Unit,
) {
    Row(Modifier.padding(horizontal = 16.dp), horizontalArrangement = Arrangement.spacedBy(4.dp)) {
        listOf("S", "M", "T", "W", "T", "F", "S").forEachIndexed { index, label ->
            FilterChip(
                selected = index.toByte() in schedule.weekdays,
                onClick = {
                    val days = schedule.weekdays.toMutableList()
                    val day = index.toByte()
                    if (!days.remove(day)) days.add(day)
                    onChange(
                        current.copy(
                            schedule =
                                ScheduledTaskScheduleDraft.FixedTime(schedule.timeOfDay, days.sorted().toByteArray())
                        )
                    )
                },
                label = { Text(label) },
            )
        }
    }
}

@Composable
private fun ScheduledTaskWorkspaceSection(
    snapshot: Snapshot,
    current: ScheduledTaskDraft,
    onChange: (ScheduledTaskDraft) -> Unit,
    onAction: (Intent) -> Unit,
) {
    val workspace = current.workspace
    Text("Workspace", modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp))
    Row(Modifier.padding(horizontal = 16.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        listOf("root" to "Project checkout", "worktree" to "New worktree", "existing" to "Existing worktree").forEach {
            (kind, label) ->
            FilterChip(
                selected =
                    when (kind) {
                        "worktree" -> workspace is ScheduledTaskWorkspaceDraft.Worktree
                        "existing" -> workspace is ScheduledTaskWorkspaceDraft.ExistingWorktree
                        else -> workspace is ScheduledTaskWorkspaceDraft.Root
                    },
                onClick = {
                    onChange(
                        current.copy(
                            workspace =
                                when (kind) {
                                    "worktree" -> ScheduledTaskWorkspaceDraft.Worktree("main", null, true)
                                    "existing" -> ScheduledTaskWorkspaceDraft.ExistingWorktree("", null)
                                    else -> ScheduledTaskWorkspaceDraft.Root(null)
                                }
                        )
                    )
                },
                label = { Text(label) },
            )
        }
    }
    ScheduledTaskWorkspaceFields(snapshot, current, onChange, onAction)
}

@Composable
private fun ScheduledTaskWorkspaceFields(
    snapshot: Snapshot,
    current: ScheduledTaskDraft,
    onChange: (ScheduledTaskDraft) -> Unit,
    onAction: (Intent) -> Unit,
) {
    when (val workspace = current.workspace) {
        is ScheduledTaskWorkspaceDraft.Worktree ->
            ScheduledTaskWorktreeFields(snapshot, current, workspace, onChange, onAction)
        is ScheduledTaskWorkspaceDraft.ExistingWorktree -> {
            TextField(
                workspace.worktreePath,
                { onChange(current.copy(workspace = workspace.copy(worktreePath = it))) },
                label = { Text("Worktree path") },
                modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 6.dp),
            )
        }
        is ScheduledTaskWorkspaceDraft.Root -> Unit
    }
}

@Composable
private fun ScheduledTaskWorktreeFields(
    snapshot: Snapshot,
    current: ScheduledTaskDraft,
    workspace: ScheduledTaskWorkspaceDraft.Worktree,
    onChange: (ScheduledTaskDraft) -> Unit,
    onAction: (Intent) -> Unit,
) {
    var branchOpen by remember { mutableStateOf(false) }
    var branchQuery by remember { mutableStateOf("") }
    val branches = snapshot.scheduledTaskBranches(current.projectId, workspace.baseRef)
    TextButton(onClick = { branchOpen = !branchOpen }) {
        Text("Base branch: ${workspace.baseRef.ifBlank { "Choose a branch" }}")
    }
    if (branchOpen) {
        TextField(
            branchQuery,
            {
                branchQuery = it
                onAction(Intent.SearchScheduledTaskBranches(current.projectId, it))
            },
            label = { Text("Find a branch") },
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 6.dp),
        )
        if (branches.loading) {
            Text("Loading branches…", modifier = Modifier.padding(horizontal = 16.dp))
        }
        branches.branches.forEach { branch ->
            FilterChip(
                selected = branch.selected,
                onClick = {
                    branchOpen = false
                    onChange(current.copy(workspace = workspace.copy(baseRef = branch.name)))
                },
                label = { Text(branch.name) },
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 2.dp),
            )
        }
        if (branches.hasMore) {
            TextButton(onClick = { onAction(Intent.LoadMoreScheduledTaskBranches(current.projectId)) }) {
                Text("Load more branches")
            }
        }
        if (branches.branches.isEmpty() && !branches.loading) {
            Text("No local branches available", modifier = Modifier.padding(horizontal = 16.dp))
        }
    }
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
        verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
    ) {
        Text("Start from origin", modifier = Modifier.weight(1f))
        Switch(
            checked = workspace.startFromOrigin,
            onCheckedChange = { onChange(current.copy(workspace = workspace.copy(startFromOrigin = it))) },
        )
    }
}

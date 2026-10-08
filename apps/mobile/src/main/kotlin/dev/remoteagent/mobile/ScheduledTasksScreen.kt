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
import dev.remoteagent.core.Intent
import dev.remoteagent.core.CatalogFilter
import dev.remoteagent.core.CatalogSheetItem
import dev.remoteagent.core.CatalogSheetOptions
import dev.remoteagent.core.ScheduledTaskDraft
import dev.remoteagent.core.ScheduledTaskScheduleDraft
import dev.remoteagent.core.ScheduledTaskWorkspaceDraft
import dev.remoteagent.core.Snapshot

@Composable
internal fun ScheduledTasksScreen(model: AndroidAppModel) {
    var selected by remember { mutableStateOf<String?>(null) }
    var draft by remember { mutableStateOf<ScheduledTaskDraft?>(null) }
    LaunchedEffect(model.snapshot.revision) {
        if (draft == null && selected != null) {
            draft = model.snapshot.scheduledTaskDraft(selected)
        }
    }
    ScreenScaffold("Scheduled tasks", onBack = model::back) {
        LazyColumn(
            contentPadding = PaddingValues(16.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            item {
                SectionCard("Automations") {
                    model.snapshot.scheduledTasks().tasks.forEachIndexed { index, task ->
                        if (index > 0) HorizontalDivider(color = AppTheme.colors.border)
                        Row(Modifier.fillMaxWidth().padding(16.dp)) {
                            Column(Modifier.weight(1f)) {
                                Text(task.title, style = AppTheme.body, color = AppTheme.colors.foreground)
                                Text("${task.scheduleLabel} · ${task.lastRunStatus}", style = AppTheme.caption, color = AppTheme.colors.foregroundMuted)
                            }
                            TextButton(onClick = {
                                model.perform(Intent.SetScheduledTaskEnabled(task.id, !task.enabled))
                            }) {
                                Text(if (task.enabled) "Pause" else "Enable")
                            }
                            TextButton(onClick = { model.perform(Intent.RunScheduledTaskNow(task.id)) }) {
                                Text("Run now")
                            }
                            TextButton(onClick = {
                                model.perform(Intent.DeleteScheduledTask(task.id))
                                if (selected == task.id) {
                                    selected = null
                                    draft = null
                                }
                            }) {
                                Text("Delete", color = AppTheme.colors.dangerForeground)
                            }
                        }
                        TextButton(onClick = {
                            selected = task.id
                            draft = model.snapshot.scheduledTaskDraft(task.id)
                        }, modifier = Modifier.padding(horizontal = 12.dp)) {
                            Text("Edit")
                        }
                    }
                    TextButton(onClick = {
                        selected = null
                        draft = model.snapshot.scheduledTaskDraft(null)
                    }) {
                        Text("New scheduled task")
                    }
                }
            }
            draft?.let { current ->
                item {
                    ScheduledTaskEditor(
                        model = model,
                        current = current,
                        snapshot = model.snapshot,
                        projects = model.snapshot.projects().map { it.id to it.name },
                        onChange = { draft = it },
                        onSave = {
                            draft?.let { latest ->
                                model.perform(Intent.SaveScheduledTask(latest)) {
                                    if (it.isSuccess) {
                                        selected = null
                                        draft = null
                                    }
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
private fun ScheduledTaskEditor(
    model: AndroidAppModel,
    current: ScheduledTaskDraft,
    snapshot: Snapshot,
    projects: List<Pair<String, String>>,
    onChange: (ScheduledTaskDraft) -> Unit,
    onSave: () -> Unit,
) {
    var branchOpen by remember(current.projectId) { mutableStateOf(false) }
    var branchQuery by remember(current.projectId) { mutableStateOf("") }
    LaunchedEffect(current.projectId) {
        model.perform(Intent.SearchScheduledTaskBranches(current.projectId, ""))
    }
    SectionCard(if (current.id == null) "New scheduled task" else "Edit scheduled task") {
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
            Switch(
                checked = current.enabled,
                onCheckedChange = { onChange(current.copy(enabled = it)) },
            )
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
        var modelOpen by remember { mutableStateOf(false) }
        val catalogOptions = CatalogSheetOptions(CatalogFilter.All, false, "", emptyList(), null)
        val catalog by rememberView(snapshot, catalogOptions) { it.catalogSheet(catalogOptions) }
        TextButton(onClick = { modelOpen = !modelOpen }) {
            Text("Model: ${current.instanceId.ifBlank { "Choose a provider" }} / ${current.model.ifBlank { "Choose a model" }}")
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
                                ),
                            )
                        },
                        label = { Text(item.label) },
                        modifier = Modifier.padding(horizontal = 16.dp, vertical = 2.dp),
                    )
                }
            }
        }
        val interval = current.schedule is ScheduledTaskScheduleDraft.Interval
        Row(Modifier.padding(horizontal = 16.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            FilterChip(
                selected = !interval,
                onClick = {
                    onChange(
                        current.copy(
                            schedule = ScheduledTaskScheduleDraft.FixedTime(
                                "09:00",
                                listOf(1u, 2u, 3u, 4u, 5u).map { it.toUByte() },
                            ),
                        ),
                    )
                },
                label = { Text("Fixed time") },
            )
            FilterChip(
                selected = interval,
                onClick = {
                    onChange(current.copy(schedule = ScheduledTaskScheduleDraft.Interval(900_000UL)))
                },
                label = { Text("Interval") },
            )
        }
        when (val schedule = current.schedule) {
            is ScheduledTaskScheduleDraft.Interval -> {
                val minutes = (schedule.everyMs / 60_000UL).toString()
                TextField(
                    minutes,
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
                Row(Modifier.padding(horizontal = 16.dp), horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                    listOf("S", "M", "T", "W", "T", "F", "S").forEachIndexed { index, label ->
                        FilterChip(
                            selected = index.toUByte() in schedule.weekdays.map { it.toUByte() },
                            onClick = {
                                val days = schedule.weekdays.toMutableList()
                                val day = index.toUByte()
                                if (!days.remove(day)) days.add(day)
                                onChange(current.copy(schedule = ScheduledTaskScheduleDraft.FixedTime(schedule.timeOfDay, days.sorted())))
                            },
                            label = { Text(label) },
                        )
                    }
                }
            }
        }
        Text("Workspace", modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp))
        val workspace = current.workspace
        Row(Modifier.padding(horizontal = 16.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            listOf(
                "root" to "Project checkout",
                "worktree" to "New worktree",
                "existing" to "Existing worktree",
            ).forEach { (kind, label) ->
                FilterChip(
                    selected = when (kind) {
                        "worktree" -> workspace is ScheduledTaskWorkspaceDraft.Worktree
                        "existing" -> workspace is ScheduledTaskWorkspaceDraft.ExistingWorktree
                        else -> workspace is ScheduledTaskWorkspaceDraft.Root
                    },
                    onClick = {
                        onChange(
                            current.copy(
                                workspace = when (kind) {
                                    "worktree" -> ScheduledTaskWorkspaceDraft.Worktree("main", null, true)
                                    "existing" -> ScheduledTaskWorkspaceDraft.ExistingWorktree("", null)
                                    else -> ScheduledTaskWorkspaceDraft.Root(null)
                                },
                            ),
                        )
                    },
                    label = { Text(label) },
                )
            }
        }
        when (workspace) {
            is ScheduledTaskWorkspaceDraft.Worktree -> {
                val branches = snapshot.scheduledTaskBranches(current.projectId, workspace.baseRef)
                TextButton(onClick = { branchOpen = !branchOpen }) {
                    Text("Base branch: ${workspace.baseRef.ifBlank { "Choose a branch" }}")
                }
                if (branchOpen) {
                    TextField(
                        branchQuery,
                        {
                            branchQuery = it
                            model.perform(Intent.SearchScheduledTaskBranches(current.projectId, it))
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
                        TextButton(onClick = {
                            model.perform(Intent.LoadMoreScheduledTaskBranches(current.projectId))
                        }) {
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
        TextButton(onClick = onSave, modifier = Modifier.padding(16.dp)) { Text("Save scheduled task") }
    }
}

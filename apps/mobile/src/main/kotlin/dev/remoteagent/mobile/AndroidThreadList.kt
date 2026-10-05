package dev.remoteagent.mobile

import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.Intent
import dev.remoteagent.core.ShelfKind
import dev.remoteagent.core.ThreadAction
import dev.remoteagent.core.ThreadRow
import java.time.Instant
import kotlinx.coroutines.delay

private const val LIST_CLOCK_INTERVAL_MILLIS = 1000L
private const val SNOOZE_HOUR_SECONDS = 3600L

@Composable
// Declarative native layout; the conversation decisions are supplied by core.
@Suppress("LongMethod", "CyclomaticComplexMethod")
internal fun ThreadListScreen(model: AndroidAppModel, modifier: Modifier = Modifier) {
    var search by remember(model.profileId) { mutableStateOf(model.snapshot.searchQuery()) }
    var collapsed by remember { mutableStateOf(emptySet<ShelfKind>()) }
    var settledLimit by remember { mutableStateOf(10u) }
    var now by remember { mutableStateOf(System.currentTimeMillis()) }
    var projectMenu by remember { mutableStateOf(false) }
    var archive by remember { mutableStateOf(false) }
    var settings by remember { mutableStateOf(false) }
    var addProject by remember { mutableStateOf(false) }
    var path by remember { mutableStateOf("") }
    LaunchedEffect(Unit) {
        while (true) {
            delay(LIST_CLOCK_INTERVAL_MILLIS)
            now = System.currentTimeMillis()
        }
    }
    val shelves = remember(model.snapshot, now, settledLimit) { model.snapshot.shelves(now, settledLimit) }
    Column(modifier.fillMaxSize()) {
        Row(Modifier.fillMaxWidth().padding(horizontal = 20.dp), horizontalArrangement = Arrangement.spacedBy(4.dp)) {
            OutlinedTextField(
                search,
                {
                    search = it
                    model.perform(Intent.Search(it))
                },
                Modifier.weight(1f),
                placeholder = { Text("Search") },
                singleLine = true,
            )
            Box {
                TextButton(onClick = { projectMenu = true }) { Text("▾") }
                DropdownMenu(projectMenu, { projectMenu = false }) {
                    DropdownMenuItem(
                        text = { Text("All projects") },
                        onClick = {
                            projectMenu = false
                            model.perform(Intent.FilterProject(null))
                        },
                    )
                    model.snapshot.projects().forEach { project ->
                        DropdownMenuItem(
                            text = { Text(project.name) },
                            onClick = {
                                projectMenu = false
                                model.perform(Intent.FilterProject(project.id))
                            },
                        )
                    }
                }
            }
            TextButton(onClick = { addProject = true }) { Text("+") }
            TextButton(onClick = { model.newThread() }) { Text("New") }
        }
        model.snapshot.selectedProjectId()?.let { id ->
            Row {
                Text("Project: " + (model.snapshot.projects().firstOrNull { it.id == id }?.name ?: "Chats"))
                TextButton(onClick = { model.perform(Intent.FilterProject(null)) }) { Text("Clear") }
            }
        }
        LazyColumn(
            Modifier.weight(1f),
            contentPadding = PaddingValues(horizontal = 20.dp, vertical = 12.dp),
            verticalArrangement = Arrangement.spacedBy(1.dp),
        ) {
            shelves.forEach { shelf ->
                item(key = "shelf:${shelf.kind}") {
                    TextButton(
                        onClick = {
                            collapsed = if (shelf.kind in collapsed) collapsed - shelf.kind else collapsed + shelf.kind
                        }
                    ) {
                        Text(
                            "${if (shelf.kind in collapsed) "›" else "⌄"} ${shelf.title}  ${shelf.total}",
                            style = MaterialTheme.typography.labelLarge,
                        )
                    }
                }
                if (shelf.kind !in collapsed) {
                    items(shelf.rows, key = { it.id }) { row -> ThreadCard(model, row, row.settled) }
                    if (shelf.hasMore) item { TextButton(onClick = { settledLimit += 25u }) { Text("Load 25 more") } }
                }
            }
            item { TextButton(onClick = { archive = !archive }) { Text("${if (archive) "⌄" else "›"} Archived") } }
            if (archive)
                items(model.snapshot.archivedThreads(now), key = { it.id }) { ThreadCard(model, it, it.settled) }
            item { TextButton(onClick = { settings = true }) { Text("Settings") } }
        }
    }
    if (settings) SettingsDialog(model) { settings = false }
    if (addProject)
        AlertDialog(
            onDismissRequest = { addProject = false },
            title = { Text("Add project") },
            text = { OutlinedTextField(path, { path = it }, label = { Text("Absolute path on Host") }) },
            confirmButton = {
                TextButton(
                    onClick = {
                        model.perform(Intent.RegisterProject(path))
                        path = ""
                        addProject = false
                    }
                ) {
                    Text("Add")
                }
            },
            dismissButton = { TextButton(onClick = { addProject = false }) { Text("Cancel") } },
        )
}

@Composable
// Declarative native layout; the conversation decisions are supplied by core.
@Suppress("LongMethod", "CyclomaticComplexMethod")
private fun ThreadCard(model: AndroidAppModel, row: ThreadRow, settled: Boolean) {
    var menu by remember(row.id) { mutableStateOf(false) }
    Box {
        Surface(
            Modifier.fillMaxWidth()
                .combinedClickable(onClick = { model.openThread(row.id) }, onLongClick = { menu = true }),
            color = T3.color(if (row.selected) "mobileSelected" else "surface"),
            shape = androidx.compose.foundation.shape.RoundedCornerShape(10.dp),
        ) {
            Row(
                Modifier.padding(horizontal = 12.dp, vertical = if (row.slim) 8.dp else 12.dp)
                    .heightIn(min = if (row.slim) 20.dp else 58.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                ProviderIcon(row.providerKind, Modifier.size(16.dp))
                Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    Text(
                        row.title,
                        style = MaterialTheme.typography.bodyMedium,
                        fontWeight = if (row.unread) FontWeight.Bold else FontWeight.Medium,
                        maxLines = 1,
                    )
                    if (!row.slim) {
                        Text(
                            row.preview,
                            style = MaterialTheme.typography.bodySmall,
                            color = T3.color("textMuted"),
                            maxLines = 1,
                        )
                        Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                            Text(row.status, style = MaterialTheme.typography.labelSmall, color = T3.status(row.tone))
                            row.durationMs?.let {
                                Text(
                                    "${it / 1000u}s",
                                    style = MaterialTheme.typography.labelSmall,
                                    color = T3.color("textMuted"),
                                )
                            }
                            row.wakeLabel?.let {
                                Text(it, style = MaterialTheme.typography.labelSmall, color = T3.color("textMuted"))
                            }
                            Spacer(Modifier.weight(1f))
                            Text(
                                model.snapshot.projects().firstOrNull { it.id == row.projectId }?.name ?: "",
                                style = MaterialTheme.typography.labelSmall,
                                color = T3.color("textMuted"),
                            )
                        }
                        row.branch?.let {
                            Text(
                                it,
                                fontFamily = FontFamily.Monospace,
                                style = MaterialTheme.typography.labelSmall,
                                color = T3.color("textMuted"),
                                maxLines = 1,
                            )
                        }
                    }
                }
                if (row.unread) Text("•", color = T3.color("accent"))
            }
        }
        DropdownMenu(menu, { menu = false }) {
            ThreadActionItems(model, row.id, row.pinned, row.archived, row.settled, row.snoozed) { menu = false }
        }
    }
}

@Composable
@Suppress("LongParameterList") // Pass the explicit core capabilities needed by this menu.
internal fun ThreadActionItems(
    model: AndroidAppModel,
    id: String,
    pinned: Boolean,
    archived: Boolean,
    settled: Boolean,
    snoozed: Boolean = false,
    close: () -> Unit,
) {
    fun action(value: ThreadAction) {
        close()
        if (value == ThreadAction.Delete) model.deleteThreadId = id else model.perform(Intent.Thread(id, value))
    }
    DropdownMenuItem(
        text = { Text(if (pinned) "Unpin" else "Pin") },
        onClick = { action(if (pinned) ThreadAction.Unpin else ThreadAction.Pin) },
    )
    if (pinned) {
        DropdownMenuItem(
            text = { Text("Move up") },
            onClick = {
                close()
                model.perform(Intent.MovePinned(id, true))
            },
        )
        DropdownMenuItem(
            text = { Text("Move down") },
            onClick = {
                close()
                model.perform(Intent.MovePinned(id, false))
            },
        )
    }
    DropdownMenuItem(
        text = { Text(if (settled) "Un-settle" else "Settle") },
        onClick = { action(if (settled) ThreadAction.Unsettle else ThreadAction.Settle) },
    )
    if (snoozed) DropdownMenuItem(text = { Text("Unsnooze") }, onClick = { action(ThreadAction.Unsnooze) })
    DropdownMenuItem(
        text = { Text("Snooze 1 hour") },
        onClick = { action(ThreadAction.Snooze(Instant.now().plusSeconds(SNOOZE_HOUR_SECONDS).toString())) },
    )
    DropdownMenuItem(text = { Text("Mark unread") }, onClick = { action(ThreadAction.MarkUnread) })
    DropdownMenuItem(
        text = { Text(if (archived) "Unarchive" else "Archive") },
        onClick = { action(if (archived) ThreadAction.Unarchive else ThreadAction.Archive) },
    )
    DropdownMenuItem(
        text = { Text("Delete", color = T3.color("errorForeground")) },
        onClick = { action(ThreadAction.Delete) },
    )
}

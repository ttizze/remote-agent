package dev.remoteagent.mobile

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
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
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Model
import dev.remoteagent.core.QueueAction
import dev.remoteagent.core.RowKind
import dev.remoteagent.core.SendBehavior
import dev.remoteagent.core.ThreadAction
import dev.remoteagent.core.TimelineRow
import dev.remoteagent.core.runtimeModeChoices
import kotlinx.coroutines.flow.collect

private const val AUTO_FOLLOW_DISTANCE_PX = 80

@OptIn(ExperimentalMaterial3Api::class)
@Composable
// Declarative native layout; the conversation decisions are supplied by core.
@Suppress("LongMethod", "CyclomaticComplexMethod")
internal fun ThreadDetailScreen(model: AndroidAppModel, modifier: Modifier = Modifier) {
    val conversation = model.conversation
    val list = rememberLazyListState()
    var queue by remember { mutableStateOf(false) }
    var agents by remember(conversation.threadId) { mutableStateOf(false) }
    var actions by remember { mutableStateOf(false) }
    var rename by remember { mutableStateOf(false) }
    var title by remember { mutableStateOf("") }
    var tools by remember { mutableStateOf<String?>(null) }
    var initialized by remember(conversation.threadId) { mutableStateOf(false) }
    var following by remember(conversation.threadId) { mutableStateOf(true) }
    var programmaticScroll by remember { mutableStateOf(false) }
    LaunchedEffect(list) {
        snapshotFlow {
                Triple(
                    list.isScrollInProgress,
                    list.canScrollForward,
                    list.layoutInfo.visibleItemsInfo.lastOrNull()?.let {
                        it.offset + it.size - list.layoutInfo.viewportEndOffset < AUTO_FOLLOW_DISTANCE_PX &&
                            it.index == list.layoutInfo.totalItemsCount - 1
                    } == true,
                )
            }
            .collect { (scrolling, forward, nearBottom) ->
                if (scrolling && !programmaticScroll) following = !forward || nearBottom
            }
    }
    LaunchedEffect(conversation.threadId, conversation.rows.lastOrNull()) {
        if (conversation.rows.isNotEmpty() && (!initialized || following)) {
            programmaticScroll = true
            try {
                list.scrollToItem(conversation.rows.size + if (conversation.hasMoreHistory) 1 else 0)
            } finally {
                programmaticScroll = false
            }
            initialized = true
        }
    }
    Column(modifier.fillMaxSize()) {
        Row(Modifier.fillMaxWidth().padding(horizontal = 20.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(
                conversation.project,
                Modifier.weight(1f),
                color = T3.color("textMuted"),
                style = MaterialTheme.typography.labelSmall,
            )
            Box {
                TextButton(onClick = { actions = true }) { Text("•••") }
                DropdownMenu(actions, { actions = false }) {
                    conversation.threadId?.let { id ->
                        if (conversation.canMergeBack)
                            DropdownMenuItem(
                                text = { Text("Merge back to source") },
                                onClick = {
                                    actions = false
                                    model.perform(Intent.MergeBack)
                                },
                            )
                        ThreadActionItems(model, id, conversation.pinned, conversation.archived, conversation.settled) {
                            actions = false
                        }
                        if (conversation.snoozed)
                            DropdownMenuItem(
                                text = { Text("Unsnooze") },
                                onClick = {
                                    actions = false
                                    model.perform(Intent.Thread(id, ThreadAction.Unsnooze))
                                },
                            )
                        DropdownMenuItem(
                            text = { Text("Rename") },
                            onClick = {
                                actions = false
                                title = conversation.title
                                rename = true
                            },
                        )
                        DropdownMenuItem(
                            text = {
                                Text(if (conversation.autoSettle) "Disable auto-settle" else "Enable auto-settle")
                            },
                            onClick = {
                                actions = false
                                model.perform(Intent.Thread(id, ThreadAction.AutoSettle(!conversation.autoSettle)))
                            },
                        )
                    }
                    if (conversation.agents.rows.isNotEmpty())
                        DropdownMenuItem(
                            text = { Text("Agents") },
                            onClick = {
                                actions = false
                                agents = true
                            },
                        )
                    HorizontalDivider()
                    listOf("Terminal", "Files", "Diff", "Browser").forEach { tool ->
                        DropdownMenuItem(
                            text = { Text(tool) },
                            enabled = tool != "Terminal" || model.snapshot.canOpenTerminal(),
                            onClick = {
                                actions = false
                                tools = tool
                            },
                        )
                    }
                }
            }
        }
        LazyColumn(
            state = list,
            modifier = Modifier.weight(1f).fillMaxWidth(),
            contentPadding = PaddingValues(horizontal = 20.dp, vertical = 18.dp),
            verticalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            if (conversation.hasMoreHistory)
                item(key = "history") {
                    TextButton(onClick = { model.perform(Intent.LoadHistory) }) { Text("Load earlier messages") }
                }
            if (conversation.loading && conversation.rows.isEmpty()) item { CircularProgressIndicator() }
            items(conversation.rows, key = { it.id }) { row -> TimelineCard(model, row) }
            item(key = "conversation-bottom") { Spacer(Modifier.height(1.dp)) }
        }
        if (conversation.requests.isNotEmpty()) {
            LazyColumn(Modifier.fillMaxWidth().heightIn(max = 280.dp)) {
                items(conversation.requests, key = { it.id }) { request -> RequestCard(model, request) }
            }
        }
        ThreadComposer(model, queue = { queue = true }, agents = { agents = true })
    }
    if (queue) QueueSheet(model) { queue = false }
    if (rename)
        AlertDialog(
            onDismissRequest = { rename = false },
            title = { Text("Rename thread") },
            text = { OutlinedTextField(title, { title = it }) },
            confirmButton = {
                TextButton(
                    onClick = {
                        conversation.threadId?.let { model.perform(Intent.Thread(it, ThreadAction.Rename(title))) }
                        rename = false
                    }
                ) {
                    Text("Save")
                }
            },
            dismissButton = { TextButton(onClick = { rename = false }) { Text("Cancel") } },
        )
    if (agents)
        ModalBottomSheet(onDismissRequest = { agents = false }, containerColor = T3.color("canvas")) {
            Text("Agents", Modifier.padding(horizontal = 20.dp), style = MaterialTheme.typography.titleMedium)
            LazyColumn(contentPadding = PaddingValues(horizontal = 20.dp, vertical = 14.dp)) {
                items(conversation.agents.rows, key = { it.id }) { agent ->
                    TextButton(
                        enabled = agent.childThreadId != null,
                        onClick = {
                            agent.childThreadId?.let {
                                agents = false
                                model.perform(Intent.OpenThread(it))
                            }
                        },
                    ) {
                        Column(Modifier.fillMaxWidth().padding(vertical = 8.dp)) {
                            Text(agent.title, style = MaterialTheme.typography.labelLarge)
                            Text(
                                agent.metadata,
                                style = MaterialTheme.typography.bodySmall,
                                color = T3.color("textMuted"),
                            )
                            if (agent.detail.isNotEmpty())
                                Text(agent.detail, style = MaterialTheme.typography.bodySmall)
                        }
                    }
                    HorizontalDivider()
                }
            }
        }
    when (tools) {
        "Terminal" -> key(model.profileId) { TerminalDialog(model.snapshot, model::perform) { tools = null } }
        "Files",
        "Diff",
        "Browser" -> WorkspaceDialog(model, tools!!) { tools = null }
    }
}

@Composable
// Declarative native layout; the conversation decisions are supplied by core.
@Suppress("LongMethod", "CyclomaticComplexMethod")
internal fun TimelineCard(model: AndroidAppModel, row: TimelineRow) {
    when (row.kind) {
        RowKind.USER ->
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                Column(
                    Modifier.widthIn(max = 600.dp)
                        .background(T3.color("mobileUserBubble"), RoundedCornerShape(14.dp))
                        .padding(12.dp)
                ) {
                    if (row.title.isNotEmpty())
                        Text(row.title, style = MaterialTheme.typography.labelSmall, color = T3.color("textMuted"))
                    androidx.compose.foundation.text.selection.SelectionContainer { Text(row.text) }
                }
            }
        RowKind.ASSISTANT ->
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                ConversationBody(row.text)
                if (row.streaming) CircularProgressIndicator(Modifier.size(14.dp), strokeWidth = 1.dp)
                else
                    Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                        CopyButton(row.text)
                        val source = row.forkSourceThreadId
                        val run = row.runId
                        if (source != null && run != null)
                            TextButton(onClick = { model.perform(Intent.Fork(source, run)) }) { Text("Fork") }
                    }
            }
        RowKind.APPROVAL,
        RowKind.QUESTION -> RequestCard(model, row)
        RowKind.WORK -> {
            var expanded by remember(row.id) { mutableStateOf(false) }
            Surface(color = T3.color("mobileGroupedCard"), shape = RoundedCornerShape(10.dp)) {
                Column(Modifier.fillMaxWidth().padding(10.dp)) {
                    TextButton(onClick = { expanded = !expanded }) {
                        Text(
                            "${if (expanded) "⌄" else "›"} ${row.title} · ${row.status}",
                            style = MaterialTheme.typography.labelLarge,
                        )
                    }
                    if (expanded)
                        row.work.forEach { work ->
                            Column(Modifier.padding(vertical = 6.dp)) {
                                Text(work.title, style = MaterialTheme.typography.labelLarge)
                                if (work.detail.isNotEmpty())
                                    androidx.compose.foundation.text.selection.SelectionContainer {
                                        Text(
                                            work.detail,
                                            fontFamily = androidx.compose.ui.text.font.FontFamily.Monospace,
                                            style = MaterialTheme.typography.bodySmall,
                                        )
                                    }
                                work.childThreadId?.let { id ->
                                    TextButton(onClick = { model.perform(Intent.OpenThread(id)) }) {
                                        Text("Open subagent thread")
                                    }
                                }
                                Text(
                                    work.status,
                                    style = MaterialTheme.typography.labelSmall,
                                    color = T3.color("textMuted"),
                                )
                            }
                        }
                }
            }
        }
        RowKind.PLAN ->
            Surface(color = T3.color("mobileGroupedCard"), shape = RoundedCornerShape(12.dp)) {
                Column(Modifier.fillMaxWidth().padding(14.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                    Text(row.title.ifEmpty { "Proposed plan" }, style = MaterialTheme.typography.titleSmall)
                    ConversationBody(row.text)
                }
            }
        RowKind.DIFF -> {
            var expanded by remember(row.id) { mutableStateOf(false) }
            Column {
                TextButton(onClick = { expanded = !expanded }) {
                    Text("${if (expanded) "⌄" else "›"} ${row.title.ifEmpty { "File changes" }}")
                }
                if (expanded)
                    androidx.compose.foundation.text.selection.SelectionContainer {
                        Text(row.text, fontFamily = androidx.compose.ui.text.font.FontFamily.Monospace)
                    }
            }
        }
        RowKind.NOTICE,
        RowKind.ERROR ->
            Column {
                if (row.title.isNotEmpty()) Text(row.title, style = MaterialTheme.typography.labelLarge)
                Text(
                    row.text,
                    style = MaterialTheme.typography.bodyMedium,
                    color = T3.color(if (row.kind == RowKind.ERROR) "errorForeground" else "textMuted"),
                )
            }
    }
}

@Composable
// Declarative native layout; the conversation decisions are supplied by core.
@Suppress("LongMethod", "CyclomaticComplexMethod")
private fun ThreadComposer(model: AndroidAppModel, queue: () -> Unit, agents: () -> Unit) {
    val composer = model.conversation.composer
    val draft = model.snapshot.draft()
    val choices = model.snapshot.modelChoices()
    val selected = choices.firstOrNull { it.selected }?.model
    var modelMenu by remember { mutableStateOf(false) }
    var modeMenu by remember { mutableStateOf(false) }
    var behaviorMenu by remember { mutableStateOf(false) }
    fun select(value: Model, instanceId: String, effort: String?, tier: String?) {
        model.perform(Intent.SetModel(instanceId, value.model.id, effort, tier))
        modelMenu = false
    }
    Column(
        Modifier.fillMaxWidth().imePadding().padding(horizontal = 14.dp, vertical = 10.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        model.conversation.agents.pillLabel?.let { label -> TextButton(onClick = agents) { Text("Agents $label") } }
        if (composer.queueCount > 0u)
            TextButton(onClick = queue) {
                Text(
                    "${composer.queueCount} queued${if (composer.queueHeld) " · paused" else ""}",
                    style = MaterialTheme.typography.bodySmall,
                )
            }
        if (composer.editing)
            Row {
                Text("Editing queued message", Modifier.weight(1f), style = MaterialTheme.typography.bodySmall)
                TextButton(onClick = { model.perform(Intent.Queue(QueueAction.CancelEdit)) }) { Text("Cancel") }
            }
        composer.pendingDeliveries.forEach { id ->
            TextButton(onClick = { model.perform(Intent.DiscardPending(id)) }) {
                Text("Delivery unconfirmed · Stop retrying")
            }
        }
        composer.notice?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = T3.color("textMuted")) }
        Surface(
            color = T3.color("mobileComposer").copy(alpha = .9f),
            shape = RoundedCornerShape(18.dp),
            border = androidx.compose.foundation.BorderStroke(1.dp, T3.color("border")),
        ) {
            Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                androidx.compose.foundation.text.BasicTextField(
                    model.composerText,
                    model::editDraft,
                    Modifier.fillMaxWidth(),
                    textStyle = MaterialTheme.typography.bodyLarge.copy(color = T3.color("text")),
                    enabled = composer.canEdit,
                    minLines = 2,
                    maxLines = 8,
                    cursorBrush = androidx.compose.ui.graphics.SolidColor(T3.color("text")),
                    decorationBox = { input ->
                        Box {
                            if (model.composerText.isEmpty()) Text(composer.placeholder, color = T3.color("textMuted"))
                            input()
                        }
                    },
                )
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Box(Modifier.weight(1f)) {
                        TextButton(onClick = { modelMenu = true }, contentPadding = PaddingValues()) {
                            ProviderIcon(composer.providerKind, Modifier.size(13.dp))
                            Spacer(Modifier.width(4.dp))
                            Text(
                                selected?.displayName ?: draft.model,
                                style = MaterialTheme.typography.bodySmall,
                                maxLines = 1,
                            )
                        }
                        DropdownMenu(modelMenu, { modelMenu = false }) {
                            choices.forEach { choice ->
                                val value = choice.model
                                DropdownMenuItem(
                                    text = { Text(value.displayName) },
                                    onClick = {
                                        select(
                                            value,
                                            choice.instanceId,
                                            value.defaultReasoningEffort,
                                            value.defaultServiceTier,
                                        )
                                    },
                                )
                            }
                            selected?.let { value ->
                                HorizontalDivider()
                                value.supportedReasoningEfforts.forEach { effort ->
                                    DropdownMenuItem(
                                        text = { Text(effort.reasoningEffort) },
                                        onClick = {
                                            select(value, draft.instanceId, effort.reasoningEffort, draft.serviceTier)
                                        },
                                    )
                                }
                                value.serviceTiers.orEmpty().forEach { tier ->
                                    DropdownMenuItem(
                                        text = { Text(tier.name ?: tier.id) },
                                        onClick = { select(value, draft.instanceId, draft.effort, tier.id) },
                                    )
                                }
                            }
                        }
                    }
                    Box {
                        TextButton(onClick = { modeMenu = true }, contentPadding = PaddingValues(4.dp)) {
                            Text(
                                if (draft.interactionMode == "plan") "Plan" else "Mode",
                                style = MaterialTheme.typography.bodySmall,
                            )
                        }
                        DropdownMenu(modeMenu, { modeMenu = false }) {
                            runtimeModeChoices().forEach { mode ->
                                DropdownMenuItem(
                                    text = { Text("${if (draft.runtimeMode == mode.id) "✓ " else ""}${mode.label}") },
                                    onClick = {
                                        modeMenu = false
                                        model.perform(Intent.SetRuntimeMode(mode.id))
                                    },
                                )
                            }
                            HorizontalDivider()
                            dev.remoteagent.core.interactionModeChoices().forEach { mode ->
                                DropdownMenuItem(
                                    text = { Text(mode.label) },
                                    onClick = {
                                        modeMenu = false
                                        model.perform(Intent.SetInteractionMode(mode.id))
                                    },
                                )
                            }
                        }
                    }
                    if (composer.canStop)
                        TextButton(onClick = { model.perform(Intent.Stop) }, contentPadding = PaddingValues(4.dp)) {
                            Text("■")
                        }
                    if (composer.canSteer || composer.canRestart)
                        Box {
                            TextButton(
                                onClick = { behaviorMenu = true },
                                enabled = composer.enabled,
                                contentPadding = PaddingValues(4.dp),
                            ) {
                                Text("▾")
                            }
                            DropdownMenu(behaviorMenu, { behaviorMenu = false }) {
                                if (composer.canSteer)
                                    DropdownMenuItem(
                                        text = { Text("Steer") },
                                        onClick = {
                                            behaviorMenu = false
                                            model.perform(Intent.Send(SendBehavior.STEER))
                                        },
                                    )
                                if (composer.canRestart)
                                    DropdownMenuItem(
                                        text = { Text("Restart with message") },
                                        onClick = {
                                            behaviorMenu = false
                                            model.perform(Intent.Send(SendBehavior.RESTART))
                                        },
                                    )
                            }
                        }
                    FilledIconButton(
                        onClick = {
                            if (composer.editing) model.perform(Intent.Queue(QueueAction.SaveEdit))
                            else model.perform(Intent.Send(SendBehavior.DEFAULT))
                        },
                        enabled = composer.enabled,
                        colors =
                            IconButtonDefaults.filledIconButtonColors(
                                containerColor = T3.color("text"),
                                contentColor = T3.color("canvas"),
                            ),
                    ) {
                        Text("↑")
                    }
                }
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun QueueSheet(model: AndroidAppModel, dismiss: () -> Unit) {
    ModalBottomSheet(onDismissRequest = dismiss, containerColor = T3.color("canvas")) {
        LazyColumn(contentPadding = PaddingValues(20.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            item { Text("Queue", style = MaterialTheme.typography.titleLarge) }
            if (model.conversation.composer.queueHeld)
                item { Button(onClick = { model.perform(Intent.Queue(QueueAction.Resume)) }) { Text("Resume queue") } }
            items(model.conversation.queue, key = { it.runId }) { row ->
                Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text(row.text, maxLines = 5)
                    Text(row.model, style = MaterialTheme.typography.labelSmall, color = T3.color("textMuted"))
                    Row {
                        if (row.canEdit) TextButton(
                            onClick = {
                                model.perform(Intent.Queue(QueueAction.Edit(row.runId)))
                                dismiss()
                            }
                        ) {
                            Text("Edit")
                        }
                        if (row.canSteer)
                            TextButton(onClick = { model.perform(Intent.Queue(QueueAction.Steer(row.runId))) }) {
                                Text("Steer")
                            }
                        TextButton(onClick = { model.perform(Intent.Queue(QueueAction.Cancel(row.runId))) }) {
                            Text("Cancel")
                        }
                        TextButton(
                            onClick = {
                                val ids = model.conversation.queue.map { it.runId }.toMutableList()
                                val index = ids.indexOf(row.runId)
                                if (index > 0) {
                                    ids.removeAt(index)
                                    ids.add(index - 1, row.runId)
                                    model.perform(Intent.Queue(QueueAction.Reorder(ids)))
                                }
                            }
                        ) {
                            Text("↑")
                        }
                    }
                }
            }
        }
    }
}

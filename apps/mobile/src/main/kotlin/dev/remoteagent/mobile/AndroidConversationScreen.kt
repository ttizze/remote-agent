package dev.remoteagent.mobile

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.*
import kotlinx.coroutines.flow.collect

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun ThreadDetailScreen(model: AndroidAppModel, modifier: Modifier = Modifier) {
    val conversation = model.conversation
    val list = rememberLazyListState()
    var queue by remember { mutableStateOf(false) }
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
                        it.offset + it.size - list.layoutInfo.viewportEndOffset < 80 &&
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
                list.scrollToItem(conversation.rows.lastIndex + if (conversation.hasMoreHistory) 1 else 0)
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
                    HorizontalDivider()
                    listOf("Terminal", "Files", "Diff", "Browser").forEach { tool ->
                        DropdownMenuItem(
                            text = { Text(tool) },
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
        }
        ThreadComposer(model) { queue = true }
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
    when (tools) {
        "Terminal" -> TerminalDialog(model.snapshot, model::perform) { tools = null }
        "Files",
        "Diff",
        "Browser" -> WorkspaceDialog(model, tools!!) { tools = null }
    }
}

@Composable
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
                else CopyButton(row.text)
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
        RowKind.NOTICE,
        RowKind.ERROR,
        RowKind.DIFF ->
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
private fun ThreadComposer(model: AndroidAppModel, queue: () -> Unit) {
    val composer = model.conversation.composer
    val draft = model.snapshot.draft()
    val models = model.snapshot.models()
    val selected = models.firstOrNull {
        it.model.id == draft.model &&
            (if (it.model.provider == ProviderKind.CLAUDE) "claude" else "codex") == draft.instanceId
    }
    var modelMenu by remember { mutableStateOf(false) }
    var modeMenu by remember { mutableStateOf(false) }
    var behaviorMenu by remember { mutableStateOf(false) }
    fun select(value: Model, effort: String?, tier: String?) {
        model.perform(
            Intent.SetModel(
                if (value.model.provider == ProviderKind.CLAUDE) "claude" else "codex",
                value.model.id,
                effort,
                tier,
            )
        )
        modelMenu = false
    }
    Column(
        Modifier.fillMaxWidth().imePadding().padding(horizontal = 14.dp, vertical = 10.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
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
                            ProviderIcon(draft.instanceId, Modifier.size(13.dp))
                            Spacer(Modifier.width(4.dp))
                            Text(
                                selected?.displayName ?: draft.model,
                                style = MaterialTheme.typography.bodySmall,
                                maxLines = 1,
                            )
                        }
                        DropdownMenu(modelMenu, { modelMenu = false }) {
                            models.forEach { value ->
                                DropdownMenuItem(
                                    text = { Text(value.displayName) },
                                    onClick = { select(value, value.defaultReasoningEffort, value.defaultServiceTier) },
                                )
                            }
                            selected?.let { value ->
                                HorizontalDivider()
                                value.supportedReasoningEfforts.forEach { effort ->
                                    DropdownMenuItem(
                                        text = { Text(effort.reasoningEffort) },
                                        onClick = { select(value, effort.reasoningEffort, draft.serviceTier) },
                                    )
                                }
                                value.serviceTiers.orEmpty().forEach { tier ->
                                    DropdownMenuItem(
                                        text = { Text(tier.name ?: tier.id) },
                                        onClick = { select(value, draft.effort, tier.id) },
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
                            listOf("approval-required", "auto-accept-edits", "auto", "full-access").forEach { mode ->
                                DropdownMenuItem(
                                    text = { Text(mode) },
                                    onClick = {
                                        modeMenu = false
                                        model.perform(Intent.SetRuntimeMode(mode))
                                    },
                                )
                            }
                            HorizontalDivider()
                            listOf("default", "plan").forEach { mode ->
                                DropdownMenuItem(
                                    text = { Text(if (mode == "default") "Chat" else "Plan") },
                                    onClick = {
                                        modeMenu = false
                                        model.perform(Intent.SetInteractionMode(mode))
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
                        TextButton(
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

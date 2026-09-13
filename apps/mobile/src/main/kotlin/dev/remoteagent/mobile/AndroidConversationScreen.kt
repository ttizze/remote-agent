package dev.remoteagent.mobile

import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.runtime.withFrameNanos
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.ActivityExpansion
import dev.remoteagent.core.ActivityPresentation
import dev.remoteagent.core.ConversationRow
import dev.remoteagent.core.ConversationRowContent
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Interrupt
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.ReadItem
import dev.remoteagent.core.Respond
import dev.remoteagent.core.Snapshot
import dev.remoteagent.core.activityIsExpanded
import java.util.UUID

@Composable
internal fun ThreadDetailScreen(
    snapshot: Snapshot,
    projection: ConversationProjection?,
    perform: (Intent, (Result<Outcome>) -> Unit) -> Unit,
    older: ((String?) -> Unit)?,
    modifier: Modifier = Modifier,
    composer: @Composable (() -> Unit) -> Unit,
) {
    val threadId = snapshot.navigation().threadId
    val listState = rememberLazyListState()
    var activityExpansion by remember(threadId) { mutableStateOf(emptyMap<String, ActivityExpansion>()) }
    var following by remember { mutableStateOf(true) }
    ObserveFollowing(listState) { following = it }
    LaunchedEffect(snapshot) {
        if (following && older != null) {
            withFrameNanos {}
            val last = listState.layoutInfo.totalItemsCount - 1
            if (last >= 0) listState.scrollToItem(last)
        }
    }
    Column(modifier.fillMaxSize()) {
        LazyColumn(
            state = listState,
            modifier = Modifier.weight(1f),
            contentPadding = PaddingValues(16.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            if (threadId?.let(snapshot::conversation)?.historyCursor() != null)
                item(key = "history:turns") {
                    Button(
                        onClick = {
                            following = false
                            older?.invoke(null)
                        },
                        enabled = older != null,
                    ) {
                        Text("以前の会話を読み込む")
                    }
                }
            conversationRows(projection?.rows.orEmpty(), activityExpansion) { content ->
                ConversationContent(
                    content,
                    threadId,
                    perform,
                    older =
                        older?.let { load ->
                            { turnId ->
                                following = false
                                load(turnId)
                            }
                        },
                    activityHeader = { activity ->
                        ActivityHeader(activity, activityExpansion[activity.id]) { choice ->
                            activityExpansion = activityExpansion + (activity.id to choice)
                        }
                    },
                )
            }
            items(projection?.queued.orEmpty(), key = { "queued:${it.id()}" }) {
                Text("順番待ち")
                ThreadMessageCard(it, true)
            }
        }
        composer { following = true }
    }
}

private fun LazyListScope.conversationRows(
    rows: List<ConversationRow>,
    activityExpansion: Map<String, ActivityExpansion>,
    render: @Composable (ConversationRowContent) -> Unit,
) {
    var expanded = false
    rows.forEach { row ->
        val content = row.content
        if (content is ConversationRowContent.ActivityHeader) {
            expanded = activityIsExpanded(content.activity, activityExpansion[content.activity.id])
        }
        if (content !is ConversationRowContent.Activity || expanded) {
            item(key = row.id) { render(content) }
        }
    }
}

@Composable
private fun ConversationContent(
    content: ConversationRowContent,
    threadId: String?,
    perform: (Intent, (Result<Outcome>) -> Unit) -> Unit,
    older: ((String) -> Unit)?,
    activityHeader: @Composable (ActivityPresentation) -> Unit,
) {
    when (content) {
        is ConversationRowContent.OlderItems ->
            Button(onClick = { older?.invoke(content.turnId) }, enabled = older != null) { Text("途中の履歴を読み込む") }
        is ConversationRowContent.User -> ThreadMessageCard(content.item, true)
        is ConversationRowContent.Response -> ThreadMessageCard(content.item, false)
        is ConversationRowContent.Activity ->
            ThreadActivityCard(content.item) { itemId ->
                if (threadId != null) perform(Intent.ReadItem(ReadItem(threadId, content.turnId, itemId))) {}
            }
        is ConversationRowContent.ActivityHeader -> activityHeader(content.activity)
        is ConversationRowContent.PendingRequest ->
            RequestCard(content.request) { answer, complete ->
                perform(Intent.Respond(Respond(content.request.id, answer))) { complete(it.exceptionOrNull()?.message) }
            }
        is ConversationRowContent.Error -> {
            Text(content.error.title, style = MaterialTheme.typography.labelLarge)
            Text(content.error.message, color = MaterialTheme.colorScheme.error)
            content.error.details?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
        }
        is ConversationRowContent.InProgress ->
            Button(
                onClick = { if (threadId != null) perform(Intent.Interrupt(Interrupt(threadId, content.turnId))) {} }
            ) {
                Text("停止")
            }
    }
}

@Composable
private fun ActivityHeader(
    activity: ActivityPresentation,
    choice: ActivityExpansion?,
    toggle: (ActivityExpansion) -> Unit,
) {
    val expanded = activityIsExpanded(activity, choice)
    TextButton(onClick = { if (activity.activityCanCollapse) toggle(ActivityExpansion(activity.status, !expanded)) }) {
        Text(activity.activitySummary + if (activity.activityCanCollapse) if (expanded) " ⌄" else " ›" else "")
    }
}

@Composable
internal fun ThreadComposer(
    snapshot: Snapshot,
    perform: (Intent, (Result<Outcome>) -> Unit) -> Unit,
    onSend: () -> Unit,
    attach: @Composable () -> Unit,
) {
    val navigation = snapshot.navigation()
    val draft = snapshot.draft(navigation.draftKey)
    var sending by remember { mutableStateOf(false) }
    Column(Modifier.padding(12.dp)) {
        draft.attachments.forEachIndexed { index, attachment ->
            Row {
                Text(attachment.name, Modifier.weight(1f))
                TextButton(onClick = { perform(Intent.RemoveAttachment(navigation.draftKey, index.toUInt())) {} }) {
                    Text("削除")
                }
            }
        }
        OutlinedTextField(
            draft.text,
            { perform(Intent.SetDraftText(navigation.draftKey, it)) {} },
            Modifier.fillMaxWidth(),
            label = { Text("Codexへの入力") },
            minLines = 2,
        )
        Row {
            attach()
            Button(
                onClick = {
                    sending = true
                    onSend()
                    perform(Intent.Submit(navigation.threadId, UUID.randomUUID().toString())) { sending = false }
                },
                enabled = !sending && (draft.text.isNotBlank() || draft.attachments.isNotEmpty()),
            ) {
                Text("送信")
            }
        }
    }
}

@Composable
internal fun AttachmentButton(selectionKey: String, attach: (String, Uri, () -> Unit) -> Unit) {
    var transferring by remember { mutableStateOf(false) }
    var selection by remember { mutableStateOf("") }
    val picker =
        rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
            if (uri != null) {
                transferring = true
                attach(selection, uri) { transferring = false }
            }
        }
    Button(
        onClick = {
            selection = selectionKey
            picker.launch(arrayOf("*/*"))
        },
        enabled = !transferring,
    ) {
        Text(if (transferring) "添付中…" else "添付")
    }
}

@Composable
internal fun ConversationHeader(title: String?, showThreads: () -> Unit) {
    Row {
        Button(onClick = showThreads) { Text("タスク一覧") }
        Text(title?.ifEmpty { "タスク" } ?: "チャット", Modifier.padding(12.dp))
    }
}

@Composable
private fun ObserveFollowing(listState: androidx.compose.foundation.lazy.LazyListState, follow: (Boolean) -> Unit) {
    LaunchedEffect(listState) {
        snapshotFlow {
                listState.isScrollInProgress to
                    (listState.layoutInfo.visibleItemsInfo.lastOrNull()?.index ==
                        listState.layoutInfo.totalItemsCount - 1)
            }
            .collect { (scrolling, bottom) -> if (scrolling) follow(bottom) }
    }
}

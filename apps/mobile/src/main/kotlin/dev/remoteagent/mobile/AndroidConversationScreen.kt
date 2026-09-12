package dev.remoteagent.mobile

import android.content.Context
import android.net.Uri
import android.provider.OpenableColumns
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.LazyColumn
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
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.runtime.withFrameNanos
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.Interrupt
import dev.remoteagent.core.UploadAttachment
import dev.remoteagent.core.Attachment
import dev.remoteagent.core.Intent
import dev.remoteagent.core.ConversationRow
import dev.remoteagent.core.ActivityPresentation
import dev.remoteagent.core.ConversationRowContent
import dev.remoteagent.core.ActivityExpansion
import dev.remoteagent.core.activityIsExpanded
import java.io.File
import java.io.IOException
import java.util.UUID
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

@Composable
internal fun ThreadDetailScreen(model: AndroidAppModel, modifier: Modifier) {
    val snapshot = model.snapshot
    val threadId = snapshot.navigation().threadId
    val thread = threadId?.let { snapshot.conversation(it) }
    val projection = remember(model.profileId, threadId) { ConversationProjection() }
    val rows = projection.project(snapshot, thread)
    val listState = rememberLazyListState()
    val activityExpansion = remember(threadId) { mutableStateMapOf<String, ActivityExpansion>() }
    var following by remember(model.selectionKey) { mutableStateOf(true) }
    LaunchedEffect(listState) {
        snapshotFlow {
                listState.isScrollInProgress to
                    (listState.layoutInfo.visibleItemsInfo.lastOrNull()?.index ==
                        listState.layoutInfo.totalItemsCount - 1)
            }
            .collect { (scrolling, bottom) -> if (scrolling) following = bottom }
    }
    LaunchedEffect(snapshot) {
        if (following && !model.loadingHistory) {
            withFrameNanos {}
            val last = listState.layoutInfo.totalItemsCount - 1
            if (last >= 0) listState.scrollToItem(last)
        }
    }
    Column(modifier.fillMaxSize()) {
        ConversationHeader(model, thread?.title())
        LazyColumn(
            state = listState,
            modifier = Modifier.weight(1f),
            contentPadding = PaddingValues(16.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            if (thread?.historyCursor() != null)
                item(key = "history:turns") {
                    Button(
                        onClick = {
                            following = false
                            model.older(null)
                        },
                        enabled = !model.loadingHistory,
                    ) {
                        Text("以前の会話を読み込む")
                    }
                }
            conversationRows(rows, model, threadId, activityExpansion) { turnId ->
                following = false
                model.older(turnId)
            }
            items(projection.queued, key = { "queued:${it.id()}" }) {
                Text("順番待ち")
                ThreadMessageCard(it, true)
            }
        }
        ThreadComposer(model) { following = true }
    }
}

private fun LazyListScope.conversationRows(
    rows: List<ConversationRow>,
    model: AndroidAppModel,
    threadId: String?,
    activityExpansion: MutableMap<String, ActivityExpansion>,
    older: (String) -> Unit,
) {
    var expanded = false
    rows.forEach { row ->
        val content = row.content
        if (content is ConversationRowContent.ActivityHeader) {
            expanded = activityIsExpanded(content.activity, activityExpansion[content.activity.id])
        }
        if (content !is ConversationRowContent.Activity || expanded) {
            item(key = row.id) { ConversationContent(content, model, threadId, activityExpansion, older) }
        }
    }
}

@Composable
private fun ConversationContent(
    content: ConversationRowContent,
    model: AndroidAppModel,
    threadId: String?,
    activityExpansion: MutableMap<String, ActivityExpansion>,
    older: (String) -> Unit,
) {
    when (content) {
        is ConversationRowContent.OlderItems -> Button(
            onClick = { older(content.turnId) }, enabled = !model.loadingHistory,
        ) {
            Text("途中の履歴を読み込む")
        }
        is ConversationRowContent.User -> ThreadMessageCard(content.item, true)
        is ConversationRowContent.Response -> ThreadMessageCard(content.item, false)
        is ConversationRowContent.Activity -> ThreadActivityCard(content.item, model, content.turnId)
        is ConversationRowContent.ActivityHeader -> ActivityHeader(content.activity, activityExpansion)
        is ConversationRowContent.PendingRequest -> RequestCard(content.request, model)
        is ConversationRowContent.Error -> {
            Text(content.error.title, style = MaterialTheme.typography.labelLarge)
            Text(content.error.message, color = MaterialTheme.colorScheme.error)
            content.error.details?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
        }
        is ConversationRowContent.InProgress -> Button(onClick = {
            if (threadId != null) model.perform(Intent.Interrupt(Interrupt(threadId, content.turnId)))
        }) { Text("停止") }
    }
}

@Composable
private fun ActivityHeader(activity: ActivityPresentation, expansion: MutableMap<String, ActivityExpansion>) {
    val expanded = activityIsExpanded(activity, expansion[activity.id])
    TextButton(onClick = {
        if (activity.activityCanCollapse) expansion[activity.id] = ActivityExpansion(activity.status, !expanded)
    }) {
        Text(activity.activitySummary + if (activity.activityCanCollapse) if (expanded) " ⌄" else " ›" else "")
    }
}

@Composable
private fun ThreadComposer(model: AndroidAppModel, onSend: () -> Unit) {
    var sending by remember { mutableStateOf(false) }
    Column(Modifier.padding(12.dp)) {
        model.draft.attachments.forEachIndexed { index, attachment ->
            Row {
                Text(attachment.name, Modifier.weight(1f))
                TextButton(onClick = { model.perform(Intent.RemoveAttachment(model.draftKey, index.toUInt())) }) {
                    Text("削除")
                }
            }
        }
        OutlinedTextField(
            model.draft.text,
            { model.perform(Intent.SetDraftText(model.draftKey, it)) },
            Modifier.fillMaxWidth(),
            label = { Text("Codexへの入力") },
            minLines = 2,
        )
        Row {
            AttachmentButton(model)
            Button(
                onClick = {
                    sending = true
                    onSend()
                    model.send { sending = false }
                },
                enabled = !sending && (model.draft.text.isNotBlank() || model.draft.attachments.isNotEmpty()),
            ) {
                Text("送信")
            }
        }
    }
}

@Composable
private fun AttachmentButton(model: AndroidAppModel) {
    var transferring by remember { mutableStateOf(false) }
    var selection by remember { mutableStateOf("") }
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    val picker =
        rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
            if (uri != null && selection == model.selectionKey) {
                val key = model.draftKey
                val directory = model.snapshot.navigation().cwd
                transferring = true
                scope.launch {
                    var local: File? = null
                    try {
                        val attachment = withContext(Dispatchers.IO) { importAttachment(context, uri) }
                        local = File(attachment.path)
                        if (selection != model.selectionKey) {
                            local.parentFile?.deleteRecursively()
                            transferring = false
                            return@launch
                        }
                        model.perform(Intent.UploadAttachment(UploadAttachment(key, attachment, directory))) {
                            local.parentFile?.deleteRecursively()
                            transferring = false
                        }
                    } catch (error: SecurityException) {
                        local?.parentFile?.deleteRecursively()
                        transferring = false
                        model.notice = error.message
                    } catch (error: IOException) {
                        local?.parentFile?.deleteRecursively()
                        transferring = false
                        model.notice = error.message
                    }
                }
            }
        }
    Button(
        onClick = {
            selection = model.selectionKey
            picker.launch(arrayOf("*/*"))
        },
        enabled = !transferring,
    ) {
        Text(if (transferring) "添付中…" else "添付")
    }
}

private fun importAttachment(context: Context, uri: Uri): Attachment {
    val resolver = context.contentResolver
    val name =
        resolver
            .query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)
            ?.use { if (it.moveToFirst()) it.getString(0) else null }
            ?.substringAfterLast('/') ?: "attachment"
    val folder = File(context.cacheDir, UUID.randomUUID().toString())
    if (!folder.mkdir()) throw IOException("添付用フォルダを作成できません")
    var imported = false
    try {
        val file = File(folder, name)
        resolver.openInputStream(uri)?.use { input -> file.outputStream().use(input::copyTo) }
            ?: throw IOException("添付ファイルを開けません")
        val attachment = Attachment(file.path, name, resolver.getType(uri)?.startsWith("image/") == true)
        imported = true
        return attachment
    } finally {
        if (!imported) folder.deleteRecursively()
    }
}

@Composable
private fun ConversationHeader(model: AndroidAppModel, title: String?) {
    Row {
        Button(onClick = model::showThreads) { Text("タスク一覧") }
        Text(title?.ifEmpty { "タスク" } ?: "チャット", Modifier.padding(12.dp))
    }
}

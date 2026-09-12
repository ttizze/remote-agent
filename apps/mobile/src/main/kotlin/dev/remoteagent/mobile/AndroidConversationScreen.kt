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
import dev.remoteagent.core.TurnPresentationData
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
    val conversation = model.conversation
    val turns = remember(conversation) { conversation?.rows().orEmpty() }
    val queued = remember(conversation) { conversation?.queued().orEmpty() }
    val listState = rememberLazyListState()
    val activityExpansion = remember(threadId) { mutableStateMapOf<String, Pair<String, Boolean>>() }
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
            turns.forEach { turn ->
                conversationTurn(turn, model, threadId, activityExpansion) { id ->
                    following = false
                    model.older(id)
                }
            }
            items(queued, key = { "queued:${it.id()}" }) {
                Text("順番待ち")
                ThreadMessageCard(it, true)
            }
        }
        ThreadComposer(model) { following = true }
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

private fun LazyListScope.conversationTurn(
    turn: TurnPresentationData,
    model: AndroidAppModel,
    threadId: String?,
    activityExpansion: MutableMap<String, Pair<String, Boolean>>,
    older: (String) -> Unit,
) {
    val id = turn.id
    turn.openingUserMessage?.let { item(key = "opening:$id") { ThreadMessageCard(it, true) } }
    if (turn.hasOlderItems)
        item(key = "history:$id") {
            Button(onClick = { older(turn.turnId) }, enabled = !model.loadingHistory) { Text("途中の履歴を読み込む") }
        }
    items(turn.userMessages, key = { "$id:user:${it.id()}" }) { ThreadMessageCard(it, true) }
    conversationActivity(turn, model, activityExpansion)
    items(turn.pendingRequests, key = { "$id:request:${it.key}" }) { RequestCard(it, model) }
    turn.error?.let { error ->
        item(key = "$id:error") {
            Text(error.title, style = MaterialTheme.typography.labelLarge)
            Text(error.message, color = MaterialTheme.colorScheme.error)
            error.details?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
        }
    }
    items(turn.responses, key = { "$id:response:${it.id()}" }) { ThreadMessageCard(it, false) }
    if (turn.isInProgress)
        item(key = "$id:stop") {
            Button(onClick = {
                if (threadId != null) model.perform(Intent.Interrupt(Interrupt(threadId, turn.turnId)))
            }) {
                Text("停止")
            }
        }
}

private fun LazyListScope.conversationActivity(
    turn: TurnPresentationData,
    model: AndroidAppModel,
    activityExpansion: MutableMap<String, Pair<String, Boolean>>,
) {
    val id = turn.id
    if (turn.activitySummary != null) {
        val expanded =
            activityExpansion[id]?.takeIf { it.first == turn.status }?.second ?: turn.activityInitiallyExpanded
        item(key = "$id:activity") {
            TextButton(onClick = { if (turn.activityCanCollapse) activityExpansion[id] = turn.status to !expanded }) {
                Text(turn.activitySummary + if (turn.activityCanCollapse) if (expanded) " ⌄" else " ›" else "")
            }
        }
        if (expanded)
            items(turn.activityItems, key = { "$id:activity:${it.id()}" }) {
                ThreadActivityCard(it, model, turn.turnId)
            }
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

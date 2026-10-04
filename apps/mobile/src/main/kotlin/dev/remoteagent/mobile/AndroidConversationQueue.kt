package dev.remoteagent.mobile

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.QueueAction
import dev.remoteagent.core.QueueMessage

private const val THUMBNAIL_LIMIT = 3

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun ConversationQueuePanel(
    messages: List<QueueMessage>,
    held: Boolean,
    enabled: Boolean,
    perform: (Intent, (Result<Outcome>) -> Unit) -> Unit,
    action: (QueueAction) -> Unit,
) {
    var showing by remember { mutableStateOf(false) }
    var editing by remember { mutableStateOf<QueueMessage?>(null) }
    if (messages.isNotEmpty() || held) {
        TextButton(onClick = { showing = true }, modifier = Modifier.testTag("queue.open")) {
            Text("キュー ${messages.size}")
        }
    }
    if (showing) {
        ModalBottomSheet(onDismissRequest = { showing = false }, containerColor = MaterialTheme.colorScheme.surface) {
            Column(Modifier.fillMaxWidth().padding(horizontal = 20.dp).padding(bottom = 24.dp)) {
                Row(
                    Modifier.fillMaxWidth(),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.SpaceBetween,
                ) {
                    Text("キュー", style = MaterialTheme.typography.titleMedium)
                    TextButton(
                        onClick = { action(if (held) QueueAction.Resume else QueueAction.Pause) },
                        enabled = enabled,
                        modifier = Modifier.testTag("queue.toggle"),
                    ) {
                        Text(if (held) "再開" else "一時停止")
                    }
                }
                if (held)
                    Text(
                        "キューは停止中です",
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(vertical = 12.dp),
                    )
                Column(Modifier.heightIn(min = 240.dp, max = 600.dp).verticalScroll(rememberScrollState())) {
                    if (messages.isEmpty()) Text("待機中のメッセージはありません", Modifier.padding(top = 24.dp))
                    messages.forEach { message -> QueueRow(message, enabled, perform, action) { editing = message } }
                }
            }
        }
    }
    editing?.let { message ->
        QueueEditor(message.id, message.text, enabled, { editing = null }) { text ->
            action(QueueAction.Edit(message.id, text))
            editing = null
        }
    }
}

@Composable
private fun QueueRow(
    message: QueueMessage,
    enabled: Boolean,
    perform: (Intent, (Result<Outcome>) -> Unit) -> Unit,
    action: (QueueAction) -> Unit,
    edit: () -> Unit,
) {
    Row(
        Modifier.fillMaxWidth().heightIn(min = 56.dp).testTag("queue.item.${message.id}"),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
            message.images.take(THUMBNAIL_LIMIT).forEach { path ->
                AttachmentThumbnail(
                    path,
                    "キューの画像",
                    perform,
                    Modifier.size(24.dp).clip(RoundedCornerShape(4.dp)),
                    contentScale = ContentScale.Crop,
                )
            }
            if (message.images.size > THUMBNAIL_LIMIT)
                Text("+${message.images.size - THUMBNAIL_LIMIT}", style = MaterialTheme.typography.labelSmall)
        }
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(message.text, maxLines = 1)
            Text(
                message.status,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        QueueMenu(message, enabled, action, edit)
    }
}

@Composable
private fun QueueMenu(message: QueueMessage, enabled: Boolean, action: (QueueAction) -> Unit, edit: () -> Unit) {
    var showing by remember(message.id) { mutableStateOf(false) }
    Box {
        IconButton(
            onClick = { showing = true },
            enabled = enabled,
            modifier = Modifier.semantics { contentDescription = "キューの操作" },
        ) {
            Text("⋮")
        }
        DropdownMenu(expanded = showing, onDismissRequest = { showing = false }) {
            message.moveUp?.let { move ->
                DropdownMenuItem(
                    text = { Text("上へ移動") },
                    onClick = {
                        showing = false
                        action(move)
                    },
                )
            }
            message.moveDown?.let { move ->
                DropdownMenuItem(
                    text = { Text("下へ移動") },
                    onClick = {
                        showing = false
                        action(move)
                    },
                )
            }
            if (message.editable)
                DropdownMenuItem(
                    text = { Text("編集") },
                    onClick = {
                        showing = false
                        edit()
                    },
                )
            if (message.removable)
                DropdownMenuItem(
                    text = { Text("削除") },
                    onClick = {
                        showing = false
                        action(QueueAction.Cancel(message.id))
                    },
                )
        }
    }
}

@Composable
private fun QueueEditor(id: String, original: String, enabled: Boolean, dismiss: () -> Unit, save: (String) -> Unit) {
    var text by remember(id) { mutableStateOf(original) }
    AlertDialog(
        onDismissRequest = dismiss,
        title = { Text("キューを編集") },
        text = { OutlinedTextField(text, { text = it }, label = { Text("メッセージ") }, minLines = 3) },
        confirmButton = { TextButton(onClick = { save(text) }, enabled = enabled && text.isNotBlank()) { Text("保存") } },
        dismissButton = { TextButton(onClick = dismiss) { Text("キャンセル") } },
    )
}

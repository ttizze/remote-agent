package dev.remoteagent.mobile

import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.ComposerControls
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.Snapshot
import dev.remoteagent.core.insertInvocation
import java.util.UUID

@Composable
internal fun ThreadComposer(
    snapshot: Snapshot,
    perform: (Intent, (Result<Outcome>) -> Unit) -> Unit,
    onSend: () -> Unit,
    attach: @Composable () -> Unit,
) {
    val navigation = snapshot.navigation()
    val draftKey = snapshot.composerDraftKey()
    val draft = snapshot.draft(draftKey)
    var sending by remember { mutableStateOf(false) }
    val session = navigation.threadId
    val thread = session?.let(snapshot::conversation)
    val controls = snapshot.composerControls(sending)
    val send: (Boolean) -> Unit = { queue ->
        sending = true
        onSend()
        val intent =
            if (queue && session != null) Intent.Queue(session, UUID.randomUUID().toString())
            else Intent.Submit(navigation.threadId, UUID.randomUUID().toString())
        perform(intent) { sending = false }
    }
    Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        if (session != null && thread != null) {
            ConversationQueuePanel(
                snapshot.queueMessages(),
                thread.queueHeld(),
                snapshot.connected() && !sending,
                perform,
            ) { action ->
                perform(Intent.QueueControl(dev.remoteagent.core.QueueControl(session, action))) {}
            }
        }
        thread?.inputUnavailableReason()?.let { Text(it) }
        val cursor = draft.text.toByteArray(Charsets.UTF_8).size.toUInt()
        ComposerInvocationPicker(snapshot.composerSuggestions(draft.text, cursor)) { invocation ->
            insertInvocation(draft.text, cursor, invocation.kind, invocation.name)?.let {
                perform(Intent.InsertInvocation(draftKey, it.text, invocation)) {}
            }
        }
        androidx.compose.material3.Surface(
            shape = androidx.compose.foundation.shape.RoundedCornerShape(26.dp),
            color = MaterialTheme.colorScheme.surfaceContainer,
            border =
                androidx.compose.foundation.BorderStroke(1.dp, MaterialTheme.colorScheme.outline.copy(alpha = 0.8f)),
        ) {
            Column(Modifier.padding(12.dp)) {
                DraftAttachments(draft.attachments, draftKey, perform)
                ComposerInput(draft.text) {
                    perform(Intent.EditComposer(draftKey, it, it.toByteArray(Charsets.UTF_8).size.toUInt())) {}
                }
                ComposerActions(
                    controls,
                    { send(false) },
                    navigation.threadId?.takeIf { !controls.editing }?.let { { send(true) } },
                    if (controls.editing) ({ perform(Intent.CancelQueueEdit) {} }) else null,
                    attach,
                )
            }
        }
    }
}

@Composable
private fun ComposerInput(text: String, change: (String) -> Unit) {
    androidx.compose.foundation.text.BasicTextField(
        value = text,
        onValueChange = change,
        modifier = Modifier.fillMaxWidth().padding(vertical = 8.dp),
        textStyle = MaterialTheme.typography.bodyLarge.copy(color = MaterialTheme.colorScheme.onSurface),
        minLines = 2,
        maxLines = 8,
        decorationBox = { editor ->
            Box {
                if (text.isEmpty()) Text("AI に依頼する", color = MaterialTheme.colorScheme.onSurfaceVariant)
                editor()
            }
        },
    )
}

@Composable
private fun ComposerActions(
    controls: ComposerControls,
    send: () -> Unit,
    queue: (() -> Unit)?,
    cancelEdit: (() -> Unit)?,
    attach: @Composable () -> Unit,
) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        attach()
        androidx.compose.foundation.layout.Spacer(Modifier.weight(1f))
        cancelEdit?.let { TextButton(onClick = it) { Text("キャンセル") } }
        queue?.let { TextButton(onClick = it, enabled = controls.queueEnabled) { Text("キューに追加") } }
        FilledIconButton(
            onClick = send,
            enabled = controls.sendEnabled,
            modifier = Modifier.semantics { contentDescription = if (controls.editing) "変更を保存" else "送信" },
        ) {
            Text(if (controls.editing) "✓" else "↑")
        }
    }
}

@Composable
internal fun AttachmentButton(
    selectionKey: Pair<String?, dev.remoteagent.core.DraftKey>,
    attach: (Pair<String?, dev.remoteagent.core.DraftKey>, Uri, () -> Unit) -> Unit,
) {
    var transferring by remember { mutableStateOf(false) }
    var selection by remember { mutableStateOf(selectionKey) }
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

package dev.remoteagent.mobile

import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
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
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.isAltPressed
import androidx.compose.ui.input.key.isCtrlPressed
import androidx.compose.ui.input.key.isMetaPressed
import androidx.compose.ui.input.key.isShiftPressed
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.ComposerAction
import dev.remoteagent.core.ComposerControls
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.Snapshot
import dev.remoteagent.core.composerActionLabel
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
    val send: (Boolean, Boolean) -> Unit = { queue, alternate ->
        sending = true
        onSend()
        val intent =
            if (queue && session != null) Intent.Queue(session, UUID.randomUUID().toString())
            else Intent.Submit(navigation.threadId, UUID.randomUUID().toString(), alternate)
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
                ComposerInput(draft.text, { alternate -> if (controls.sendEnabled) send(false, alternate) }) {
                    perform(Intent.EditComposer(draftKey, it, it.toByteArray(Charsets.UTF_8).size.toUInt())) {}
                }
                ConversationModelPicker(snapshot, snapshot.connected() && !sending, perform)
                ComposerActions(
                    controls,
                    { alternate -> send(false, alternate) },
                    navigation.threadId
                        ?.takeIf { controls.action != ComposerAction.SAVE }
                        ?.let { { send(true, false) } },
                    if (controls.action == ComposerAction.SAVE) ({ perform(Intent.CancelQueueEdit) {} }) else null,
                    attach,
                )
            }
        }
    }
}

@Composable
private fun ComposerInput(text: String, submit: (Boolean) -> Unit, change: (String) -> Unit) {
    var value by remember { mutableStateOf(androidx.compose.ui.text.input.TextFieldValue(text)) }
    if (value.text != text) {
        value =
            androidx.compose.ui.text.input.TextFieldValue(
                text,
                androidx.compose.ui.text.TextRange(value.selection.end.coerceAtMost(text.length)),
            )
    }
    androidx.compose.foundation.text.BasicTextField(
        value = value,
        onValueChange = {
            value = it
            change(it.text)
        },
        modifier =
            Modifier.fillMaxWidth().padding(vertical = 8.dp).onPreviewKeyEvent {
                if (it.isShiftPressed || it.isAltPressed || value.composition != null) return@onPreviewKeyEvent false
                val commandReturn = it.key == Key.Enter && (it.isMetaPressed || it.isCtrlPressed)
                if (it.type == KeyEventType.KeyDown && commandReturn) {
                    submit(true)
                    true
                } else false
            },
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
    send: (Boolean) -> Unit,
    queue: (() -> Unit)?,
    cancelEdit: (() -> Unit)?,
    attach: @Composable () -> Unit,
) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        attach()
        androidx.compose.foundation.layout.Spacer(Modifier.weight(1f))
        cancelEdit?.let { TextButton(onClick = it) { Text("キャンセル") } }
        queue?.let { TextButton(onClick = it, enabled = controls.queueEnabled) { Text("キューに追加") } }
        SendActionButton(controls, send)
    }
}

@Composable
private fun SendActionButton(controls: ComposerControls, send: (Boolean) -> Unit) {
    var menu by remember(controls.action, controls.sendEnabled, controls.alternateAction) { mutableStateOf(false) }
    Box(
        Modifier.size(44.dp)
            .combinedClickable(
                enabled = controls.sendEnabled,
                role = Role.Button,
                onClick = { send(false) },
                onLongClick = controls.alternateAction?.let { { menu = true } },
            )
            .semantics { contentDescription = composerActionLabel(controls.action) },
        contentAlignment = Alignment.Center,
    ) {
        Box(
            Modifier.size(30.dp)
                .background(
                    MaterialTheme.colorScheme.primary.copy(alpha = if (controls.sendEnabled) 1f else 0.15f),
                    CircleShape,
                ),
            contentAlignment = Alignment.Center,
        ) {
            Icon(
                painterResource(
                    when (controls.action) {
                        ComposerAction.SEND -> R.drawable.composer_send
                        ComposerAction.QUEUE -> R.drawable.composer_queue
                        ComposerAction.STEER -> R.drawable.composer_steer
                        ComposerAction.SAVE -> R.drawable.composer_save
                    }
                ),
                contentDescription = null,
                tint = MaterialTheme.colorScheme.onPrimary,
                modifier = Modifier.size(18.dp),
            )
        }
        DropdownMenu(
            expanded = menu && controls.sendEnabled && controls.alternateAction != null,
            onDismissRequest = { menu = false },
        ) {
            DropdownMenuItem(
                text = { Text(composerActionLabel(controls.action)) },
                onClick = {
                    menu = false
                    send(false)
                },
            )
            controls.alternateAction?.let { alternate ->
                DropdownMenuItem(
                    text = { Text(composerActionLabel(alternate)) },
                    onClick = {
                        menu = false
                        send(true)
                    },
                )
            }
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

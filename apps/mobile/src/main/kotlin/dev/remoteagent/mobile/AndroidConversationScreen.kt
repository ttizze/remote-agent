package dev.remoteagent.mobile

import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.IconButton
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.remoteagent.core.ActivityPresentation
import dev.remoteagent.core.ComposerPresentation
import dev.remoteagent.core.ConversationContext
import dev.remoteagent.core.ConversationRow
import dev.remoteagent.core.ConversationRowContent
import dev.remoteagent.core.ConversationRows
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Interrupt
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.ReadItem
import dev.remoteagent.core.Respond
import dev.remoteagent.core.VisibleConversationRow
import dev.remoteagent.core.notificationText
import java.util.UUID
import kotlinx.coroutines.launch

@Composable
internal fun ThreadDetailScreen(
    context: ConversationContext,
    projection: ConversationRows?,
    actions: ConversationActions,
    modifier: Modifier = Modifier,
    scrollToTopRequest: Int = 0,
    composer: @Composable (() -> Unit) -> Unit,
) {
    val perform = actions.perform
    val threadId = context.navigation().threadId
    var setupTerminal by remember(threadId) { mutableStateOf<dev.remoteagent.core.SetupTerminal?>(null) }
    var recoveryExpanded by remember(threadId, context.navigation().draftKey) { mutableStateOf(false) }
    val listState = rememberLazyListState()
    var following by remember(threadId) { mutableStateOf(true) }
    ObserveFollowing(listState, projection, following) { following = it }
    ObserveHistoryPrefetch(listState, threadId, context.history(), following, perform)
    LaunchedEffect(scrollToTopRequest) {
        if (scrollToTopRequest > 0) {
            following = false
            listState.scrollToItem(0)
        }
    }
    val turns = remember(projection) { projection?.turnRows().orEmpty() }
    val queued =
        remember(projection) {
            projection?.queuedRows().orEmpty().map { it to (it.data().content as ConversationRowContent.User).item }
        }
    val sessionRows = remember(projection) { projection?.sessionRows().orEmpty() }
    Column(modifier.fillMaxSize()) {
        ErrorNotices(context.notices(), perform)
        Box(Modifier.weight(1f).fillMaxWidth()) {
            LazyColumn(
                state = listState,
                modifier = Modifier.fillMaxSize(),
                contentPadding = PaddingValues(16.dp),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                historyControl(threadId, context.history(), perform)
                projection?.source()?.historyNotice()?.let { notice ->
                    item(key = "history:read-state") { Text(notice) }
                }
                val renderRow: @Composable (VisibleConversationRow) -> Unit = { projected ->
                    val row = remember(projected) { projected.data() }
                    ConversationContent(
                        row,
                        context,
                        perform,
                        openSetupTerminal = { setupTerminal = it },
                        activityHeader = { activity ->
                            ActivityHeader(activity, projected.expanded()) { actions.toggleActivity(projected.id()) }
                        },
                    )
                }
                conversationRows(turns, renderRow)
                pendingInputs(queued, context.navigation().cwd, perform, recoveryExpanded) { recoveryExpanded = it }
                conversationRows(sessionRows, renderRow)
            }
            LatestMessageButton(listState) { following = true }
        }
        composer { following = true }
    }
    setupTerminal?.let { terminal ->
        TerminalDialog(actions.terminals, terminal.cwd, perform, terminal.handle) { setupTerminal = null }
    }
}

@Composable
private fun BoxScope.LatestMessageButton(
    listState: androidx.compose.foundation.lazy.LazyListState,
    follow: () -> Unit,
) {
    val language = LocalUiLanguage.current
    val scope = rememberCoroutineScope()
    if (listState.canScrollForward) {
        FilledIconButton(
            onClick = {
                scope.launch {
                    listState.scrollToLatest()
                    follow()
                }
            },
            colors =
                IconButtonDefaults.filledIconButtonColors(containerColor = Color.DarkGray, contentColor = Color.White),
            modifier =
                Modifier.align(Alignment.BottomCenter).padding(bottom = 8.dp).semantics {
                    contentDescription = language.text("最新のメッセージへ")
                },
        ) {
            Text("↓")
        }
    }
}

private fun LazyListScope.conversationRows(
    rows: List<VisibleConversationRow>,
    render: @Composable (VisibleConversationRow) -> Unit,
) {
    rows.forEach { row -> item(key = row.id()) { render(row) } }
}

@Composable
private fun ConversationContent(
    row: ConversationRow,
    context: ConversationContext,
    perform: (Intent, (Result<Outcome>) -> Unit) -> Unit,
    openSetupTerminal: (dev.remoteagent.core.SetupTerminal) -> Unit,
    activityHeader: @Composable (ActivityPresentation) -> Unit,
) {
    val threadId = context.navigation().threadId
    val cwd = context.navigation().cwd
    val language = LocalUiLanguage.current
    val timestampMs = row.timestampMs
    when (val content = row.content) {
        is ConversationRowContent.Preparation -> PreparationCard(content.preparation, perform, openSetupTerminal)
        is ConversationRowContent.ProviderHandoff ->
            Text(
                content.text,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        is ConversationRowContent.Notification ->
            Text(
                notificationText(content.reports, language),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        is ConversationRowContent.User -> ThreadMessageCard(content.item, true, cwd, perform, timestampMs)
        is ConversationRowContent.Response -> ConversationResponse(content, context, perform, timestampMs)
        is ConversationRowContent.Activity ->
            ThreadActivityCard(content.item, threadId, cwd, perform) { itemId ->
                if (threadId != null) perform(Intent.ReadItem(ReadItem(threadId, content.turnId, itemId))) {}
            }
        is ConversationRowContent.ActivityHeader -> activityHeader(content.activity)
        is ConversationRowContent.PendingRequest ->
            RequestCard(content.request) { answer, complete ->
                perform(Intent.Respond(Respond(content.request.id, answer))) {
                    complete(it.exceptionOrNull()?.displayMessage())
                }
            }
        is ConversationRowContent.Error -> TurnErrorCard(content.error)
        is ConversationRowContent.InProgress -> TurnStopButton(threadId, content.turnId, perform)
    }
}

@Composable
private fun ConversationResponse(
    content: ConversationRowContent.Response,
    context: ConversationContext,
    perform: (Intent, (Result<Outcome>) -> Unit) -> Unit,
    timestampMs: ULong?,
) {
    val threadId = context.navigation().threadId
    val turnId = content.forkTurnId
    ThreadMessageCard(
        content.item,
        false,
        context.navigation().cwd,
        perform,
        timestampMs,
        showActions = content.showActions,
        fork =
            if (threadId != null && turnId != null)
                ({ perform(Intent.ForkSession(dev.remoteagent.core.ForkSession(threadId, turnId))) {} })
            else null,
        forkPhase = turnId?.let { context.forkPhase(it) },
    )
}

@Composable
private fun TurnStopButton(
    threadId: dev.remoteagent.core.SessionRef?,
    turnId: dev.remoteagent.core.TurnId,
    perform: (Intent, (Result<Outcome>) -> Unit) -> Unit,
) {
    val language = LocalUiLanguage.current
    Button(onClick = { if (threadId != null) perform(Intent.Interrupt(Interrupt(threadId, turnId))) {} }) {
        Text(language.text("停止"))
    }
}

@Composable
private fun ActivityHeader(
    activity: ActivityPresentation,
    expanded: Boolean,
    toggle: () -> Unit,
) {
    TextButton(onClick = toggle) {
        Text(activity.activitySummary + if (activity.activityCanCollapse) if (expanded) " ⌄" else " ›" else "")
    }
}

@Composable
internal fun ThreadComposer(
    composer: ComposerPresentation,
    perform: (Intent, (Result<Outcome>) -> Unit) -> Unit,
    onSend: () -> Unit,
    settings: @Composable (dev.remoteagent.core.ModelRef?, () -> Unit) -> Unit,
    attach: @Composable ((Boolean) -> Unit) -> Unit,
) {
    val language = LocalUiLanguage.current
    val controls = composer.controls()
    val navigation = controls.navigation
    val draft = composer.draft()
    val sending = controls.sending
    var uploading by remember(navigation.draftKey) { mutableStateOf(false) }
    val inputUnavailable = controls.unavailable
    val activeTurn = controls.runningTurn
    Surface(
        Modifier.padding(horizontal = 12.dp, vertical = 8.dp).testTag("conversation.composer"),
        shape = RoundedCornerShape(28.dp),
        color = MaterialTheme.colorScheme.surfaceVariant,
    ) {
        Column(Modifier.padding(8.dp)) {
            inputUnavailable?.let { Text(it) }
            DraftAttachments(draft.attachments, navigation.draftKey, perform)
            WorkspacePicker(controls, perform)
            ComposerTextInput(composer, perform)
            Row(verticalAlignment = Alignment.CenterVertically) {
                attach { uploading = it }
                ModelPickerButton(composer, perform, Modifier.weight(1f), settings)
                VoiceInputButton(composer, perform, enabled = inputUnavailable == null && !sending)
                if (navigation.threadId != null && activeTurn != null) {
                    ComposerStopButton(
                        Interrupt(navigation.threadId!!, activeTurn),
                        controls.interruptPhase,
                        perform,
                    )
                } else
                    FilledIconButton(
                        onClick = {
                            onSend()
                            perform(Intent.Submit(navigation.threadId, UUID.randomUUID().toString(), false)) {}
                        },
                        enabled =
                            composer.canSend(draft.text.isNotBlank() || draft.attachments.isNotEmpty()) && !uploading,
                    ) {
                        if (sending) CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
                        else
                            Text(
                                "↑",
                                fontSize = 24.sp,
                                modifier = Modifier.semantics { contentDescription = language.text("送信") },
                            )
                    }
            }
        }
    }
}

@Composable
internal fun AttachmentButton(
    selectionKey: Pair<String?, dev.remoteagent.core.DraftKey>,
    attach: (Pair<String?, dev.remoteagent.core.DraftKey>, Uri, () -> Unit) -> Unit,
    enabled: Boolean,
    transferState: (Boolean) -> Unit = {},
) {
    val language = LocalUiLanguage.current
    var transferring by remember { mutableStateOf(false) }
    var selection by remember { mutableStateOf(selectionKey) }
    val picker =
        rememberLauncherForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) { uris ->
            if (uris.isNotEmpty()) {
                transferring = true
                transferState(true)
                fun uploadNext(index: Int) {
                    if (index == uris.size) {
                        transferring = false
                        transferState(false)
                    } else attach(selection, uris[index]) { uploadNext(index + 1) }
                }
                uploadNext(0)
            }
        }
    IconButton(
        onClick = {
            selection = selectionKey
            picker.launch(arrayOf("*/*"))
        },
        enabled = enabled && !transferring,
        modifier =
            Modifier.semantics {
                contentDescription = if (transferring) language.text("アップロード中") else language.text("写真やファイルを添付")
            },
    ) {
        if (transferring) CircularProgressIndicator(Modifier.size(22.dp), strokeWidth = 2.dp)
        else Text("+", fontSize = 32.sp)
    }
}

@Composable
internal fun ConversationHeader(title: String?, showThreads: () -> Unit, scrollToTop: () -> Unit) {
    val language = LocalUiLanguage.current
    Row(Modifier.fillMaxWidth().padding(8.dp), verticalAlignment = Alignment.CenterVertically) {
        IconButton(
            onClick = showThreads,
            modifier = Modifier.semantics { contentDescription = language.text("タスク一覧に戻る") },
        ) {
            Text("‹", fontSize = 32.sp)
        }
        TextButton(
            onClick = scrollToTop,
            colors = ButtonDefaults.textButtonColors(contentColor = MaterialTheme.colorScheme.onSurface),
            modifier = Modifier.weight(1f).semantics { contentDescription = language.text("会話の先頭へ") },
        ) {
            Text(
                title?.ifEmpty { language.text("タスク") } ?: language.text("新しいチャット"),
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

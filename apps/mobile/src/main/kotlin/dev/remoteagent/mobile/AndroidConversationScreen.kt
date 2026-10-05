package dev.remoteagent.mobile

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
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
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.runtime.withFrameNanos
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.ActivityExpansion
import dev.remoteagent.core.ActivityPresentation
import dev.remoteagent.core.ConversationRow
import dev.remoteagent.core.ConversationRowContent
import dev.remoteagent.core.ImportHistory
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Interrupt
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.ReadItem
import dev.remoteagent.core.Respond
import dev.remoteagent.core.Snapshot
import dev.remoteagent.core.activityIsExpanded
import dev.remoteagent.core.shouldLoadHistory
import kotlinx.coroutines.launch

@Composable
internal fun ThreadDetailScreen(
    snapshot: Snapshot,
    projection: ConversationProjection?,
    perform: (Intent, (Result<Outcome>) -> Unit) -> Unit,
    older: (() -> Unit)?,
    modifier: Modifier = Modifier,
    scrollToTopRequest: Int = 0,
    composer: @Composable (() -> Unit) -> Unit,
) {
    val threadId = snapshot.navigation().threadId
    val listState = rememberLazyListState()
    var activityExpansion by remember(threadId) { mutableStateOf(emptyMap<String, ActivityExpansion>()) }
    var following by remember(threadId) { mutableStateOf(true) }
    LaunchedEffect(scrollToTopRequest) {
        if (scrollToTopRequest > 0) {
            following = false
            listState.scrollToItem(0)
        }
    }
    ObserveFollowing(listState, snapshot, following, { following = it }, older)
    Column(modifier.fillMaxSize()) {
        Box(Modifier.weight(1f).fillMaxWidth()) {
            LazyColumn(
                state = listState,
                modifier = Modifier.fillMaxSize(),
                contentPadding = PaddingValues(16.dp),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                threadId?.let(snapshot::conversation)?.historyNotice()?.let { notice ->
                    item(key = "history:read-state") {
                        HistoryNotice(
                            notice,
                            threadId?.let(snapshot::conversation)?.canRetryHistory() == true,
                            snapshot.connected(),
                        ) {
                            perform(Intent.ImportHistory(ImportHistory())) {}
                        }
                    }
                }
                val renderRow: @Composable (ConversationRowContent) -> Unit = { content ->
                    ConversationContent(
                        content,
                        threadId,
                        snapshot.navigation().cwd,
                        perform,
                        activityHeader = { activity ->
                            ActivityHeader(activity, activityExpansion[activity.id]) { choice ->
                                activityExpansion = activityExpansion + (activity.id to choice)
                                if (choice.expanded) activity.loadItems?.let { perform(Intent.LoadTurnItems(it)) {} }
                            }
                        },
                    )
                }
                conversationRows(projection?.rows.orEmpty(), activityExpansion, renderRow)
                items(projection?.queued.orEmpty(), key = { "queued:${it.id()}" }) {
                    ThreadMessageCard(it, true, snapshot.navigation().cwd, perform)
                }
                conversationRows(projection?.requestRows.orEmpty(), activityExpansion, renderRow)
            }
            LatestMessageButton(listState) { following = true }
        }
        composer { following = true }
    }
}

@Composable
private fun HistoryNotice(text: String, canRetry: Boolean, enabled: Boolean, retry: () -> Unit) {
    Column {
        Text(text)
        if (canRetry) {
            TextButton(onClick = retry, enabled = enabled) { Text("履歴の取り込みを再試行") }
        }
    }
}

@Composable
private fun BoxScope.LatestMessageButton(
    listState: androidx.compose.foundation.lazy.LazyListState,
    follow: () -> Unit,
) {
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
                    contentDescription = "最新のメッセージへ"
                },
        ) {
            Text("↓")
        }
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
    threadId: dev.remoteagent.core.SessionRef?,
    cwd: String,
    perform: (Intent, (Result<Outcome>) -> Unit) -> Unit,
    activityHeader: @Composable (ActivityPresentation) -> Unit,
) {
    when (content) {
        is ConversationRowContent.User -> ThreadMessageCard(content.item, true, cwd, perform)
        is ConversationRowContent.Response -> ThreadMessageCard(content.item, false, cwd, perform)
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
internal fun ConversationHeader(title: String?, showThreads: () -> Unit, scrollToTop: () -> Unit) {
    Row {
        Button(onClick = showThreads) { Text("タスク一覧") }
        TextButton(
            onClick = scrollToTop,
            colors = ButtonDefaults.textButtonColors(contentColor = MaterialTheme.colorScheme.onSurface),
            modifier = Modifier.semantics { contentDescription = "会話の先頭へ" },
        ) {
            Text(title?.ifEmpty { "タスク" } ?: "チャット")
        }
    }
}

@Composable
private fun ObserveFollowing(
    listState: androidx.compose.foundation.lazy.LazyListState,
    snapshot: Snapshot,
    following: Boolean,
    follow: (Boolean) -> Unit,
    older: (() -> Unit)?,
) {
    LaunchedEffect(snapshot) {
        if (following) {
            withFrameNanos {}
            listState.scrollToLatest()
        }
    }
    LaunchedEffect(snapshot, following, older) {
        val hasMore = snapshot.navigation().threadId?.let(snapshot::conversation)?.hasMoreHistory() == true
        snapshotFlow {
                val viewportHeight = listState.layoutInfo.viewportEndOffset - listState.layoutInfo.viewportStartOffset
                shouldLoadHistory(
                    hasMore,
                    older == null || snapshot.error() != null,
                    viewportHeight > 0 &&
                        listState.firstVisibleItemIndex == 0 &&
                        listState.firstVisibleItemScrollOffset < viewportHeight * HISTORY_PREFETCH_FRACTION,
                    !listState.canScrollForward,
                    following,
                )
            }
            .collect { needed -> if (needed) older?.invoke() }
    }
    LaunchedEffect(listState) {
        snapshotFlow { listState.isScrollInProgress to !listState.canScrollForward }
            .collect { (scrolling, bottom) -> if (scrolling) follow(bottom) }
    }
}

private suspend fun androidx.compose.foundation.lazy.LazyListState.scrollToLatest() {
    val last = layoutInfo.totalItemsCount - 1
    if (last < 0) return
    scrollToItem(last)
    scroll { scrollBy(layoutInfo.visibleItemsInfo.lastOrNull()?.size?.toFloat() ?: 0f) }
}

private const val HISTORY_PREFETCH_FRACTION = 0.6

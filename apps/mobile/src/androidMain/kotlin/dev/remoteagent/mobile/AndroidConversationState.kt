package dev.remoteagent.mobile

import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.items
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow

internal fun conversationContentVersion(snapshot: ThreadSnapshot?): String =
    snapshot
        ?.turns
        ?.joinToString("|") { turn ->
            val items = turn.items.joinToString(",") { "${it.id}:${it.threadItemContentVersion()}" }
            val requests = turn.pendingRequests.joinToString(",") { "${it.id}:${it.method}:${it.params.hashCode()}" }
            "${turn.id}:${turn.status}:${turn.error?.hashCode()}:$requests:$items"
        }
        .orEmpty() + snapshot?.submittedMessages.orEmpty().joinToString { it.clientId + ":" + it.text }

internal fun conversationOpeningMessages(snapshot: ThreadSnapshot?): Map<String, CodexItem> = buildMap {
    for (turn in snapshot?.turns.orEmpty()) {
        turn.raw
            ?.get("openingUserMessage")
            ?.let(::codexItem)
            ?.takeUnless { item -> turn.items.any { it.id == item.id } }
            ?.let { put(turn.id, it) }
    }
}

internal fun conversationRowCount(
    snapshot: ThreadSnapshot?,
    turnPresentations: List<ThreadTurnPresentation>,
    openingMessageCount: Int,
    queuedMessageCount: Int,
    activityExpansionOverrides: Map<String, Boolean>,
): Int {
    val historyRows =
        openingMessageCount +
            (if (snapshot?.olderTurnsCursor != null) 1 else 0) +
            snapshot?.turns.orEmpty().count { it.hasOlderItems }
    return historyRows +
        queuedMessageCount +
        turnPresentations.sumOf { turn ->
            val activityExpanded = activityExpansionOverrides[turn.id] ?: turn.activityInitiallyExpanded
            turn.userMessages.size +
                (if (turn.activitySummary == null) 0 else 1) +
                (if (activityExpanded) turn.activityItems.size else 0) +
                (if (turn.isLastSegment && turn.status == TurnStatus.InProgress) 1 else 0) +
                turn.pendingRequests.size +
                (if (turn.error == null) 0 else 1) +
                turn.responses.size
        }
}

@Composable
internal fun ConversationScrollEffects(
    snapshot: ThreadSnapshot?,
    state: AppState,
    listState: LazyListState,
    rowCount: Int,
    onOlderHistory: (String?) -> Unit,
) {
    val view = state.selectedView
    var followingLatest by remember(state.selectedProfileId, view.selectedThreadId) { mutableStateOf(true) }
    val contentVersion = conversationContentVersion(snapshot)
    LaunchedEffect(listState) {
        snapshotFlow {
                val layout = listState.layoutInfo
                listState.isScrollInProgress to
                    (layout.visibleItemsInfo.lastOrNull()?.index == layout.totalItemsCount - 1)
            }
            .collect { (isScrolling, isAtBottom) -> if (isScrolling) followingLatest = isAtBottom }
    }
    LaunchedEffect(listState.firstVisibleItemIndex, listState.isScrollInProgress) {
        if (listState.isScrollInProgress && !followingLatest && !view.loadingHistory) {
            val key = listState.layoutInfo.visibleItemsInfo.firstOrNull()?.key as? String
            if (key?.startsWith("history:") == true)
                onOlderHistory(key.removePrefix("history:").takeUnless { it == "turns" })
        }
    }
    LaunchedEffect(contentVersion) {
        if (followingLatest && !view.loadingHistory) {
            listState.scrollToItem(rowCount + 1)
        }
    }
}

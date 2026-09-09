package dev.remoteagent.mobile

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.MutableState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import kotlinx.atomicfu.AtomicRef
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch

internal fun LazyListScope.threadHeader(
    state: AppState,
    controller: AtomicRef<MobileApp>,
    scope: CoroutineScope,
    onOlderHistory: (String?) -> Unit,
) {
    val profile = requireNotNull(state.selectedProfile)
    val view = state.selectedView
    val selectedThreadId = view.selectedThreadId
    val snapshot = state.cache.profile(profile.id).snapshots[selectedThreadId]
    val onBack: () -> Unit = { scope.launch { controller.showThreadList(profile) } }
    val onRetry: () -> Unit = { selectedThreadId?.let { scope.launch { controller.readThread(profile, it) } } }
    item {
        Button(onClick = onBack) { Text("タスク一覧") }
        Text(
            snapshot?.summary?.name
                ?: snapshot?.summary?.preview
                ?: if (view.newThreadCwd != null) "チャット" else "タスクを読み込み中…",
            style = MaterialTheme.typography.headlineSmall,
        )
        if (view.threadDetail is LoadPhase.Failed) {
            Text(view.threadDetail.message, color = MaterialTheme.colorScheme.error)
            Button(onClick = onRetry) { Text("再試行") }
        }
    }
    if (snapshot?.olderTurnsCursor != null) {
        item(key = "history:turns") {
            Button(onClick = { onOlderHistory(null) }, enabled = !view.loadingHistory) { Text("以前の会話を読み込む") }
        }
    }
}

internal fun LazyListScope.turnHistory(
    turn: ThreadTurnPresentation,
    openingMessage: CodexItem?,
    hasOlderItems: Boolean,
    loadingHistory: Boolean,
    onOlderHistory: (String?) -> Unit,
) {
    if (turn.id == turn.turnId)
        openingMessage?.let { opening ->
            item(key = "opening:${turn.turnId}") { ThreadMessageCard(item = opening, isUser = true) }
        }
    if (turn.id == turn.turnId && hasOlderItems) {
        item(key = "history:${turn.turnId}") {
            Button(onClick = { onOlderHistory(turn.turnId) }, enabled = !loadingHistory) { Text("途中の履歴を読み込む") }
        }
    }
    items(turn.userMessages, key = { item -> "${turn.id}:user:${item.id}" }) { item ->
        ThreadMessageCard(item = item, isUser = true)
    }
}

internal fun LazyListScope.turnActivity(
    turn: ThreadTurnPresentation,
    expandedItemIds: MutableState<Set<String>>,
    activityExpansionOverrides: MutableState<Map<String, Boolean>>,
) {
    val activityExpanded = activityExpansionOverrides.value[turn.id] ?: turn.activityInitiallyExpanded
    turn.activitySummary?.let { summary ->
        item(key = "${turn.id}:activity") {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                val activityModifier =
                    if (turn.activityCanCollapse) {
                        Modifier.fillMaxWidth().clickable {
                            activityExpansionOverrides.value =
                                activityExpansionOverrides.value + (turn.id to !activityExpanded)
                        }
                    } else {
                        Modifier.fillMaxWidth()
                    }
                Row(
                    modifier = activityModifier.padding(vertical = 6.dp),
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    Text(summary, style = MaterialTheme.typography.labelMedium)
                    if (turn.activityCanCollapse) Text(if (activityExpanded) "⌄" else "›")
                }
                androidx.compose.material3.HorizontalDivider()
            }
        }
    }
    if (activityExpanded) {
        items(turn.activityItems, key = { item -> "${turn.id}:activity:${item.id}" }) { item ->
            val expansionKey = "${turn.id}:${item.id}"
            ThreadActivityCard(
                item = item,
                isExpanded = expansionKey in expandedItemIds.value,
                toggleExpanded = {
                    expandedItemIds.value =
                        if (expansionKey in expandedItemIds.value) {
                            expandedItemIds.value - expansionKey
                        } else {
                            expandedItemIds.value + expansionKey
                        }
                },
            )
        }
    }
}

internal fun LazyListScope.turnConclusion(
    turn: ThreadTurnPresentation,
    state: AppState,
    controller: AtomicRef<MobileApp>,
    scope: CoroutineScope,
) {
    val profile = requireNotNull(state.selectedProfile)
    val view = state.selectedView
    val threadId = requireNotNull(view.selectedThreadId)
    if (turn.isLastSegment && turn.status == TurnStatus.InProgress) {
        item(key = "${turn.id}:stop") {
            Button(
                onClick = { scope.launch { controller.interrupt(profile, threadId, turn.turnId) } },
                enabled = view.interruptingTurnId != turn.turnId,
            ) {
                Text(if (view.interruptingTurnId == turn.turnId) "停止中…" else "停止")
            }
        }
    }
    items(turn.pendingRequests, key = { request -> "${turn.id}:request:${request.id}" }) { request ->
        val raw =
            requireNotNull(state.cache.snapshot(profile.id, threadId))
                .turns
                .first { it.id == turn.turnId }
                .pendingRequests
                .first { it.id == request.id }
        ThreadRequestCard(request, raw.paramsObject) { answer -> controller.respond(profile, raw.raw, answer) }
    }
    turn.error?.let { error -> item(key = "${turn.id}:error") { ThreadErrorCard(error) } }
    items(turn.responses, key = { item -> "${turn.id}:response:${item.id}" }) { item ->
        ThreadMessageCard(item = item, isUser = false)
    }
}

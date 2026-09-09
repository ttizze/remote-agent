package dev.remoteagent.mobile

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.MutableState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import kotlinx.atomicfu.AtomicRef
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch

@Composable
internal fun ThreadDetailScreen(
    state: AppState,
    controller: AtomicRef<MobileApp>,
    scope: CoroutineScope,
    modifier: Modifier,
) {
    val profile = requireNotNull(state.selectedProfile)
    val view = state.selectedView
    val selectedThreadId = view.selectedThreadId
    val snapshot = state.cache.profile(profile.id).snapshots[selectedThreadId]
    val onOlderHistory: (String?) -> Unit = { scope.launch { controller.loadOlderHistory(profile, it) } }
    val composer = remember(profile.id) { mutableStateOf("") }
    val sending = remember { mutableStateOf(false) }
    val composerScope = rememberCoroutineScope()
    val listState = rememberLazyListState()
    val expandedItemIds = remember(profile.id, view.selectedThreadId) { mutableStateOf(emptySet<String>()) }
    val activityExpansionOverrides =
        remember(profile.id, view.selectedThreadId) { mutableStateOf(emptyMap<String, Boolean>()) }
    val turnPresentations = snapshot?.conversationSegments().orEmpty()
    val queuedMessages = snapshot?.submittedMessages.orEmpty().filter { it.turnId == null }

    val openingMessages = conversationOpeningMessages(snapshot)
    val rowCount =
        conversationRowCount(
            snapshot,
            turnPresentations,
            openingMessages.size,
            queuedMessages.size,
            activityExpansionOverrides.value,
        )
    ConversationScrollEffects(snapshot, state, listState, rowCount, onOlderHistory)
    LazyColumn(
        state = listState,
        modifier = modifier.fillMaxSize(),
        contentPadding = PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        threadHeader(state, controller, scope, onOlderHistory)
        turnPresentations.forEach { turn ->
            turnHistory(
                turn,
                openingMessages[turn.turnId],
                snapshot?.turns?.firstOrNull { it.id == turn.turnId }?.hasOlderItems == true,
                view.loadingHistory,
                onOlderHistory,
            )
            turnActivity(turn, expandedItemIds, activityExpansionOverrides)
            turnConclusion(turn, state, controller, scope)
        }
        items(queuedMessages, key = { "queued:${it.clientId}" }) { message -> QueuedMessage(message) }

        item {
            ConversationComposer(
                composer,
                sending,
                composerScope,
                onSend = { controller.sendMessage(profile, it).accepted },
                canSend = snapshot != null || view.newThreadCwd != null,
                notice = view.notice,
            )
        }
    }
}

@Composable
private fun ConversationComposer(
    composer: MutableState<String>,
    sending: MutableState<Boolean>,
    composerScope: CoroutineScope,
    onSend: suspend (String) -> Boolean,
    canSend: Boolean,
    notice: String? = null,
) {
    OutlinedTextField(
        value = composer.value,
        onValueChange = { composer.value = it },
        modifier = Modifier.fillMaxWidth(),
        label = { Text("Codexへの入力") },
        minLines = 3,
    )
    Button(
        onClick = {
            val text = composer.value
            sending.value = true
            composerScope.launch {
                try {
                    if (onSend(text) && composer.value == text) composer.value = ""
                } finally {
                    sending.value = false
                }
            }
        },
        enabled = !sending.value && composer.value.isNotBlank() && canSend,
    ) {
        Text("送信")
    }
    notice?.let { Text(it, color = MaterialTheme.colorScheme.error) }
}

@Composable
private fun QueuedMessage(message: SubmittedMessage) {
    Column {
        Text("順番待ち", style = MaterialTheme.typography.labelSmall)
        ThreadMessageCard(
            CodexItem.UserMessage(message.clientId, message.text, message.clientId, message.imageSources),
            isUser = true,
        )
    }
}

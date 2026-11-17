package dev.remoteagent.mobile

import androidx.compose.foundation.layout.size
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.ConversationRowContent
import dev.remoteagent.core.Intent
import dev.remoteagent.core.ItemDetail
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.RenderedItem
import dev.remoteagent.core.TurnRow
import dev.remoteagent.core.VisibleConversationRow

private const val MILLISECONDS_PER_SECOND = 1000.0

@Composable
internal fun TimelineContent(
    projected: VisibleConversationRow,
    toggle: () -> Unit,
    content: @Composable () -> Unit,
) {
    val language = LocalUiLanguage.current
    when (val timeline = projected.timeline()) {
        is TurnRow.Group -> TextButton(onClick = toggle) { Text(timeline.summary) }
        is TurnRow.Subagents -> TextButton(onClick = toggle) { Text(timeline.label) }
        is TurnRow.Live -> {
            if (!timeline.open) timeline.thought?.let { Text(it, maxLines = 2) }
            TextButton(onClick = toggle) { Text(timeline.label) }
        }
        is TurnRow.Working -> {
            val now = System.currentTimeMillis().toDouble() / MILLISECONDS_PER_SECOND
            Text(projected.progressLabel(language, now).orEmpty())
        }
        is TurnRow.Thinking -> ConversationStatusText(language.text("思考中"))
        is TurnRow.Requested -> ConversationStatusText(timeline.label)
        is TurnRow.Retrying -> ConversationStatusText(timeline.label)
        is TurnRow.Interrupted -> ConversationStatusText(language.text("実行を中断しました"))
        else -> content()
    }
}

@Composable
internal fun ConversationFooter(content: ConversationRowContent) {
    if (content is ConversationRowContent.Background) {
        ConversationStatusText(listOfNotNull(content.title, content.description).joinToString(" · "))
    }
}

@Composable
internal fun ConversationStatusText(text: String) {
    Text(text, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
}

@Composable
internal fun ActivityDetails(
    item: RenderedItem,
    deferred: Boolean,
    cwd: String,
    perform: (Intent, (Result<Outcome>) -> Unit) -> Unit,
    state: ItemDetail?,
) {
    state?.error?.let { ErrorTextView(it) }
    if (state?.loading == true) CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
    if (deferred) {
        val body = remember(item) { item.expandedBody() }
        Text(body, maxLines = 1)
    } else {
        val blocks = remember(item) { item.detailBlocks() }
        ConversationBlocks(blocks, cwd, perform)
    }
}

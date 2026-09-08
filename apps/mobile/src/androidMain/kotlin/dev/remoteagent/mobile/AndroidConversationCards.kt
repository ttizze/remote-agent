package dev.remoteagent.mobile

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp

@Composable
internal fun ThreadMessageCard(item: CodexItem, isUser: Boolean) {
    val message = item.toThreadItemPresentation().collapsedBody
    if (isUser) {
        Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
            Card {
                Column(modifier = Modifier.padding(12.dp)) {
                    Text(message)
                    (item as? CodexItem.UserMessage)?.imageSources.orEmpty().forEach { source ->
                        Text(attachmentMessageLabel(true, source, ""))
                    }
                }
            }
        }
    } else {
        Text(message, modifier = Modifier.fillMaxWidth())
    }
}

@Composable
internal fun ThreadActivityCard(item: CodexItem, isExpanded: Boolean, toggleExpanded: () -> Unit) {
    val presentation = item.toThreadItemPresentation()
    Card(
        modifier =
            Modifier.fillMaxWidth()
                .then(if (presentation.isCollapsible) Modifier.clickable(onClick = toggleExpanded) else Modifier)
    ) {
        Column(modifier = Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                if (presentation.isCollapsible) Text(if (isExpanded) "⌄" else "›")
                Text(presentation.title, style = MaterialTheme.typography.labelLarge, maxLines = 1)
            }
            val body = if (isExpanded) item.expandedThreadItemBody() else presentation.collapsedBody
            if (body.isNotEmpty()) {
                Text(text = body, maxLines = if (presentation.isCollapsible && !isExpanded) 1 else Int.MAX_VALUE)
            }
        }
    }
}

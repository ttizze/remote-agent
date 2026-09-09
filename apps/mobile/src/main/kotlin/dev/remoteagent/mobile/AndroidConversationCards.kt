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
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp

@Composable
internal fun ThreadMessageCard(item: ConversationItem, isUser: Boolean) {
    Row(Modifier.fillMaxWidth(), horizontalArrangement = if (isUser) Arrangement.End else Arrangement.Start) {
        Card {
            Column(Modifier.padding(12.dp)) {
                Text(item.body)
                item.images.forEach { Text("画像: $it", style = MaterialTheme.typography.bodySmall) }
            }
        }
    }
}

@Composable
internal fun ThreadActivityCard(item: ConversationItem, model: AndroidAppModel, turnId: String) {
    var expanded by remember(item.id) { mutableStateOf(false) }
    var detail by remember(item) { mutableStateOf<String?>(null) }
    Card(
        Modifier.fillMaxWidth()
            .then(
                if (item.collapsible)
                    Modifier.clickable {
                        expanded = !expanded
                        if (expanded && item.deferred) {
                            val threadId = model.snapshot.navigation().threadId ?: return@clickable
                            model.perform(
                                dev.remoteagent.core.Intent.ReadItem(threadId, turnId, item.source?.id() ?: item.id)
                            )
                        }
                    }
                else Modifier
            )
    ) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(
                (if (item.collapsible) if (expanded) "⌄ " else "› " else "") + item.title,
                style = MaterialTheme.typography.labelLarge,
            )
            if (expanded && detail == null) detail = item.expanded()
            Text(if (expanded) detail.orEmpty() else item.body, maxLines = if (expanded) Int.MAX_VALUE else 1)
        }
    }
}

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
import dev.remoteagent.core.Intent
import dev.remoteagent.core.ReadItem
import dev.remoteagent.core.RenderedItem

@Composable
internal fun ThreadMessageCard(item: RenderedItem, isUser: Boolean) {
    val content = remember(item) { item.presentation() }
    Row(Modifier.fillMaxWidth(), horizontalArrangement = if (isUser) Arrangement.End else Arrangement.Start) {
        Card {
            Column(Modifier.padding(12.dp)) {
                Text(content.body)
                content.imageSources.forEach { Text("画像: $it", style = MaterialTheme.typography.bodySmall) }
            }
        }
    }
}

@Composable
internal fun ThreadActivityCard(item: RenderedItem, model: AndroidAppModel, turnId: String) {
    val content = remember(item) { item.presentation() }
    var expanded by remember(content.id) { mutableStateOf(false) }
    var detail by remember(item) { mutableStateOf<String?>(null) }
    Card(
        Modifier.fillMaxWidth()
            .then(
                if (content.collapsible)
                    Modifier.clickable {
                        expanded = !expanded
                        if (expanded && content.deferred) {
                            val threadId = model.snapshot.navigation().threadId ?: return@clickable
                            model.perform(
                                Intent.ReadItem(ReadItem(threadId, turnId, content.nativeId ?: content.id))
                            )
                        }
                    }
                else Modifier
            )
    ) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(
                (if (content.collapsible) if (expanded) "⌄ " else "› " else "") + content.title,
                style = MaterialTheme.typography.labelLarge,
            )
            if (expanded && detail == null) detail = item.expandedBody()
            Text(if (expanded) detail.orEmpty() else content.body, maxLines = if (expanded) Int.MAX_VALUE else 1)
        }
    }
}

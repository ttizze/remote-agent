package dev.remoteagent.mobile

import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.RenderedItem

@Composable
internal fun ThreadMessageCard(
    item: RenderedItem,
    isUser: Boolean,
    cwd: String,
    perform: (Intent, (Result<Outcome>) -> Unit) -> Unit,
    timestampMs: ULong? = null,
    changes: List<dev.remoteagent.core.WorkspaceDiffFile> = emptyList(),
    loadChanges: () -> Unit = {},
    fork: (() -> Unit)? = null,
) {
    val content = remember(item) { item.presentation() }
    val imageFrame = Modifier.widthIn(max = 320.dp).fillMaxWidth().height(320.dp)
    Column(
        Modifier.fillMaxWidth(),
        horizontalAlignment = if (isUser) Alignment.End else Alignment.Start,
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        if (content.imagePlaceholder) {
            ImageSkeleton(imageFrame, "画像を生成中", "image.generation.skeleton")
        }
        MessageImages(content.imageSources, isUser, perform)
        content.body
            ?.takeIf { it.isNotEmpty() }
            ?.let { body ->
                if (isUser) {
                    Card(Modifier.widthIn(max = 520.dp)) {
                        Column(Modifier.padding(14.dp)) { ConversationBody(body, cwd, perform) }
                    }
                } else ConversationBody(body, cwd, perform)
            }
        if (changes.isNotEmpty()) ChangedFilesSummary(changes, loadChanges)
        if (!isUser && content.kind == "agent")
            MessageFooter(content.body.orEmpty(), timestampMs, fork)
        if (isUser) MessageTime(timestampMs)
        if (isUser && content.nativeId == null) {
            content.title?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
        }
    }
}

@Composable
private fun MessageTime(timestampMs: ULong?) {
    timestampMs
        ?.takeIf { it <= Long.MAX_VALUE.toULong() }
        ?.let {
            Text(
                java.text.DateFormat.getTimeInstance(java.text.DateFormat.SHORT)
                    .format(java.util.Date(it.toLong())),
                style = MaterialTheme.typography.labelSmall,
            )
        }
}

@Composable
private fun MessageFooter(text: String, timestampMs: ULong?, fork: (() -> Unit)?) {
    val clipboard = androidx.compose.ui.platform.LocalClipboardManager.current
    Row(verticalAlignment = Alignment.CenterVertically) {
        androidx.compose.material3.TextButton(
            onClick = { clipboard.setText(androidx.compose.ui.text.AnnotatedString(text)) }
        ) {
            Text("コピー")
        }
        fork?.let { androidx.compose.material3.TextButton(onClick = it) { Text("分岐") } }
        MessageTime(timestampMs)
    }
}

@Composable
internal fun ThreadActivityCard(
    item: RenderedItem,
    cwd: String,
    perform: (Intent, (Result<Outcome>) -> Unit) -> Unit,
    loadDetails: (String) -> Unit,
) {
    val content = remember(item) { item.presentation() }
    var expanded by remember(content.id) { mutableStateOf(false) }
    val detail = remember(item, expanded) { if (expanded) item.expandedBody() else null }
    Card(
        Modifier.fillMaxWidth()
            .then(
                if (content.collapsible)
                    Modifier.clickable {
                        expanded = !expanded
                        if (expanded && content.deferred) {
                            loadDetails(content.nativeId ?: content.id)
                        }
                    }
                else Modifier
            )
    ) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            content.title?.let { title ->
                Text(
                    (if (content.collapsible) if (expanded) "⌄ " else "› " else "") + title,
                    style = MaterialTheme.typography.labelLarge,
                )
            }
            if (expanded && !content.deferred) {
                val blocks = remember(item) { item.detailBlocks() }
                ConversationBlocks(blocks, cwd, perform)
            } else (if (expanded) detail else content.body)?.let { Text(it, maxLines = 1) }
        }
    }
}

@Composable
private fun MessageImages(
    sources: List<String>,
    isUser: Boolean,
    perform: (Intent, (Result<Outcome>) -> Unit) -> Unit,
) {
    if (isUser && sources.isNotEmpty()) {
        Row(
            Modifier.horizontalScroll(rememberScrollState()),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            sources.forEach { source ->
                AttachmentThumbnail(source, "添付画像", perform, Modifier.size(80.dp))
            }
        }
    }
    if (!isUser) {
        sources.forEach { source ->
            AttachmentThumbnail(
                source,
                "生成画像",
                perform,
                Modifier.widthIn(max = 320.dp).fillMaxWidth().height(320.dp),
            )
        }
    }
}

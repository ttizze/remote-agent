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
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.Intent
import dev.remoteagent.core.MarkdownFileKind
import dev.remoteagent.core.MarkdownFileReference
import dev.remoteagent.core.OperationPhase
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.RenderedItem

@Composable
internal fun ThreadMessageCard(
    item: RenderedItem,
    isUser: Boolean,
    cwd: String,
    perform: (Intent, (Result<Outcome>) -> Unit) -> Unit,
    timestampMs: ULong? = null,
    fork: (() -> Unit)? = null,
    forkPhase: OperationPhase? = null,
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
        if (!isUser && content.kind == "agent") MessageFooter(content.body.orEmpty(), timestampMs, fork, forkPhase)
        if (isUser) MessageFooter(content.body.orEmpty(), timestampMs, null)
        if (isUser && content.nativeId == null) {
            content.title?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
            val submissionId = remember(item) { item.unknownSubmissionId() }
            submissionId?.let { UnknownSubmissionActions(it, perform) }
        }
    }
}

@Composable
private fun MessageTime(timestampMs: ULong?) {
    timestampMs
        ?.takeIf { it <= Long.MAX_VALUE.toULong() }
        ?.let {
            Text(
                java.text.DateFormat.getTimeInstance(java.text.DateFormat.SHORT).format(java.util.Date(it.toLong())),
                style = MaterialTheme.typography.labelSmall,
            )
        }
}

@Composable
internal fun MessageFooter(text: String, timestampMs: ULong?, fork: (() -> Unit)?, forkPhase: OperationPhase? = null) {
    val clipboard = androidx.compose.ui.platform.LocalClipboardManager.current
    val forking = forkPhase is OperationPhase.Running
    var copied by remember(text) { mutableStateOf(false) }
    LaunchedEffect(copied) {
        if (copied) {
            kotlinx.coroutines.delay(COPY_NOTICE_DURATION_MS)
            copied = false
        }
    }
    Column {
        Row(verticalAlignment = Alignment.CenterVertically) {
            IconButton(
                onClick = {
                    clipboard.setText(androidx.compose.ui.text.AnnotatedString(text))
                    copied = true
                },
                enabled = text.isNotEmpty(),
            ) {
                Icon(
                    painterResource(if (copied) R.drawable.ic_check else R.drawable.ic_copy),
                    if (copied) "コピーしました" else "コピー",
                    Modifier.size(20.dp),
                )
            }
            fork?.let {
                IconButton(onClick = it, enabled = !forking) {
                    if (forking) CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
                    else Icon(painterResource(R.drawable.ic_fork), "分岐", Modifier.size(20.dp))
                }
            }
            MessageTime(timestampMs)
        }
        if (forkPhase != null)
            Text(
                if (forking) "分岐を作成中…" else "分岐できませんでした。もう一度お試しください。",
                style = MaterialTheme.typography.bodySmall,
                color =
                    if (forkPhase is OperationPhase.Failed) MaterialTheme.colorScheme.error
                    else MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
            )
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
    var preview by remember(sources) { mutableStateOf<MarkdownFileReference?>(null) }
    fun openImage(source: String) {
        preview = MarkdownFileReference(source, "画像プレビュー", MarkdownFileKind.IMAGE, null, null)
    }
    if (isUser && sources.isNotEmpty()) {
        Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            sources.forEach { source ->
                AttachmentThumbnail(
                    source,
                    "添付画像",
                    perform,
                    Modifier.size(80.dp).clickable(onClickLabel = "画像をプレビュー") { openImage(source) },
                )
            }
        }
    }
    if (!isUser) {
        sources.forEach { source ->
            AttachmentThumbnail(
                source,
                "生成画像",
                perform,
                Modifier.widthIn(max = 320.dp).fillMaxWidth().height(320.dp).clickable(onClickLabel = "画像をプレビュー") {
                    openImage(source)
                },
            )
        }
    }
    preview?.let { reference -> ConversationFilePreview(reference, perform) { preview = null } }
}

private const val COPY_NOTICE_DURATION_MS = 1_500L

package dev.remoteagent.mobile

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.withLink
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.MarkdownAlignment
import dev.remoteagent.core.MarkdownBlock
import dev.remoteagent.core.MarkdownRun
import dev.remoteagent.core.markdownBlocks
import dev.remoteagent.core.Intent
import dev.remoteagent.core.ReadItem
import dev.remoteagent.core.RenderedItem

@Composable
internal fun ThreadMessageCard(item: RenderedItem, isUser: Boolean) {
    val content = remember(item) { item.presentation() }
    Row(Modifier.fillMaxWidth(), horizontalArrangement = if (isUser) Arrangement.End else Arrangement.Start) {
        Card {
            Column(Modifier.padding(12.dp)) {
                if (isUser) Text(content.body) else ConversationBody(content.body)
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

@Composable
internal fun ConversationBody(body: String) {
    val blocks = remember(body) { markdownBlocks(body) }
    Column(verticalArrangement = Arrangement.spacedBy(14.dp)) {
        blocks.forEachIndexed { index, block ->
            when (block) {
                is MarkdownBlock.Paragraph -> Text(
                    markdownText(block.runs, header = block.style.header != null,
                                  marker = block.style.marker),
                    style = when (block.style.header?.toInt()) {
                        1 -> MaterialTheme.typography.headlineSmall
                        null -> MaterialTheme.typography.bodyLarge
                        else -> MaterialTheme.typography.titleLarge
                    },
                    fontFamily = if (block.style.code) FontFamily.Monospace else FontFamily.Default,
                    modifier = Modifier.padding(start = if (block.style.quoted) 12.dp else 0.dp),
                )
                is MarkdownBlock.Table -> MarkdownTable(block, index)
            }
        }
    }
}

@Composable
private fun MarkdownTable(table: MarkdownBlock.Table, index: Int) {
    val width = 220.dp * LocalDensity.current.fontScale
    SelectionContainer {
        Column(Modifier.horizontalScroll(rememberScrollState()).testTag("markdown.table.$index")) {
            table.rows.forEachIndexed { row, cells ->
                Row(
                    Modifier.background(
                        if (row == 0) MaterialTheme.colorScheme.surfaceVariant else Color.Transparent
                    )
                ) {
                    cells.forEachIndexed { column, cell ->
                        Text(
                            markdownText(cell.runs),
                            modifier = Modifier.width(width).padding(10.dp)
                                .testTag("markdown.cell.$index.$row.$column"),
                            style = MaterialTheme.typography.bodyLarge,
                            textAlign = when (table.columns[column]) {
                                MarkdownAlignment.LEFT -> TextAlign.Left
                                MarkdownAlignment.CENTER -> TextAlign.Center
                                MarkdownAlignment.RIGHT -> TextAlign.Right
                            },
                        )
                    }
                }
                HorizontalDivider(Modifier.width(width * table.columns.size))
            }
        }
    }
}

private fun markdownText(
    runs: List<MarkdownRun>, header: Boolean = false, marker: String? = null,
) = buildAnnotatedString {
    if (marker != null) append("$marker ")
    runs.forEach { run ->
        withStyle(SpanStyle(
            fontWeight = if (header || run.strong) FontWeight.Bold else null,
            fontStyle = if (run.emphasis) FontStyle.Italic else null,
            fontFamily = if (run.code) FontFamily.Monospace else null,
            textDecoration = if (run.strikethrough) TextDecoration.LineThrough else TextDecoration.None,
        )) {
            val link = run.link
            if (link != null) withLink(LinkAnnotation.Url(link)) { append(run.text) }
            else append(run.text)
        }
    }
}

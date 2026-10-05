package dev.remoteagent.mobile

import android.content.ClipData
import android.content.ClipboardManager
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.painterResource
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
import androidx.compose.ui.unit.sp
import dev.remoteagent.core.MarkdownAlignment
import dev.remoteagent.core.MarkdownBlock
import dev.remoteagent.core.MarkdownRun
import dev.remoteagent.core.markdownBlocks
import dev.remoteagent.core.safeMarkdownUrl

@Composable
internal fun ProviderIcon(provider: String, modifier: Modifier = Modifier) {
    Icon(
        painterResource(if (provider == "claude") R.drawable.ic_claude else R.drawable.ic_openai),
        provider,
        modifier,
        tint = T3.color("textMuted"),
    )
}

@Composable
internal fun CopyButton(text: String) {
    val context = LocalContext.current
    TextButton(
        onClick = {
            context.getSystemService(ClipboardManager::class.java).setPrimaryClip(ClipData.newPlainText("", text))
        },
        contentPadding = PaddingValues(),
    ) {
        Text("Copy", style = MaterialTheme.typography.labelSmall)
    }
}

@Composable
internal fun ConversationBody(body: String) {
    val blocks = remember(body) { markdownBlocks(body) }
    Column(verticalArrangement = Arrangement.spacedBy(14.dp)) {
        blocks.forEachIndexed { index, block ->
            when (block) {
                is MarkdownBlock.Paragraph -> {
                    if (block.style.code)
                        Surface(
                            color = T3.color("codeBackground"),
                            shape = androidx.compose.foundation.shape.RoundedCornerShape(10.dp),
                            border = BorderStroke(1.dp, T3.color("border")),
                        ) {
                            Column(Modifier.fillMaxWidth().padding(12.dp)) {
                                CopyButton(block.runs.joinToString("") { it.text })
                                SelectionContainer {
                                    Text(
                                        block.runs.joinToString("") { it.text },
                                        Modifier.horizontalScroll(rememberScrollState()),
                                        fontFamily = FontFamily.Monospace,
                                        fontSize = 13.sp,
                                        lineHeight = 19.sp,
                                    )
                                }
                            }
                        }
                    else
                        SelectionContainer {
                            Text(
                                markdownText(
                                    block.runs,
                                    header = block.style.header != null,
                                    marker = block.style.marker,
                                ),
                                style = MaterialTheme.typography.bodyLarge,
                                fontSize =
                                    when (block.style.header?.toInt()) {
                                        1 -> 21.sp
                                        2 -> 19.sp
                                        3 -> 17.sp
                                        null -> 16.sp
                                        else -> 15.sp
                                    },
                                fontFamily = T3.fonts,
                                modifier = Modifier.padding(start = if (block.style.quoted) 12.dp else 0.dp),
                            )
                        }
                }
                is MarkdownBlock.Visualization ->
                    Text(block.path, color = T3.color("textMuted"), style = MaterialTheme.typography.bodySmall)
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
                    Modifier.background(if (row == 0) MaterialTheme.colorScheme.surfaceVariant else Color.Transparent)
                ) {
                    cells.forEachIndexed { column, cell ->
                        Text(
                            markdownText(cell.runs),
                            modifier =
                                Modifier.width(width).padding(10.dp).testTag("markdown.cell.$index.$row.$column"),
                            style = MaterialTheme.typography.bodySmall,
                            textAlign =
                                when (table.columns[column]) {
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

private fun markdownText(runs: List<MarkdownRun>, header: Boolean = false, marker: String? = null) =
    buildAnnotatedString {
        if (marker != null) append("$marker ")
        runs.forEach { run ->
            withStyle(
                SpanStyle(
                    fontWeight = if (header || run.strong) FontWeight.Bold else null,
                    fontStyle = if (run.emphasis) FontStyle.Italic else null,
                    fontFamily = if (run.code) FontFamily.Monospace else null,
                    textDecoration = if (run.strikethrough) TextDecoration.LineThrough else TextDecoration.None,
                )
            ) {
                val link = run.link
                if (link != null && safeMarkdownUrl(link)) withLink(LinkAnnotation.Url(link)) { append(run.text) }
                else append(run.text)
            }
        }
    }

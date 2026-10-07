// Declarative native layout with the fixed mobile metrics; the conversation decisions are supplied by core.
@file:Suppress("TooManyFunctions", "LongMethod", "CyclomaticComplexMethod", "LongParameterList", "MagicNumber")

package dev.remoteagent.mobile

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.produceState
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.buildAnnotatedString
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
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/** Core markdown blocks at the mobile sizes: body 16/23, headings 21/19/17/15, code 13/19. */
@Composable
internal fun MarkdownText(source: String, modifier: Modifier = Modifier, color: Color = AppTheme.colors.foreground) {
    val blocks by
        produceState(emptyList<MarkdownBlock>(), source) {
            value = withContext(Dispatchers.Default) { markdownBlocks(source) }
        }
    Column(modifier, verticalArrangement = Arrangement.spacedBy(10.dp)) {
        blocks.forEachIndexed { index, block ->
            when (block) {
                is MarkdownBlock.Paragraph ->
                    if (block.style.code) CodeBlock(block.runs.joinToString("") { it.text })
                    else Paragraph(block, color)
                is MarkdownBlock.Visualization ->
                    Text(block.path, style = AppTheme.caption, color = AppTheme.colors.foregroundMuted)
                is MarkdownBlock.Table -> MarkdownTable(block, index)
            }
        }
    }
}

@Composable
private fun Paragraph(block: MarkdownBlock.Paragraph, color: Color) {
    val size =
        when (block.style.header?.toInt()) {
            null -> 16
            1 -> 21
            2 -> 19
            3 -> 17
            else -> 15
        }
    val quoted = block.style.quoted
    Row {
        if (quoted) Box(Modifier.width(3.dp).padding(vertical = 2.dp).background(AppTheme.colors.mdBlockquoteBorder))
        SelectionContainer {
            Text(
                markdownText(
                    block.runs,
                    AppTheme.colors.mdLink,
                    header = block.style.header != null,
                    marker = block.style.marker,
                ),
                Modifier.padding(start = if (quoted) 10.dp else 0.dp),
                style = AppTheme.body.copy(fontSize = size.sp, lineHeight = (size + 7).sp),
                color = color,
            )
        }
    }
}

@Composable
private fun CodeBlock(code: String) {
    Surface(
        color = AppTheme.colors.mdCodeBackground,
        shape = RoundedCornerShape(12.dp),
        border = BorderStroke(1.dp, AppTheme.colors.border),
    ) {
        SelectionContainer {
            Text(
                code,
                Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(12.dp),
                fontFamily = AppTheme.mono,
                fontSize = 13.sp,
                lineHeight = 19.sp,
                color = AppTheme.colors.foreground,
            )
        }
    }
}

@Composable
private fun MarkdownTable(table: MarkdownBlock.Table, index: Int) {
    val width = 220.dp * LocalDensity.current.fontScale
    SelectionContainer {
        Column(Modifier.horizontalScroll(rememberScrollState()).testTag("markdown.table.$index")) {
            table.rows.forEachIndexed { row, cells ->
                Row(Modifier.background(if (row == 0) AppTheme.colors.subtleStrong else Color.Transparent)) {
                    cells.forEachIndexed { column, cell ->
                        Text(
                            markdownText(cell.runs, AppTheme.colors.mdLink),
                            Modifier.width(width).padding(10.dp).testTag("markdown.cell.$index.$row.$column"),
                            style = AppTheme.caption,
                            color = AppTheme.colors.foreground,
                            textAlign =
                                when (table.columns.getOrNull(column)) {
                                    MarkdownAlignment.CENTER -> TextAlign.Center
                                    MarkdownAlignment.RIGHT -> TextAlign.Right
                                    else -> TextAlign.Left
                                },
                        )
                    }
                }
                HorizontalDivider(Modifier.width(width * table.columns.size), color = AppTheme.colors.mdHr)
            }
        }
    }
}

private fun markdownText(runs: List<MarkdownRun>, link: Color, header: Boolean = false, marker: String? = null) =
    buildAnnotatedString {
        if (marker != null) append("$marker ")
        runs.forEach { run ->
            withStyle(
                SpanStyle(
                    fontWeight = if (header || run.strong) FontWeight.Bold else null,
                    fontStyle = if (run.emphasis) FontStyle.Italic else null,
                    fontFamily = if (run.code) AppTheme.mono else null,
                    textDecoration = if (run.strikethrough) TextDecoration.LineThrough else TextDecoration.None,
                )
            ) {
                val url = run.link
                if (url != null)
                    withLink(LinkAnnotation.Url(url, TextLinkStyles(SpanStyle(color = link)))) { append(run.text) }
                else append(run.text)
            }
        }
    }

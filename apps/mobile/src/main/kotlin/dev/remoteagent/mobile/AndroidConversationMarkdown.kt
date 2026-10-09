package dev.remoteagent.mobile

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.appendInlineContent
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.semantics
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
import androidx.compose.ui.unit.em
import dev.remoteagent.core.Intent
import dev.remoteagent.core.MarkdownAlignment
import dev.remoteagent.core.MarkdownBlock
import dev.remoteagent.core.MarkdownRun
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.markdownBlocks

private const val FILE_CHIP_COLOR = 0xFF0096AF
private const val OPAQUE_ALPHA_MASK = 0xFF000000L
private const val DARK_SURFACE_LUMINANCE = 0.5f
private const val COPY_FEEDBACK_MILLIS = 1200L
private const val CHIP_DEFAULT_FONT_SP = 18f
private const val CHIP_MAX_CHARACTERS = 28
private const val CHIP_CHARACTER_WIDTH_EM = 0.56f
private const val CHIP_PADDING_EM = 2.1f
private const val CHIP_HEIGHT_EM = 1.42
private const val CHIP_BORDER_ALPHA = 0.35f
private const val CHIP_FONT_SCALE = 0.86f
private const val LIST_INDENT_DP = 22

@Composable
internal fun ConversationBody(
    body: String,
    cwd: String = "",
    perform: ((Intent, (Result<Outcome>) -> Unit) -> Unit)? = null,
) {
    val blocks = remember(body) { markdownBlocks(body) }
    ConversationBlocks(blocks, cwd, perform)
}

@Composable
internal fun ConversationBlocks(
    blocks: List<MarkdownBlock>,
    cwd: String,
    perform: ((Intent, (Result<Outcome>) -> Unit) -> Unit)?,
) {
    var file by remember { mutableStateOf<dev.remoteagent.core.MarkdownFileReference?>(null) }
    val openFile: (String) -> Unit = { source ->
        file = dev.remoteagent.core.markdownFileTarget(source, cwd)
    }
    SelectionContainer {
        Column(verticalArrangement = Arrangement.spacedBy(14.dp)) {
            blocks.forEachIndexed { index, block ->
                when (block) {
                    is MarkdownBlock.Paragraph ->
                        when {
                            block.style.rule -> HorizontalDivider(Modifier.padding(vertical = 8.dp))
                            block.style.code -> MarkdownCode(block, index)
                            else -> {
                                val style =
                                    when (block.style.header?.toInt()) {
                                        1 -> MaterialTheme.typography.headlineSmall
                                        2 -> MaterialTheme.typography.titleLarge
                                        null -> MaterialTheme.typography.bodyLarge
                                        else -> MaterialTheme.typography.titleMedium
                                    }
                                val quoteColor = MaterialTheme.colorScheme.outline
                                Box(
                                    Modifier.padding(
                                            start = (block.style.listDepth.toInt() - 1)
                                                .coerceAtLeast(0).times(LIST_INDENT_DP).dp
                                        )
                                        .then(if (block.style.quoted) {
                                            Modifier.drawBehind {
                                                drawLine(quoteColor, Offset(1.dp.toPx(), 0f),
                                                    Offset(1.dp.toPx(), size.height), 2.dp.toPx())
                                            }.padding(start = 12.dp)
                                        } else Modifier)
                                ) {
                                    MarkdownText(block.runs, style, block.style.marker, openFile)
                                }
                            }
                        }
                    is MarkdownBlock.Visualization ->
                        ConversationVisualization(block.path, cwd, perform)
                    is MarkdownBlock.Table -> MarkdownTable(block, index, openFile)
                }
            }
        }
    }
    file?.let { target -> ConversationFilePreview(target, perform) { file = null } }
}

@Composable
private fun MarkdownCode(block: MarkdownBlock.Paragraph, index: Int) {
    val clipboard = androidx.compose.ui.platform.LocalClipboardManager.current
    var copied by remember { mutableStateOf(false) }
    var wrapped by remember { mutableStateOf(false) }
    val text = block.runs.joinToString("") { it.text }
    androidx.compose.runtime.LaunchedEffect(copied) {
        if (copied) {
            kotlinx.coroutines.delay(COPY_FEEDBACK_MILLIS)
            copied = false
        }
    }
    Column(
        Modifier.fillMaxWidth()
            .clip(androidx.compose.foundation.shape.RoundedCornerShape(12.dp))
            .border(
                1.dp,
                MaterialTheme.colorScheme.outlineVariant,
                androidx.compose.foundation.shape.RoundedCornerShape(12.dp),
            )
            .background(MaterialTheme.colorScheme.surfaceContainer)
    ) {
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                block.style.filename ?: (block.style.language ?: "CODE").uppercase(),
                Modifier.weight(1f),
                style = MaterialTheme.typography.labelMedium,
                maxLines = 1,
                overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
            )
            androidx.compose.material3.TextButton(onClick = { wrapped = !wrapped }) {
                Text(if (wrapped) "折り返し解除" else "折り返し")
            }
            androidx.compose.material3.TextButton(
                onClick = {
                    clipboard.setText(androidx.compose.ui.text.AnnotatedString(text))
                    copied = true
                },
                modifier = Modifier.testTag("markdown.code.copy.$index"),
            ) {
                Text(if (copied) "✓" else "コピー")
            }
        }
        HorizontalDivider()
        Box(
            Modifier.fillMaxWidth()
                .then(if (wrapped) Modifier else Modifier.horizontalScroll(rememberScrollState()))
                .padding(12.dp)
        ) {
            Text(
                markdownText(block.runs),
                style = MaterialTheme.typography.bodyMedium,
                fontFamily = FontFamily.Monospace,
            )
        }
    }
}

@Composable
private fun MarkdownTable(table: MarkdownBlock.Table, index: Int, openFile: (String) -> Unit) {
    val width = 160.dp * LocalDensity.current.fontScale
    Column {
        Box(Modifier.align(Alignment.End)) { TableCopyMenu(table) }
        Column(
            Modifier.horizontalScroll(rememberScrollState())
                .testTag("markdown.table.$index")
                .border(
                    1.dp,
                    MaterialTheme.colorScheme.outlineVariant,
                    androidx.compose.foundation.shape.RoundedCornerShape(8.dp),
                )
        ) {
            table.rows.forEachIndexed { row, cells ->
                Row(
                    Modifier.background(
                        if (row == 0) MaterialTheme.colorScheme.surfaceVariant
                        else Color.Transparent
                    )
                ) {
                    cells.forEachIndexed { column, cell ->
                        Box(
                            Modifier.width(width)
                                .padding(10.dp)
                                .testTag("markdown.cell.$index.$row.$column")
                                .semantics(mergeDescendants = true) {}
                        ) {
                            MarkdownText(
                                cell.runs,
                                MaterialTheme.typography.bodyLarge,
                                null,
                                openFile,
                                when (table.columns[column]) {
                                    MarkdownAlignment.LEFT -> TextAlign.Left
                                    MarkdownAlignment.CENTER -> TextAlign.Center
                                    MarkdownAlignment.RIGHT -> TextAlign.Right
                                },
                            )
                        }
                    }
                }
                HorizontalDivider(Modifier.width(width * table.columns.size))
            }
        }
    }
}

@Composable
private fun TableCopyMenu(table: MarkdownBlock.Table) {
    val clipboard = androidx.compose.ui.platform.LocalClipboardManager.current
    var menu by remember { mutableStateOf(false) }
    Box {
        androidx.compose.material3.TextButton(onClick = { menu = true }) { Text("表をコピー") }
        androidx.compose.material3.DropdownMenu(menu, onDismissRequest = { menu = false }) {
            androidx.compose.material3.DropdownMenuItem(
                text = { Text("Markdownをコピー") },
                onClick = {
                    clipboard.setText(androidx.compose.ui.text.AnnotatedString(table.source))
                    menu = false
                },
            )
            androidx.compose.material3.DropdownMenuItem(
                text = { Text("CSVをコピー") },
                onClick = {
                    clipboard.setText(
                        androidx.compose.ui.text.AnnotatedString(
                            dev.remoteagent.core.markdownTableCsv(
                                table.rows.map { row ->
                                    row.map { cell -> cell.runs.joinToString("") { it.text } }
                                }
                            )
                        )
                    )
                    menu = false
                },
            )
        }
    }
}

@Composable
private fun MarkdownText(
    runs: List<MarkdownRun>,
    style: androidx.compose.ui.text.TextStyle,
    marker: String?,
    openFile: (String) -> Unit,
    alignment: TextAlign = TextAlign.Left,
) {
    val chips =
        runs
            .mapIndexedNotNull { index, run -> run.file?.let { index.toString() to it } }
            .toMap()
    val linkColor = MaterialTheme.colorScheme.primary
    val styledRuns = runs.map { markdownText(listOf(it)) }
    val text = buildAnnotatedString {
        if (marker != null) append("$marker ")
        runs.forEachIndexed { index, run ->
            val link = run.link
            if (run.file != null && link != null) {
                withLink(LinkAnnotation.Clickable(index.toString()) { openFile(link) }) {
                    appendInlineContent(index.toString(), "[${run.file!!.label}](<$link>)")
                }
            } else if (link != null) {
                withStyle(
                    SpanStyle(color = linkColor, textDecoration = TextDecoration.Underline)
                ) {
                    withLink(LinkAnnotation.Url(link)) { append(styledRuns[index]) }
                }
            } else append(styledRuns[index])
        }
    }
    if (chips.isEmpty()) {
        Text(text, style = style, textAlign = alignment)
    } else {
        BoxWithConstraints {
            val density = LocalDensity.current
            val fontWidth = style.fontSize.value.takeIf { it.isFinite() && it > 0 } ?: CHIP_DEFAULT_FONT_SP
            val maxChipEm = maxWidth.value / (fontWidth * density.fontScale)
            val inline = chips.mapValues { (_, file) ->
                androidx.compose.foundation.text.InlineTextContent(
                    androidx.compose.ui.text.Placeholder(
                        minOf(
                                file.label
                                    .codePointCount(0, file.label.length)
                                    .coerceAtMost(CHIP_MAX_CHARACTERS) * CHIP_CHARACTER_WIDTH_EM +
                                    CHIP_PADDING_EM,
                                maxChipEm,
                            )
                            .em,
                        CHIP_HEIGHT_EM.em,
                        androidx.compose.ui.text.PlaceholderVerticalAlign.TextCenter,
                    )
                ) {
                    FileChip(file.label, file.kind, style.fontSize)
                }
            }
            Text(text, style = style, textAlign = alignment, inlineContent = inline)
        }
    }
}

@Composable
private fun markdownText(runs: List<MarkdownRun>): androidx.compose.ui.text.AnnotatedString {
    val dark = MaterialTheme.colorScheme.surface.luminance() < DARK_SURFACE_LUMINANCE
    val inlineBackground = MaterialTheme.colorScheme.surfaceVariant
    return buildAnnotatedString {
        runs.forEach { run ->
            withStyle(
                SpanStyle(
                    color =
                        (if (dark) run.darkColor else run.lightColor)?.let {
                            Color(OPAQUE_ALPHA_MASK or it.toLong())
                        } ?: Color.Unspecified,
                    fontWeight = if (run.strong) FontWeight.Bold else null,
                    fontStyle = if (run.emphasis) FontStyle.Italic else null,
                    fontFamily = if (run.code) FontFamily.Monospace else null,
                    background = if (run.code) inlineBackground else Color.Unspecified,
                    textDecoration =
                        if (run.strikethrough) TextDecoration.LineThrough else TextDecoration.None,
                )
            ) {
                append(run.text)
            }
        }
    }
}

@Composable
private fun FileChip(
    label: String,
    kind: dev.remoteagent.core.MarkdownFileKind,
    fontSize: androidx.compose.ui.unit.TextUnit,
) {
    Row(
        Modifier.fillMaxSize()
            .clip(androidx.compose.foundation.shape.RoundedCornerShape(6.dp))
            .border(
                1.dp,
                Color(FILE_CHIP_COLOR).copy(alpha = CHIP_BORDER_ALPHA),
                androidx.compose.foundation.shape.RoundedCornerShape(6.dp),
            )
            .background(Color(FILE_CHIP_COLOR).copy(alpha = 0.1f))
            .padding(horizontal = 5.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Text(
            when (kind) {
                dev.remoteagent.core.MarkdownFileKind.MARKDOWN -> "M↓"
                dev.remoteagent.core.MarkdownFileKind.CODE -> "‹›"
                dev.remoteagent.core.MarkdownFileKind.FOLDER -> "▱"
                else -> "▤"
            },
            color = Color(FILE_CHIP_COLOR),
            style = MaterialTheme.typography.labelSmall,
        )
        Text(
            label,
            color = Color(FILE_CHIP_COLOR),
            fontSize = fontSize * CHIP_FONT_SCALE,
            fontWeight = FontWeight.Medium,
            maxLines = 1,
            overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
        )
    }
}

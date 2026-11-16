package dev.remoteagent.mobile

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
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
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.graphics.toArgb
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
import dev.remoteagent.core.markdownCalloutMarker

private const val RGB_MASK = 0xFFFFFFu
private const val OPAQUE_ALPHA_MASK = 0xFF000000L
private const val DARK_SURFACE_LUMINANCE = 0.5f
private const val LIST_INDENT_DP = 22

@Composable
internal fun ConversationBlocks(
    blocks: List<MarkdownBlock>,
    cwd: String,
    perform: ((Intent, (Result<Outcome>) -> Unit) -> Unit)?,
) {
    var file by remember { mutableStateOf<String?>(null) }
    var shareError by remember { mutableStateOf<String?>(null) }
    val shareFile = rememberFileSharer(perform) { shareError = it }
    val clipboard = androidx.compose.ui.platform.LocalClipboardManager.current
    val openFile: (String, dev.remoteagent.core.MobileFileMenuItem?) -> Unit = { source, item ->
        when (item?.action) {
            dev.remoteagent.core.MobileFileAction.COPY_FULL_PATH,
            dev.remoteagent.core.MobileFileAction.COPY_RELATIVE_PATH ->
                clipboard.setText(androidx.compose.ui.text.AnnotatedString(item.path))
            dev.remoteagent.core.MobileFileAction.SAVE_OR_SHARE -> item.mimeType?.let { shareFile(item.path, it) }
            else -> file = source
        }
    }
    SelectionContainer {
        Column(verticalArrangement = Arrangement.spacedBy(14.dp)) { MarkdownBlocks(blocks, cwd, perform, openFile) }
    }
    shareError?.let {
        Text(
            dev.remoteagent.core.localizedErrorMessage(LocalUiLanguage.current, it),
            color = MaterialTheme.colorScheme.error,
        )
    }
    file?.let { source ->
        dev.remoteagent.core.markdownFileTarget(source, cwd, dev.remoteagent.core.MarkdownFileSurface.MOBILE)?.let {
            target ->
            ConversationFileViewer(target, perform, source = source, workspaceRoot = cwd) { file = null }
        }
    }
}

@Composable
private fun MarkdownBlocks(
    blocks: List<MarkdownBlock>,
    cwd: String,
    perform: ((Intent, (Result<Outcome>) -> Unit) -> Unit)?,
    openFile: (String, dev.remoteagent.core.MobileFileMenuItem?) -> Unit,
) {
    blocks.forEachIndexed { index, block ->
        when (block) {
            is MarkdownBlock.Paragraph ->
                when {
                    block.style.rule -> HorizontalDivider(Modifier.padding(vertical = 8.dp))
                    block.style.code -> MarkdownCode(block, index)
                    else -> MarkdownParagraph(block, cwd, openFile)
                }
            is MarkdownBlock.Visualization -> ConversationVisualization(block.path, cwd, perform)
            is MarkdownBlock.Table -> MarkdownTable(block, index, openFile, cwd)
            is MarkdownBlock.Disclosure -> {
                MarkdownParagraph(MarkdownBlock.Paragraph(block.summary, block.style), cwd, openFile)
                MarkdownBlocks(block.blocks, cwd, perform, openFile)
            }
            is MarkdownBlock.Callout -> {
                val marker =
                    MarkdownRun(
                        text = markdownCalloutMarker(block.kind),
                        template = null,
                        strong = false,
                        emphasis = false,
                        strikethrough = false,
                        code = false,
                        math = false,
                        link = null,
                        image = null,
                        file = null,
                        darkColor = null,
                        lightColor = null,
                    )
                MarkdownParagraph(MarkdownBlock.Paragraph(listOf(marker), block.style), cwd, openFile)
                MarkdownBlocks(block.blocks, cwd, perform, openFile)
            }
        }
    }
}

@Composable
private fun MarkdownParagraph(
    block: MarkdownBlock.Paragraph,
    cwd: String,
    openFile: (String, dev.remoteagent.core.MobileFileMenuItem?) -> Unit,
) {
    val style =
        when (block.style.header?.toInt()) {
            1 -> MaterialTheme.typography.headlineSmall
            2 -> MaterialTheme.typography.titleLarge
            null -> MaterialTheme.typography.bodyLarge
            else -> MaterialTheme.typography.titleMedium
        }
    val quoteColor = MaterialTheme.colorScheme.outline
    Box(
        Modifier.padding(start = (block.style.listDepth.toInt() - 1).coerceAtLeast(0).times(LIST_INDENT_DP).dp)
            .then(
                if (block.style.quoted)
                    Modifier.drawBehind {
                            drawLine(quoteColor, Offset(1.dp.toPx(), 0f), Offset(1.dp.toPx(), size.height), 2.dp.toPx())
                        }
                        .padding(start = 12.dp)
                else Modifier
            )
    ) {
        MarkdownText(block.runs, style, block.style.marker, openFile, cwd)
    }
}

@Composable
private fun MarkdownTable(
    table: MarkdownBlock.Table,
    index: Int,
    openFile: (String, dev.remoteagent.core.MobileFileMenuItem?) -> Unit,
    cwd: String,
) {
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
                    Modifier.background(if (row == 0) MaterialTheme.colorScheme.surfaceVariant else Color.Transparent)
                ) {
                    cells.forEachIndexed { column, cell ->
                        Box(
                            Modifier.width(width).padding(10.dp).testTag("markdown.cell.$index.$row.$column").semantics(
                                mergeDescendants = true
                            ) {}
                        ) {
                            MarkdownText(
                                cell.runs,
                                MaterialTheme.typography.bodyLarge.copy(
                                    textAlign =
                                        when (table.columns[column]) {
                                            MarkdownAlignment.LEFT -> TextAlign.Left
                                            MarkdownAlignment.CENTER -> TextAlign.Center
                                            MarkdownAlignment.RIGHT -> TextAlign.Right
                                        }
                                ),
                                null,
                                openFile,
                                cwd,
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
    val language = LocalUiLanguage.current
    val clipboard = androidx.compose.ui.platform.LocalClipboardManager.current
    var menu by remember { mutableStateOf(false) }
    Box {
        androidx.compose.material3.TextButton(onClick = { menu = true }) { Text(language.text("表をコピー")) }
        androidx.compose.material3.DropdownMenu(menu, onDismissRequest = { menu = false }) {
            androidx.compose.material3.DropdownMenuItem(
                text = { Text(language.text("Markdownをコピー")) },
                onClick = {
                    clipboard.setText(androidx.compose.ui.text.AnnotatedString(table.source))
                    menu = false
                },
            )
            androidx.compose.material3.DropdownMenuItem(
                text = { Text(language.text("CSVをコピー")) },
                onClick = {
                    clipboard.setText(
                        androidx.compose.ui.text.AnnotatedString(
                            dev.remoteagent.core.markdownTableCsv(
                                table.rows.map { row -> row.map { cell -> cell.runs.joinToString("") { it.text } } }
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
    openFile: (String, dev.remoteagent.core.MobileFileMenuItem?) -> Unit,
    cwd: String,
) {
    val chips = runs.mapIndexedNotNull { index, run -> run.file?.let { index.toString() to it } }.toMap()
    val linkColor = MaterialTheme.colorScheme.primary
    val openExternalLink = rememberExternalLinkOpener()
    val styledRuns = runs.map { markdownText(listOf(it)) }
    val text = buildAnnotatedString {
        if (marker != null) append("$marker ")
        runs.forEachIndexed { index, run ->
            val link = run.link
            if (run.file != null && link != null) {
                withLink(LinkAnnotation.Clickable(index.toString()) { openFile(link, null) }) {
                    appendInlineContent(index.toString(), "[${run.file!!.label}](<$link>)")
                }
            } else if (link != null) {
                withStyle(SpanStyle(color = linkColor, textDecoration = TextDecoration.Underline)) {
                    withLink(LinkAnnotation.Url(link) { openExternalLink(link) }) { append(styledRuns[index]) }
                }
            } else append(styledRuns[index])
        }
    }
    if (chips.isEmpty()) {
        Text(text, style = style, textAlign = style.textAlign)
    } else {
        BoxWithConstraints {
            val home = LocalFileLocation.current.home
            val language = LocalUiLanguage.current
            val inline = chips.mapValues { (index, file) ->
                val link = runs[index.toInt()].link!!
                val menu = dev.remoteagent.core.mobileFileMenu(language, link, cwd, home)
                fileChipContent(file, style, maxWidth, menu) { item -> openFile(link, item) }
            }
            Text(text, style = style, textAlign = style.textAlign, inlineContent = inline)
        }
    }
}

@Composable
internal fun markdownText(runs: List<MarkdownRun>): androidx.compose.ui.text.AnnotatedString {
    val dark = MaterialTheme.colorScheme.surface.luminance() < DARK_SURFACE_LUMINANCE
    val inlineBackground = MaterialTheme.colorScheme.surfaceVariant
    return buildAnnotatedString {
        runs.forEach { run ->
            withStyle(
                SpanStyle(
                    color =
                        (if (dark) run.darkColor else run.lightColor)?.let { Color(OPAQUE_ALPHA_MASK or it.toLong()) }
                            ?: Color.Unspecified,
                    fontWeight = if (run.strong) FontWeight.Bold else null,
                    fontStyle = if (run.emphasis) FontStyle.Italic else null,
                    fontFamily = if (run.code) FontFamily.Monospace else null,
                    background = if (run.code) inlineBackground else Color.Unspecified,
                    textDecoration = if (run.strikethrough) TextDecoration.LineThrough else TextDecoration.None,
                )
            ) {
                append(run.text)
            }
        }
    }
}

@Composable
private fun fileChipContent(
    file: dev.remoteagent.core.MarkdownFileReference,
    style: androidx.compose.ui.text.TextStyle,
    maxWidth: androidx.compose.ui.unit.Dp,
    menu: List<dev.remoteagent.core.MobileFileMenuItem>,
    action: (dev.remoteagent.core.MobileFileMenuItem?) -> Unit,
): androidx.compose.foundation.text.InlineTextContent {
    val density = LocalDensity.current
    val measure = androidx.compose.ui.text.rememberTextMeasurer()
    val label = measure.measure(file.label, style.copy(fontWeight = FontWeight.Medium), maxLines = 1)
    val fontPixels = with(density) { style.fontSize.toPx() }
    val width = with(density) { minOf(label.size.width + fontPixels + 14.dp.toPx(), maxWidth.toPx()) }
    val height = label.size.height + with(density) { 4.dp.toPx() }
    return androidx.compose.foundation.text.InlineTextContent(
        androidx.compose.ui.text.Placeholder(
            (width / fontPixels).em,
            (height / fontPixels).em,
            androidx.compose.ui.text.PlaceholderVerticalAlign.TextCenter,
        )
    ) {
        FileChip(file, style.fontSize, menu, action)
    }
}

@Composable
private fun FileChip(
    file: dev.remoteagent.core.MarkdownFileReference,
    fontSize: androidx.compose.ui.unit.TextUnit,
    menu: List<dev.remoteagent.core.MobileFileMenuItem>,
    action: (dev.remoteagent.core.MobileFileMenuItem?) -> Unit,
) {
    val surfaces = listOf(MaterialTheme.colorScheme.surface, MaterialTheme.colorScheme.surfaceContainer)
    val color = dev.remoteagent.core.mobileFileChipColor(surfaces.map { it.toArgb().toUInt() and RGB_MASK })
    val context = androidx.compose.ui.platform.LocalContext.current
    val density = LocalDensity.current
    val fontPixels = with(density) { fontSize.toPx() }
    val token = dev.remoteagent.core.mobileFileIcon(file.path)
    val bitmap =
        remember(token) {
            context.assets.open("pierre_$token.png").use { android.graphics.BitmapFactory.decodeStream(it) }
        }
    androidx.compose.ui.viewinterop.AndroidView(
        modifier = Modifier.fillMaxWidth().testTag("markdown.file.${file.path}"),
        factory = {
            android.widget.TextView(it).apply {
                maxLines = 1
                includeFontPadding = false
                gravity = android.view.Gravity.CENTER_VERTICAL
                ellipsize = android.text.TextUtils.TruncateAt.END
                typeface = android.graphics.Typeface.create("sans-serif-medium", android.graphics.Typeface.NORMAL)
                setPadding(
                    with(density) { 5.dp.roundToPx() },
                    with(density) { 2.dp.roundToPx() },
                    with(density) { 5.dp.roundToPx() },
                    with(density) { 2.dp.roundToPx() },
                )
                compoundDrawablePadding = with(density) { 4.dp.roundToPx() }
            }
        },
        update = { view ->
            view.text = file.label
            view.setTextSize(android.util.TypedValue.COMPLEX_UNIT_PX, fontPixels)
            view.setTextColor((OPAQUE_ALPHA_MASK or color.toLong()).toInt())
            val icon = android.graphics.drawable.BitmapDrawable(context.resources, bitmap)
            icon.setBounds(0, 0, fontPixels.toInt(), fontPixels.toInt())
            view.setCompoundDrawables(icon, null, null, null)
            view.setOnClickListener { action(null) }
            view.setOnCreateContextMenuListener { native, _, _ ->
                native.setHeaderTitle(menu.firstOrNull()?.path ?: file.path)
                menu.forEachIndexed { index, item ->
                    native.add(0, index, index, item.label).setOnMenuItemClickListener {
                        action(item)
                        true
                    }
                }
            }
            view.setOnLongClickListener { if (menu.isEmpty()) false else view.showContextMenu() }
        },
    )
}

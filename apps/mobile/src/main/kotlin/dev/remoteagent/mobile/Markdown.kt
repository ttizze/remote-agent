// Declarative native layout with the fixed mobile metrics; the conversation decisions are supplied by core.
@file:Suppress("TooManyFunctions", "LongMethod", "CyclomaticComplexMethod", "LongParameterList", "MagicNumber")

package dev.remoteagent.mobile

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.util.Base64
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.Message
import androidx.compose.material.icons.automirrored.outlined.OpenInNew
import androidx.compose.material.icons.outlined.AutoAwesome
import androidx.compose.material.icons.outlined.BarChart
import androidx.compose.material.icons.outlined.Description
import androidx.compose.material.icons.outlined.PhotoCamera
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withLink
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import dev.remoteagent.core.ArtifactTemplate
import dev.remoteagent.core.ArtifactTemplateSymbol
import dev.remoteagent.core.ContextChip
import dev.remoteagent.core.MarkdownAlignment
import dev.remoteagent.core.MarkdownBlock
import dev.remoteagent.core.MarkdownImageSource
import dev.remoteagent.core.MarkdownRun
import dev.remoteagent.core.artifactTemplatePresentationLabel
import dev.remoteagent.core.artifactTemplateSymbol
import dev.remoteagent.core.markdownBlocks
import dev.remoteagent.core.markdownImageDisplaySize
import dev.remoteagent.core.markdownImageSource
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * Core markdown blocks at the mobile sizes: body 16/23, headings 21/19/17/15, code 13/19. `onUseArtifactTemplate` is
 * set where a card's "Use template" can reach the composer; plans and reasoning show cards without it.
 */
@Composable
internal fun MarkdownText(
    source: String,
    modifier: Modifier = Modifier,
    color: Color = AppTheme.colors.foreground,
    onUseArtifactTemplate: ((ArtifactTemplate) -> Unit)? = null,
    contextChips: List<ContextChip> = emptyList(),
    onOpenContext: (ContextChip) -> Unit = {},
) {
    val actions = LocalMarkdownActions.current
    val blocks by
        produceState(emptyList<MarkdownBlock>(), source) {
            value = withContext(Dispatchers.Default) { markdownBlocks(source) }
        }
    Column(modifier, verticalArrangement = Arrangement.spacedBy(10.dp)) {
        blocks.forEachIndexed { index, block ->
            when (block) {
                is MarkdownBlock.Paragraph ->
                    if (block.style.code) CodeBlock(block.runs.joinToString("") { it.text })
                    else Paragraph(block, color, contextChips, onOpenContext, actions.open)
                is MarkdownBlock.Visualization ->
                    Text(block.path, style = AppTheme.caption, color = AppTheme.colors.foregroundMuted)
                is MarkdownBlock.Table -> MarkdownTable(block, index, contextChips, onOpenContext)
                is MarkdownBlock.ArtifactTemplate -> ArtifactTemplateCard(block.template, onUseArtifactTemplate)
            }
        }
    }
}

private val ArtifactBadge = Color(0xFFD946EF)

/**
 * A `::artifact-template` card: the template's icon with a sparkle badge, its name and kind, and "Use template" when
 * the card can reach the composer.
 */
@Composable
private fun ArtifactTemplateCard(template: ArtifactTemplate, onUse: ((ArtifactTemplate) -> Unit)?) {
    val colors = AppTheme.colors
    val icon =
        when (artifactTemplateSymbol(template.artifactKind)) {
            ArtifactTemplateSymbol.DOCUMENT -> Icons.Outlined.Description
            ArtifactTemplateSymbol.CHART -> Icons.Outlined.BarChart
            ArtifactTemplateSymbol.BROWSER -> Icons.AutoMirrored.Outlined.OpenInNew
            ArtifactTemplateSymbol.CAMERA -> Icons.Outlined.PhotoCamera
            ArtifactTemplateSymbol.MESSAGE -> Icons.AutoMirrored.Outlined.Message
        }
    Surface(
        Modifier.fillMaxWidth().padding(vertical = 8.dp),
        color = colors.card,
        shape = RoundedCornerShape(16.dp),
        border = BorderStroke(1.dp, colors.border),
    ) {
        Row(
            Modifier.padding(12.dp),
            horizontalArrangement = Arrangement.spacedBy(12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Box(Modifier.size(40.dp)) {
                Surface(
                    Modifier.size(40.dp),
                    color = colors.subtle,
                    shape = RoundedCornerShape(12.dp),
                    border = BorderStroke(1.dp, colors.border),
                ) {
                    Box(contentAlignment = Alignment.Center) {
                        Icon(icon, null, Modifier.size(20.dp), tint = colors.foregroundMuted)
                    }
                }
                Box(
                    Modifier.align(Alignment.BottomEnd)
                        .offset(4.dp, 4.dp)
                        .size(16.dp)
                        .background(ArtifactBadge, CircleShape),
                    contentAlignment = Alignment.Center,
                ) {
                    Icon(Icons.Outlined.AutoAwesome, null, Modifier.size(9.dp), tint = Color.White)
                }
            }
            Column(Modifier.weight(1f)) {
                Text(
                    template.displayName,
                    style = AppTheme.footnote,
                    fontWeight = FontWeight.Bold,
                    color = colors.foreground,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                Text(
                    artifactTemplatePresentationLabel(template.artifactKind),
                    style = AppTheme.caption,
                    color = colors.foregroundMuted,
                )
            }
            if (onUse != null)
                Surface(
                    onClick = { onUse(template) },
                    color = colors.subtle,
                    shape = RoundedCornerShape(8.dp),
                    border = BorderStroke(1.dp, colors.border),
                ) {
                    Box(
                        Modifier.heightIn(min = 36.dp).padding(horizontal = 12.dp),
                        contentAlignment = Alignment.Center,
                    ) {
                        Text(
                            "Use template",
                            Modifier.semantics { contentDescription = "Use ${template.displayName} template" },
                            style = AppTheme.caption,
                            fontWeight = FontWeight.Bold,
                            color = colors.foreground,
                        )
                    }
                }
        }
    }
}

@Composable
private fun Paragraph(
    block: MarkdownBlock.Paragraph,
    color: Color,
    contextChips: List<ContextChip>,
    onOpenContext: (ContextChip) -> Unit,
    open: (String) -> Unit,
) {
    val size = AppTheme.markdownSize(block.style.header?.toInt())
    val quoted = block.style.quoted
    val textRuns = block.runs.filter { it.image == null }
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Row {
            if (quoted)
                Box(Modifier.width(3.dp).padding(vertical = 2.dp).background(AppTheme.colors.mdBlockquoteBorder))
            SelectionContainer {
                Text(
                    markdownText(
                        textRuns,
                        AppTheme.colors.mdLink,
                        open = { href ->
                            val chip = markdownContextChip(href, contextChips)
                            if (chip != null) onOpenContext(chip) else open(href)
                        },
                        header = block.style.header != null,
                        marker = block.style.marker,
                    ),
                    Modifier.padding(start = if (quoted) 10.dp else 0.dp),
                    style = AppTheme.body.copy(fontSize = size.sp, lineHeight = AppTheme.markdownBodyLineHeight.sp),
                    color = color,
                )
            }
        }
        block.runs.filter { it.image != null }.forEach { run -> MarkdownImage(run.image!!, run.text) }
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
            if (AppTheme.codeWordWrap)
                Text(
                    code,
                    Modifier.fillMaxWidth().padding(12.dp),
                    fontFamily = AppTheme.mono,
                    fontSize = AppTheme.markdownCodeFontSize.sp,
                    lineHeight = AppTheme.markdownCodeLineHeight.sp,
                    softWrap = true,
                    color = AppTheme.colors.foreground,
                )
            else
                Text(
                    code,
                    Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(12.dp),
                    fontFamily = AppTheme.mono,
                    fontSize = AppTheme.markdownCodeFontSize.sp,
                    lineHeight = AppTheme.markdownCodeLineHeight.sp,
                    softWrap = false,
                    color = AppTheme.colors.foreground,
                )
        }
    }
}

@Composable
private fun MarkdownTable(
    table: MarkdownBlock.Table,
    index: Int,
    contextChips: List<ContextChip>,
    onOpenContext: (ContextChip) -> Unit,
) {
    val width = 220.dp * LocalDensity.current.fontScale
    val actions = LocalMarkdownActions.current
    BoxWithConstraints(Modifier.fillMaxWidth()) {
        val wrapping = AppTheme.codeWordWrap
        val cellWidth =
            if (wrapping) maxWidth / table.columns.size.coerceAtLeast(1).toFloat()
            else width
        val tableWidth = if (wrapping) maxWidth else width * table.columns.size
        SelectionContainer {
            Column(
                Modifier.then(if (wrapping) Modifier else Modifier.horizontalScroll(rememberScrollState()))
                    .testTag("markdown.table.$index")
            ) {
                table.rows.forEachIndexed { row, cells ->
                    Row(Modifier.background(if (row == 0) AppTheme.colors.subtleStrong else Color.Transparent)) {
                        cells.forEachIndexed { column, cell ->
                            Text(
                                markdownText(
                                    cell.runs,
                                    AppTheme.colors.mdLink,
                                    open = { href ->
                                        val chip = markdownContextChip(href, contextChips)
                                        if (chip != null) onOpenContext(chip) else actions.open(href)
                                    },
                                ),
                                Modifier.width(cellWidth).padding(10.dp)
                                    .testTag("markdown.cell.$index.$row.$column"),
                                style = AppTheme.caption,
                                color = AppTheme.colors.foreground,
                                softWrap = wrapping,
                                textAlign =
                                    when (table.columns.getOrNull(column)) {
                                        MarkdownAlignment.CENTER -> TextAlign.Center
                                        MarkdownAlignment.RIGHT -> TextAlign.Right
                                        else -> TextAlign.Left
                                    },
                            )
                        }
                    }
                    HorizontalDivider(Modifier.width(tableWidth), color = AppTheme.colors.mdHr)
                }
            }
        }
    }
}

internal fun markdownContextChip(href: String, contextChips: List<ContextChip>): ContextChip? {
    val id = markdownContextId(href)
    return contextChips.firstOrNull { it.contextId == id }
}

internal fun markdownContextId(href: String): String = href.substringAfterLast('/')

private fun markdownText(
    runs: List<MarkdownRun>,
    link: Color,
    open: (String) -> Unit,
    header: Boolean = false,
    marker: String? = null,
) = buildAnnotatedString {
    if (marker != null) append("$marker ")
    runs.forEach { run ->
        withStyle(
            SpanStyle(
                fontWeight = if (header || run.strong) FontWeight.Bold else null,
                fontStyle = if (run.emphasis) FontStyle.Italic else null,
                fontFamily = if (run.code) AppTheme.mono else null,
                fontSize = if (run.code) AppTheme.markdownCodeFontSize.sp else androidx.compose.ui.unit.TextUnit.Unspecified,
                textDecoration = if (run.strikethrough) TextDecoration.LineThrough else TextDecoration.None,
            )
        ) {
            val url = run.link
            if (url != null)
                withLink(LinkAnnotation.Clickable(url, TextLinkStyles(SpanStyle(color = link))) { open(url) }) {
                    append(run.text)
                }
            else append(run.text)
        }
    }
}

/** Inline response images use core's source and size decisions. */
@Composable
private fun MarkdownImage(href: String, alt: String) {
    val actions = LocalMarkdownActions.current
    val source = markdownImageSource(href, actions.workspaceRoot)
    var preview by remember(href) { mutableStateOf(false) }
    val loaded by
        produceState<Result<Bitmap>?>(null, source) {
            value = null
            value =
                withContext(Dispatchers.IO) {
                    runCatching {
                        val bytes =
                            when (source) {
                                is MarkdownImageSource.WorkspaceFile -> actions.loadFile(source.path)
                                MarkdownImageSource.Blocked -> error("Image unavailable")
                                is MarkdownImageSource.Direct -> {
                                    val uri = source.uri
                                    if (uri.startsWith("data:")) {
                                        val payload = uri.substringAfter(',')
                                        if (uri.substringBefore(',').endsWith(";base64"))
                                            Base64.decode(payload, Base64.DEFAULT)
                                        else android.net.Uri.decode(payload).toByteArray()
                                    } else {
                                        val connection =
                                            java.net
                                                .URI(if (uri.startsWith("//")) "https:$uri" else uri)
                                                .toURL()
                                                .openConnection()
                                        connection.connectTimeout = 10000
                                        connection.readTimeout = 10000
                                        connection.getInputStream().use { it.readBytes() }
                                    }
                                }
                            }
                        BitmapFactory.decodeByteArray(bytes, 0, bytes.size) ?: error("Image unavailable")
                    }
                }
        }
    val image = loaded?.getOrNull()
    if (image == null)
        Text(
            if (loaded == null) "Loading image…" else "Image unavailable",
            style = AppTheme.caption,
            color = AppTheme.colors.foregroundMuted,
        )
    else {
        BoxWithConstraints(Modifier.fillMaxWidth()) {
            val size =
                markdownImageDisplaySize(image.width.toDouble(), image.height.toDouble(), maxWidth.value.toDouble())
            if (size != null)
                Image(
                    image.asImageBitmap(),
                    alt,
                    Modifier.width(size.width.dp).height(size.height.dp).clickable { preview = true },
                    contentScale = ContentScale.Fit,
                )
        }
        if (preview)
            Dialog(onDismissRequest = { preview = false }) {
                Image(
                    image.asImageBitmap(),
                    alt,
                    Modifier.fillMaxWidth().clickable { preview = false },
                    contentScale = ContentScale.Fit,
                )
            }
    }
}

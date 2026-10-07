// Declarative native layout with the fixed mobile metrics; the sections and drops are supplied by core.
@file:Suppress("LongMethod", "MagicNumber", "CyclomaticComplexMethod")

package dev.remoteagent.mobile

import androidx.activity.compose.BackHandler
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.gestures.scrollBy
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.DragHandle
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.customActions
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.ArrangementDrop
import dev.remoteagent.core.ArrangementOptions
import dev.remoteagent.core.ArrangementRow
import dev.remoteagent.core.ArrangementRowKind
import dev.remoteagent.core.DragSection
import dev.remoteagent.core.DropSection
import dev.remoteagent.core.Intent
import dev.remoteagent.core.MoveDestination
import dev.remoteagent.core.OrderSection
import dev.remoteagent.core.Placement
import dev.remoteagent.core.dragGapOffset
import kotlinx.coroutines.delay

private const val ROW_HEIGHT = 56
private const val HEADER_HEIGHT = 48
private const val EDGE = 48f
private const val FRAME_MILLIS = 16L

/** A drag in progress: the lifted thread, where it started on screen, and where it would land. */
private data class ArrangementDrag(
    val threadId: String,
    val title: String,
    val startY: Float,
    val translation: Float,
    val drop: ArrangementDrop?,
)

private fun ArrangementRow.height() = if (kind is ArrangementRowKind.Header) HEADER_HEIGHT else ROW_HEIGHT

/** "Arrange threads": drag a handle to reorder, pin, unpin, unsettle or settle; changes save on drop. */
@Composable
internal fun ArrangeSheet(model: AndroidAppModel, onDismiss: () -> Unit) {
    val colors = AppTheme.colors
    val density = LocalDensity.current
    val now = rememberNow()
    var options by remember { mutableStateOf(ArrangementOptions(snoozedExpanded = false, settledExpanded = false)) }
    val view by rememberView(model.snapshot, now / MINUTE_MILLIS, options) { it.threadArrangement(now, options) }
    val rows = view?.rows.orEmpty()
    val px = { dp: Int -> with(density) { dp.dp.toPx() } }
    // Pixel offsets of the fixed-height rows; hit testing keeps this layout while rows shift.
    val offsets = remember(rows, density) { rows.runningFold(0f) { offset, row -> offset + px(row.height()) } }
    val list = rememberLazyListState()
    var viewport by remember { mutableFloatStateOf(0f) }
    var drag by remember { mutableStateOf<ArrangementDrag?>(null) }
    val scrolled = { offsets.getOrElse(list.firstVisibleItemIndex) { 0f } + list.firstVisibleItemScrollOffset }
    BackHandler(onBack = onDismiss)

    fun update(translation: Float) {
        val current = drag ?: return
        val y = current.startY + translation
        val content = y.coerceAtLeast(0f) + scrolled()
        val index = offsets.dropLast(1).indexOfLast { it <= content }.coerceAtLeast(0)
        val drop =
            rows
                .getOrNull(index)
                ?.takeIf { y in 0f..viewport }
                ?.let { row ->
                    val after =
                        row.kind is ArrangementRowKind.Thread && content >= offsets[index] + px(row.height()) / 2
                    model.snapshot.threadArrangementDrop(now, options, current.threadId, row.key, after)
                }
        drag = current.copy(translation = translation, drop = drop)
    }

    // Scrolls while the lifted row hovers near an edge.
    LaunchedEffect(drag != null) {
        while (drag != null) {
            val current = drag ?: break
            val y = current.startY + current.translation
            val speed =
                when {
                    y < EDGE -> -minOf(1f, (EDGE - y) / EDGE)
                    y > viewport - EDGE -> minOf(1f, (y - viewport + EDGE) / EDGE)
                    else -> 0f
                }
            if (speed != 0f && list.scrollBy(speed * FRAME_MILLIS * px(1) / 2) != 0f) update(current.translation)
            delay(FRAME_MILLIS)
        }
    }

    val current = drag
    val sourceIndex = current?.let { lifted -> rows.indexOfFirst { it.key == lifted.threadId } } ?: -1
    val insertion =
        current?.drop?.let { drop ->
            val destination = drop.destination as? MoveDestination.Drop
            val section =
                when (destination?.section) {
                    DropSection.PINNED -> DragSection.PINNED
                    DropSection.ACTIVE -> DragSection.ACTIVE
                    DropSection.SETTLED -> DragSection.SETTLED
                    null -> null
                }
            val key = destination?.target ?: section?.let { "section:${it.name.lowercase()}" }
            val index = rows.indexOfFirst { it.section == section && it.key == key }
            rows.getOrNull(index)?.let { row ->
                offsets[index] +
                    if (row.kind is ArrangementRowKind.Header || destination?.placement == Placement.AFTER)
                        px(row.height())
                    else 0f
            }
        } ?: offsets.getOrNull(sourceIndex)

    Surface(Modifier.fillMaxSize(), color = colors.screen) {
        Column(Modifier.fillMaxSize().safeDrawingPadding()) {
            Row(
                Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 12.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    "Arrange threads",
                    Modifier.weight(1f),
                    style = AppTheme.title,
                    fontWeight = FontWeight.SemiBold,
                    color = colors.foreground,
                )
                TextButton(onClick = onDismiss, modifier = Modifier.heightIn(min = 44.dp)) {
                    Text("Done", style = AppTheme.body, color = colors.primaryText)
                }
            }
            Text(
                "Drag to reorder, pin, or settle. Changes save when you drop.",
                Modifier.padding(start = 20.dp, end = 20.dp, bottom = 12.dp),
                style = AppTheme.footnote,
                color = colors.foregroundMuted,
            )
            Box(Modifier.weight(1f).fillMaxWidth().onSizeChanged { viewport = it.height.toFloat() }) {
                LazyColumn(state = list, userScrollEnabled = current == null) {
                    itemsIndexed(rows, key = { _, row -> row.key }) { index, row ->
                        val shift =
                            if (sourceIndex >= 0 && insertion != null)
                                dragGapOffset(
                                        offsets[index].toDouble(),
                                        offsets[sourceIndex].toDouble(),
                                        px(rows[sourceIndex].height()).toDouble(),
                                        insertion.toDouble(),
                                    )
                                    .toFloat()
                            else 0f
                        val animated by
                            animateFloatAsState(shift, tween(if (current != null) 160 else 0), label = "gap")
                        Box(
                            Modifier.fillMaxWidth()
                                .height(row.height().dp)
                                .graphicsLayer { translationY = animated }
                                .alpha(if (index == sourceIndex) 0f else 1f)
                        ) {
                            when (val kind = row.kind) {
                                is ArrangementRowKind.Header ->
                                    HeaderRow(kind) {
                                        options =
                                            when (row.section) {
                                                DragSection.SNOOZED ->
                                                    options.copy(snoozedExpanded = !options.snoozedExpanded)
                                                DragSection.SETTLED ->
                                                    options.copy(settledExpanded = !options.settledExpanded)
                                                else -> options
                                            }
                                    }
                                is ArrangementRowKind.Thread ->
                                    ThreadRow(
                                        view?.locked ?: true,
                                        model,
                                        row.section,
                                        kind,
                                        onStart = {
                                            val top = offsets[index] - scrolled()
                                            drag =
                                                ArrangementDrag(
                                                    kind.threadId,
                                                    kind.title,
                                                    top + px(ROW_HEIGHT) / 2,
                                                    0f,
                                                    null,
                                                )
                                            update(0f)
                                        },
                                        onMove = ::update,
                                        onEnd = { cancelled ->
                                            val lifted = drag
                                            drag = null
                                            val drop = lifted?.drop
                                            if (!cancelled && drop != null)
                                                model.perform(
                                                    Intent.MoveThread(lifted.threadId, drop.section, drop.destination)
                                                )
                                        },
                                    )
                            }
                        }
                    }
                }
                current?.let { lifted ->
                    val top =
                        (lifted.startY + lifted.translation - px(ROW_HEIGHT) / 2).coerceIn(
                            0f,
                            (viewport - px(ROW_HEIGHT)).coerceAtLeast(0f),
                        )
                    Surface(
                        Modifier.padding(horizontal = 20.dp).fillMaxWidth().height(ROW_HEIGHT.dp).offset {
                            IntOffset(0, top.toInt())
                        },
                        shape = RoundedCornerShape(12.dp),
                        color = colors.screen,
                        border = BorderStroke(1.dp, colors.border),
                    ) {
                        Column(Modifier.padding(horizontal = 16.dp), verticalArrangement = Arrangement.Center) {
                            Text(
                                lifted.title,
                                style = AppTheme.body,
                                fontWeight = FontWeight.Medium,
                                color = colors.foreground,
                                maxLines = if (lifted.drop?.label != null) 1 else 2,
                            )
                            lifted.drop?.label?.let {
                                Text(it, style = AppTheme.caption, color = colors.foregroundMuted)
                            }
                        }
                    }
                }
            }
        }
    }
    // A drag ends when its thread leaves the list.
    LaunchedEffect(rows) { if (rows.none { it.key == drag?.threadId }) drag = null }
}

@Composable
private fun HeaderRow(header: ArrangementRowKind.Header, onToggle: () -> Unit) {
    Column(Modifier.fillMaxSize()) {
        Box(
            Modifier.weight(1f)
                .fillMaxWidth()
                .then(if (header.foldable) Modifier.clickable(onClick = onToggle) else Modifier)
                .padding(horizontal = 20.dp),
            contentAlignment = Alignment.CenterStart,
        ) {
            Text(
                header.label,
                style = AppTheme.footnote,
                fontWeight = FontWeight.SemiBold,
                color = AppTheme.colors.foregroundMuted,
            )
        }
        HorizontalDivider(color = AppTheme.colors.borderSubtle)
    }
}

// Drag lifecycle callbacks are independent of accessibility moves; keep their inputs explicit.
@Suppress("LongParameterList")
@Composable
private fun ThreadRow(
    locked: Boolean,
    model: AndroidAppModel,
    section: DragSection,
    row: ArrangementRowKind.Thread,
    onStart: () -> Unit,
    onMove: (Float) -> Unit,
    onEnd: (cancelled: Boolean) -> Unit,
) {
    val order = if (section == DragSection.PINNED) OrderSection.PINNED else OrderSection.ACTIVE
    fun move(destination: MoveDestination) = model.perform(Intent.MoveThread(row.threadId, order, destination))
    val latestStart by rememberUpdatedState(onStart)
    val latestMove by rememberUpdatedState(onMove)
    val latestEnd by rememberUpdatedState(onEnd)
    Column(Modifier.fillMaxSize()) {
        Row(Modifier.weight(1f).padding(horizontal = 20.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(
                row.title,
                Modifier.weight(1f),
                style = AppTheme.body,
                color = AppTheme.colors.foreground,
                maxLines = 2,
            )
            Box(
                Modifier.size(48.dp)
                    .alpha(if (!locked) 1f else 0.3f)
                    .semantics {
                        contentDescription = "Reorder ${row.title}"
                        customActions =
                            if (locked) emptyList()
                            else
                                row.sectionMoves.map { action ->
                                    CustomAccessibilityAction(action.label) {
                                        move(MoveDestination.Drop(null, action.section, Placement.BEFORE))
                                        true
                                    }
                                } +
                                    listOfNotNull(
                                        if (row.canMoveUp)
                                            CustomAccessibilityAction("Move up") {
                                                move(MoveDestination.Up)
                                                true
                                            }
                                        else null,
                                        if (row.canMoveDown)
                                            CustomAccessibilityAction("Move down") {
                                                move(MoveDestination.Down)
                                                true
                                            }
                                        else null,
                                    )
                    }
                    .pointerInput(!locked) {
                        if (locked) return@pointerInput
                        var translation = 0f
                        detectDragGestures(
                            onDragStart = {
                                translation = 0f
                                latestStart()
                            },
                            onDragEnd = { latestEnd(false) },
                            onDragCancel = { latestEnd(true) },
                        ) { change, amount ->
                            change.consume()
                            translation += amount.y
                            latestMove(translation)
                        }
                    },
                contentAlignment = Alignment.Center,
            ) {
                Icon(Icons.Outlined.DragHandle, null, Modifier.size(22.dp), tint = AppTheme.colors.foregroundMuted)
            }
        }
        HorizontalDivider(color = AppTheme.colors.borderSubtle)
    }
}

private const val MINUTE_MILLIS = 60_000L

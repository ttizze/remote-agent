// Declarative native layout with the fixed mobile metrics; the conversation decisions are supplied by core.
@file:Suppress("TooManyFunctions", "LongMethod", "CyclomaticComplexMethod", "LongParameterList", "MagicNumber")

package dev.remoteagent.mobile

import androidx.compose.animation.core.Animatable
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.gestures.Orientation
import androidx.compose.foundation.gestures.draggable
import androidx.compose.foundation.gestures.rememberDraggableState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.Undo
import androidx.compose.material.icons.outlined.AlarmOff
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material.icons.outlined.Edit
import androidx.compose.material.icons.outlined.ExpandMore
import androidx.compose.material.icons.outlined.HourglassTop
import androidx.compose.material.icons.outlined.PushPin
import androidx.compose.material.icons.outlined.Schedule
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.Driver
import dev.remoteagent.core.PendingTaskRow
import dev.remoteagent.core.RowVariant
import dev.remoteagent.core.ShelfHeader
import dev.remoteagent.core.SwipeAction
import dev.remoteagent.core.SwipeButton
import dev.remoteagent.core.ThreadListStatus
import dev.remoteagent.core.ThreadMenuChild
import dev.remoteagent.core.ThreadMenuItem
import dev.remoteagent.core.ThreadRow
import kotlin.math.roundToInt
import kotlinx.coroutines.launch

private val ACTION_ITEM_WIDTH = 58.dp
private const val FULL_SWIPE_FRACTION = 0.6f

/** What a row asks its list to do. */
internal class ThreadRowActions(
    val open: (ThreadRow) -> Unit,
    val swipe: (ThreadRow, SwipeAction) -> Unit,
    val snooze: (ThreadRow, ThreadMenuChild) -> Unit,
    val menu: (ThreadRow, ThreadMenuItem) -> Unit,
)

@Composable
private fun statusColor(status: ThreadListStatus, label: String?): Color? {
    val colors = AppTheme.colors
    return when {
        label == null -> null
        status == ThreadListStatus.APPROVAL || status == ThreadListStatus.LIMITED -> colors.warningForeground
        status == ThreadListStatus.INPUT -> colors.indigo
        status == ThreadListStatus.WORKING -> colors.sky
        status == ThreadListStatus.FAILED -> colors.dangerForeground
        else -> colors.emerald
    }
}

/** Shelf label and rule; "Working (3)" while collapsed. */
@Composable
internal fun ShelfHeaderRow(label: String, shelf: ShelfHeader?, snoozed: Boolean = false, onToggle: () -> Unit = {}) {
    val colors = AppTheme.colors
    val text = if (shelf == null || shelf.expanded) label else "$label (${shelf.count})"
    Row(
        Modifier.fillMaxWidth()
            .then(if (shelf != null && !shelf.disabled) Modifier.clickable(onClick = onToggle) else Modifier)
            .padding(start = 17.5.dp, end = 17.5.dp, top = 14.dp, bottom = 5.25.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.75.dp),
    ) {
        Text(
            text,
            style = AppTheme.label,
            fontWeight = FontWeight.Medium,
            color = if (snoozed) colors.foregroundSecondary else colors.foregroundTertiary,
        )
        Box(
            Modifier.weight(1f)
                .height(1.dp)
                .background(if (snoozed) colors.primary.copy(alpha = 0.2f) else colors.border)
        )
        if (shelf != null)
            Icon(
                Icons.Outlined.ExpandMore,
                null,
                Modifier.size(14.dp).rotate(if (shelf.expanded) 180f else 0f),
                tint = if (snoozed) colors.iconMuted else colors.foregroundMuted,
            )
    }
}

@Composable
internal fun ShowMoreRow(hidden: UInt, onClick: () -> Unit) {
    val colors = AppTheme.colors
    val density = LocalDensity.current
    Box(
        Modifier.fillMaxWidth()
            .padding(start = 14.dp, end = 14.dp, top = 7.dp)
            .clip(RoundedCornerShape(10.dp))
            .clickable(onClick = onClick)
            .drawBehind {
                drawRoundRect(
                    colors.border,
                    cornerRadius = CornerRadius(with(density) { 10.dp.toPx() }),
                    style =
                        Stroke(
                            width = with(density) { 1.dp.toPx() },
                            pathEffect = PathEffect.dashPathEffect(floatArrayOf(8f, 6f)),
                        ),
                )
            }
            .padding(vertical = 8.75.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            "Show more ($hidden settled hidden)",
            style = AppTheme.label,
            fontWeight = FontWeight.Medium,
            color = colors.foregroundMuted,
        )
    }
}

private fun swipeIcon(action: SwipeAction): ImageVector =
    when (action) {
        SwipeAction.SETTLE -> Icons.Outlined.Check
        SwipeAction.UNSETTLE -> Icons.AutoMirrored.Outlined.Undo
        SwipeAction.SNOOZE -> Icons.Outlined.Schedule
        SwipeAction.UNSNOOZE -> Icons.Outlined.AlarmOff
    }

@Composable
private fun SwipeActionButton(button: SwipeButton, compact: Boolean, secondary: Boolean, onClick: () -> Unit) {
    val colors = AppTheme.colors
    Column(
        Modifier.width(ACTION_ITEM_WIDTH).fillMaxHeight().clickable(onClick = onClick),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Box(
            Modifier.size(if (compact) 28.dp else 36.dp)
                .background(if (secondary) colors.secondary else colors.primary, CircleShape),
            contentAlignment = Alignment.Center,
        ) {
            Icon(
                swipeIcon(button.action),
                null,
                Modifier.size(if (compact) 13.dp else 15.dp),
                tint = if (secondary) colors.secondaryForeground else colors.primaryForeground,
            )
        }
        if (!compact) Spacer(Modifier.height(2.dp))
        Text(button.label, style = AppTheme.micro, fontWeight = FontWeight.Medium, color = colors.foregroundMuted)
    }
}

/** Swipe left to reveal the lifecycle actions; a full swipe commits the primary one. */
@Composable
private fun SwipeRow(row: ThreadRow, actions: ThreadRowActions, content: @Composable () -> Unit) {
    val density = LocalDensity.current
    val scope = rememberCoroutineScope()
    val offset = remember(row.key) { Animatable(0f) }
    var width by remember { mutableStateOf(0f) }
    var snoozeMenu by remember { mutableStateOf(false) }
    val reveal = with(density) { ACTION_ITEM_WIDTH.toPx() } * if (row.swipeSecondary != null) 2 else 1
    val background = if (row.selected) AppTheme.colors.threadSelected else AppTheme.colors.screen
    fun settle(target: Float) = scope.launch { offset.animateTo(target) }
    Box(
        Modifier.fillMaxWidth()
            .padding(horizontal = 8.dp, vertical = 2.dp)
            .clip(RoundedCornerShape(20.dp))
            .background(AppTheme.colors.screen)
    ) {
        Row(Modifier.matchParentSize(), horizontalArrangement = Arrangement.End) {
            row.swipeSecondary?.let { secondary ->
                Box {
                    SwipeActionButton(secondary, row.variant == RowVariant.SLIM, secondary = true) { snoozeMenu = true }
                    AnchoredMenu(snoozeMenu, { snoozeMenu = false }) {
                        Text(
                            "Snooze until",
                            Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                            style = AppTheme.caption,
                            color = AppTheme.colors.foregroundSecondary,
                        )
                        row.snoozeOptions.forEach { option ->
                            DropdownMenuItem(
                                text = { Text(option.label, style = AppTheme.footnote) },
                                trailingIcon =
                                    option.detail?.let { detail ->
                                        {
                                            Text(
                                                detail,
                                                style = AppTheme.caption,
                                                color = AppTheme.colors.foregroundMuted,
                                            )
                                        }
                                    },
                                onClick = {
                                    snoozeMenu = false
                                    settle(0f)
                                    actions.snooze(row, option)
                                },
                            )
                        }
                    }
                }
            }
            SwipeActionButton(row.swipePrimary, row.variant == RowVariant.SLIM, secondary = false) {
                settle(0f)
                actions.swipe(row, row.swipePrimary.action)
            }
        }
        Box(
            Modifier.fillMaxWidth()
                .offset { IntOffset(offset.value.roundToInt(), 0) }
                .background(background, RoundedCornerShape(20.dp))
                .draggable(
                    rememberDraggableState { delta ->
                        scope.launch { offset.snapTo((offset.value + delta).coerceIn(-width, 0f)) }
                    },
                    Orientation.Horizontal,
                    onDragStopped = {
                        when {
                            -offset.value >= width * FULL_SWIPE_FRACTION -> {
                                settle(0f)
                                actions.swipe(row, row.swipePrimary.action)
                            }
                            -offset.value >= reveal / 2 -> settle(-reveal)
                            else -> settle(0f)
                        }
                    },
                )
        ) {
            Box(Modifier.fillMaxWidth().then(Modifier.drawBehind { width = size.width })) { content() }
        }
    }
}

@Composable
internal fun ThreadListRow(
    row: ThreadRow,
    environmentLabel: String?,
    drivers: List<Driver>,
    actions: ThreadRowActions,
) {
    var menu by remember(row.key) { mutableStateOf(false) }
    var children by remember(row.key) { mutableStateOf<ThreadMenuItem?>(null) }
    Box {
        SwipeRow(row, actions) {
            Box(Modifier.combinedClickable(onClick = { actions.open(row) }, onLongClick = { menu = true })) {
                if (row.variant == RowVariant.CARD) CardRow(row, environmentLabel, drivers) else SlimRow(row)
            }
        }
        AnchoredMenu(
            menu || children != null,
            {
                menu = false
                children = null
            },
        ) {
            val parent = children
            if (parent == null)
                ThreadMenuItems(
                    row.menu
                        .filter { it.action != null || it.children.isNotEmpty() }
                        .map { it.copy(label = menuLabel(it)) },
                    onSelect = { item ->
                        menu = false
                        actions.menu(row, item)
                    },
                    onChildren = { item ->
                        menu = false
                        children = item
                    },
                )
            else
                parent.children.forEach { child ->
                    if (child.separatorBefore) HorizontalDivider(color = AppTheme.colors.border)
                    DropdownMenuItem(
                        text = { Text(child.label, style = AppTheme.footnote) },
                        trailingIcon =
                            child.detail?.let { detail ->
                                { Text(detail, style = AppTheme.caption, color = AppTheme.colors.foregroundMuted) }
                            },
                        onClick = {
                            children = null
                            actions.snooze(row, child)
                        },
                    )
                }
        }
    }
}

@Composable
private fun CardRow(row: ThreadRow, environmentLabel: String?, drivers: List<Driver>) {
    val colors = AppTheme.colors
    val muted = colors.foregroundMuted
    Column(Modifier.fillMaxWidth().padding(horizontal = 10.5.dp, vertical = 8.75.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(5.25.dp)) {
            ProjectFavicon(row.projectId, 15.dp)
            Text(
                row.projectTitle.orEmpty(),
                Modifier.weight(1f),
                style = AppTheme.footnote,
                fontWeight = FontWeight.Medium,
                color = muted,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            if (row.hasQueuedMessages) QueuedIcon()
            if (row.pinned) Icon(Icons.Outlined.PushPin, null, Modifier.size(11.dp), tint = muted)
            Text(
                row.statusLabel ?: row.snoozeWakeLabel ?: row.timeLabel,
                style = AppTheme.caption,
                color = statusColor(row.status, row.statusLabel) ?: colors.foregroundTertiary,
            )
        }
        Text(
            row.title,
            Modifier.padding(top = 3.5.dp),
            style = AppTheme.body,
            fontWeight = FontWeight.Medium,
            color = colors.foreground,
            maxLines = 2,
            overflow = TextOverflow.Ellipsis,
        )
        row.pullRequestLabel?.let {
            Text(
                it,
                Modifier.padding(top = 3.5.dp),
                style = AppTheme.caption.copy(fontFamily = AppTheme.mono),
                color = colors.primaryText,
                maxLines = 1,
            )
        }
        row.searchSnippet?.let {
            Text(it, Modifier.padding(top = 3.5.dp), style = AppTheme.caption, color = muted, maxLines = 2)
        }
        Row(
            Modifier.padding(top = 3.5.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(7.dp),
        ) {
            Text(
                buildAnnotatedString {
                    row.branch?.let { withStyle(SpanStyle(fontFamily = AppTheme.mono)) { append(it) } }
                    if (row.branch != null && environmentLabel != null) append("  ·  ")
                    environmentLabel?.let { withStyle(SpanStyle(color = colors.foregroundTertiary)) { append(it) } }
                },
                Modifier.weight(1f),
                style = AppTheme.caption,
                color = muted,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            ProviderStack(drivers)
        }
    }
}

/** Earlier owners peek out behind the current provider. */
@Composable
private fun ProviderStack(drivers: List<Driver>) {
    if (drivers.isEmpty()) return
    Row(verticalAlignment = Alignment.CenterVertically) {
        drivers.dropLast(1).forEach { driver ->
            ProviderIcon(driver, 12.dp, Modifier.padding(end = 0.dp).then(Modifier.offset(x = 4.dp)).alphaLayer(0.3f))
        }
        ProviderIcon(drivers.last(), 14.dp)
    }
}

private fun Modifier.alphaLayer(value: Float) = alpha(value)

@Composable
private fun QueuedIcon() {
    Icon(
        Icons.Outlined.HourglassTop,
        "Messages queued to send",
        Modifier.size(12.dp),
        tint = AppTheme.colors.primaryText,
    )
}

@Composable
private fun SlimRow(row: ThreadRow) {
    val colors = AppTheme.colors
    Row(
        Modifier.fillMaxWidth().heightIn(min = 44.dp).padding(horizontal = 17.5.dp, vertical = 7.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.75.dp),
    ) {
        ProjectFavicon(row.projectId, 15.dp, Modifier.alphaLayer(0.4f))
        Column(Modifier.weight(1f)) {
            Text(
                row.title,
                style = AppTheme.body,
                color = if (row.selected) colors.foreground else colors.foregroundMuted,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            row.searchSnippet?.let { Text(it, style = AppTheme.caption, color = colors.foregroundMuted, maxLines = 1) }
        }
        if (row.hasQueuedMessages) QueuedIcon()
        Text(
            if (row.snoozed) row.snoozeWakeLabel ?: row.timeLabel else row.timeLabel,
            style = AppTheme.footnote,
            fontFamily = AppTheme.mono,
            color = if (row.snoozed) colors.foregroundMuted else colors.foregroundTertiary,
        )
    }
}

@Composable
internal fun PendingTaskListRow(
    task: PendingTaskRow,
    status: String,
    isDraft: Boolean,
    onOpen: () -> Unit,
    onDelete: () -> Unit,
) {
    val colors = AppTheme.colors
    var menu by remember { mutableStateOf(false) }
    if (task.showPendingDivider) ShelfHeaderRow("Unsent", null)
    Box(
        Modifier.fillMaxWidth()
            .padding(horizontal = 8.dp, vertical = 2.dp)
            .clip(RoundedCornerShape(20.dp))
            .combinedClickable(onClick = onOpen, onLongClick = { menu = true })
    ) {
        Column(Modifier.fillMaxWidth().padding(horizontal = 10.5.dp, vertical = 8.75.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(5.25.dp)) {
                ProjectFavicon(task.projectId, 15.dp)
                Text(
                    task.projectTitle,
                    Modifier.weight(1f),
                    style = AppTheme.footnote,
                    fontWeight = FontWeight.Medium,
                    color = colors.foregroundMuted,
                    maxLines = 1,
                )
                Row(
                    horizontalArrangement = Arrangement.spacedBy(3.5.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    val tint = if (isDraft) colors.amber else colors.foregroundTertiary
                    if (isDraft) Icon(Icons.Outlined.Edit, null, Modifier.size(10.dp), tint = tint)
                    Text(status, style = AppTheme.caption, color = tint)
                }
            }
            Text(
                task.title,
                Modifier.padding(top = 3.5.dp),
                style = AppTheme.body,
                fontWeight = FontWeight.Medium,
                color = colors.foreground,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            task.branch?.let {
                Text(
                    it,
                    Modifier.padding(top = 3.5.dp),
                    style = AppTheme.caption,
                    fontFamily = AppTheme.mono,
                    color = colors.foregroundMuted,
                    maxLines = 1,
                )
            }
        }
        AnchoredMenu(menu, { menu = false }) {
            DropdownMenuItem(
                text = { Text("Delete", style = AppTheme.footnote, color = colors.dangerForeground) },
                onClick = {
                    menu = false
                    onDelete()
                },
            )
        }
    }
}

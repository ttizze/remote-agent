// Declarative native layout with the fixed mobile metrics; the conversation decisions are supplied by core.
@file:Suppress("TooManyFunctions", "LongMethod", "CyclomaticComplexMethod", "LongParameterList", "MagicNumber")

package dev.remoteagent.mobile

import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.CallSplit
import androidx.compose.material.icons.automirrored.outlined.OpenInNew
import androidx.compose.material.icons.outlined.Apps
import androidx.compose.material.icons.outlined.Bolt
import androidx.compose.material.icons.outlined.Build
import androidx.compose.material.icons.outlined.ChatBubbleOutline
import androidx.compose.material.icons.outlined.CheckCircle
import androidx.compose.material.icons.outlined.ChevronRight
import androidx.compose.material.icons.outlined.Computer
import androidx.compose.material.icons.outlined.ContentCopy
import androidx.compose.material.icons.outlined.Edit
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material.icons.outlined.Handyman
import androidx.compose.material.icons.outlined.Language
import androidx.compose.material.icons.outlined.Lock
import androidx.compose.material.icons.outlined.Psychology
import androidx.compose.material.icons.outlined.Public
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material.icons.outlined.SmartToy
import androidx.compose.material.icons.outlined.Terminal
import androidx.compose.material.icons.outlined.Visibility
import androidx.compose.material.icons.outlined.Warning
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.remoteagent.core.AssistantMessageRow
import dev.remoteagent.core.AssistantMeta
import dev.remoteagent.core.ChangedFileRowKind
import dev.remoteagent.core.ChangedFilesCard
import dev.remoteagent.core.ContextChip
import dev.remoteagent.core.DividerTone
import dev.remoteagent.core.FoldKind
import dev.remoteagent.core.IntentTone
import dev.remoteagent.core.LifecycleRow
import dev.remoteagent.core.PendingMessageRow
import dev.remoteagent.core.Phase
import dev.remoteagent.core.PlanCard
import dev.remoteagent.core.SubagentGroupCard
import dev.remoteagent.core.SubagentGroupTone
import dev.remoteagent.core.TimelineRow
import dev.remoteagent.core.TimelineRowKind
import dev.remoteagent.core.UserMessageRow
import dev.remoteagent.core.WorkActivityRow
import dev.remoteagent.core.WorkIcon
import dev.remoteagent.core.WorkIconTone
import dev.remoteagent.core.WorkLabelTone
import dev.remoteagent.core.WorkLogRow
import dev.remoteagent.core.WorkRowIcon
import dev.remoteagent.core.workingTimerLabel
import dev.remoteagent.core.mobileMessageContextChips
import dev.remoteagent.core.mobileMessageMarkdown
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle

private const val SHIMMER_MILLIS = 1100

/** What a feed row asks the screen to do. */
internal class FeedActions(
    val toggleRun: (String) -> Unit,
    val toggleAttempt: (String) -> Unit,
    val toggleGroup: (String) -> Unit,
    val toggleEntry: (WorkActivityRow) -> Unit,
    val toggleFolder: (ChangedFilesCard, String, Boolean) -> Unit,
    val toggleAllFolders: (ChangedFilesCard) -> Unit,
    val openThread: (String) -> Unit,
    val openTerminal: (String?) -> Unit,
    val showContextPreview: (ContextChip) -> Unit,
    val openDiff: (ChangedFilesCard) -> Unit,
    val fork: (String) -> Unit,
    val copy: (String) -> Unit,
    val retryPreparation: (String) -> Unit,
)

internal fun clockLabel(millis: Long): String =
    Instant.ofEpochMilli(millis)
        .atZone(ZoneId.systemDefault())
        .format(DateTimeFormatter.ofLocalizedTime(FormatStyle.SHORT))

@Composable
internal fun Shimmer(text: String, modifier: Modifier = Modifier, color: Color = AppTheme.colors.foregroundMuted) {
    val transition = rememberInfiniteTransition(label = "shimmer")
    val alpha by
        transition.animateFloat(
            0.45f,
            1f,
            infiniteRepeatable(tween(SHIMMER_MILLIS), RepeatMode.Reverse),
            label = "alpha",
        )
    Text(
        text,
        modifier.alpha(alpha),
        style = AppTheme.footnote,
        color = color,
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
    )
}

@Composable
internal fun FeedRow(model: AndroidAppModel, row: TimelineRow, now: Long, actions: FeedActions) {
    when (val kind = row.kind) {
        is TimelineRowKind.UserMessage -> UserBubble(model, kind.v1, actions)
        is TimelineRowKind.PendingMessage -> PendingBubble(model, kind.v1, actions)
        is TimelineRowKind.AssistantMessage -> AssistantMessage(model, kind.v1, row.createdAt, actions)
        is TimelineRowKind.AssistantMeta -> MetaRow(kind.meta, row.createdAt, actions)
        is TimelineRowKind.Work -> kind.rows.forEach { WorkRow(model, it, actions) }
        is TimelineRowKind.LiveWork ->
            WorkLine(
                icon = workIcon(kind.row),
                label = kind.label,
                shimmer = kind.active,
                trailing = if (kind.callCount > 1u) "${kind.callCount}" else null,
            ) {
                actions.toggleGroup(kind.groupId)
            }
        is TimelineRowKind.WorkToggle ->
            WorkLine(
                rowIcon(kind.v1.icon),
                kind.v1.summary,
                shimmer = kind.v1.shimmer,
                chevron = kind.v1.expanded,
                tone = if (kind.v1.hasFailure) WorkIconTone.FAILED else WorkIconTone.DEFAULT,
            ) {
                actions.toggleGroup(kind.v1.groupId)
            }
        is TimelineRowKind.Thinking ->
            WorkLine(Icons.Outlined.Psychology, "Thinking", shimmer = true) { kind.groupId?.let(actions.toggleGroup) }
        TimelineRowKind.Working -> WorkingRow("Working for ${workingTimerLabel(row.createdAt ?: now, now)}")
        is TimelineRowKind.Fold ->
            FoldLine(kind.v1.label, kind.v1.expanded) {
                if (kind.v1.kind == FoldKind.ATTEMPT) kind.v1.attempt?.let(actions.toggleAttempt)
                else actions.toggleRun(kind.v1.run)
            }
        is TimelineRowKind.ContextCompaction -> Divider(kind.label, null, DividerTone.NEUTRAL, active = kind.active)
        is TimelineRowKind.Lifecycle -> Lifecycle(kind.v1, actions)
        is TimelineRowKind.Subagents -> SubagentGroup(kind.v1, row.id, actions)
        is TimelineRowKind.Handoff -> Divider(kind.v1.label, null, kind.v1.tone)
        is TimelineRowKind.ProposedPlan -> ProposedPlan(kind.v1, model::useArtifactTemplate)
        is TimelineRowKind.WorktreeSetup -> Unit
    }
}

@Composable
private fun Bubble(
    model: AndroidAppModel,
    text: String,
    attachments: List<dev.remoteagent.core.Attachment>,
    above: (@Composable () -> Unit)?,
    context: String?,
    onOpenContext: (ContextChip) -> Unit,
    faded: Boolean = false,
) {
    val source = mobileMessageMarkdown(text, context)
    val contextChips = mobileMessageContextChips(text, context, attachments)
    BoxWithConstraints(Modifier.fillMaxWidth().padding(bottom = 17.5.dp)) {
        val bubbleWidth = maxWidth * 0.85f
        Column(
            Modifier.align(Alignment.CenterEnd).alpha(if (faded) 0.6f else 1f),
            horizontalAlignment = Alignment.End,
        ) {
            above?.invoke()
            MessageAttachments(model, attachments, alignEnd = true)
            if (text.isNotEmpty())
                Box(
                    Modifier.padding(top = if (attachments.isEmpty()) 0.dp else 7.dp)
                        .widthIn(max = bubbleWidth)
                        .background(AppTheme.colors.userBubble, RoundedCornerShape(20.dp))
                        .padding(horizontal = 12.25.dp, vertical = 8.75.dp)
                ) {
                    SelectionContainer {
                        MarkdownText(
                            source,
                            color = AppTheme.colors.userBubbleForeground,
                            contextChips = contextChips,
                            onOpenContext = onOpenContext,
                        )
                    }
                }
        }
    }
}

@Composable
private fun UserBubble(model: AndroidAppModel, row: UserMessageRow, actions: FeedActions) {
    val above: (@Composable () -> Unit)? =
        if (row.badge == null && row.decorations.attribution == null) null
        else {
            {
                Row(Modifier.padding(bottom = 3.5.dp), horizontalArrangement = Arrangement.spacedBy(7.dp)) {
                    row.decorations.attribution?.let { attribution ->
                        Text(
                            attribution.label,
                            Modifier.clickable(enabled = attribution.senderThread != null) {
                                attribution.senderThread?.let(model::openThread)
                            },
                            style = AppTheme.caption,
                            color = AppTheme.colors.foregroundMuted,
                        )
                    }
                    row.badge?.let { badge ->
                        Text(
                            badge.label,
                            style = AppTheme.caption,
                            fontWeight = FontWeight.Medium,
                            color =
                                if (badge.tone == IntentTone.STEER) AppTheme.colors.primaryText
                                else AppTheme.colors.foregroundMuted,
                        )
                    }
                }
            }
        }
    Bubble(
        model,
        row.text,
        row.attachments,
        above,
        row.context,
        { chip ->
            when (chip.kind) {
                dev.remoteagent.core.ContextChipKind.THREAD -> chip.threadId?.let(actions.openThread)
                dev.remoteagent.core.ContextChipKind.TERMINAL ->
                    if (chip.previewText?.isNotEmpty() == true) actions.showContextPreview(chip)
                    else actions.openTerminal(chip.terminalId)
                else -> Unit
            }
        },
    )
}

@Composable
private fun PendingBubble(model: AndroidAppModel, row: PendingMessageRow, actions: FeedActions) {
    val status =
        when (val phase = row.phase) {
            is Phase.Uncertain -> "Delivery unconfirmed · ${phase.error}"
            is Phase.Queued -> if (row.queued) "Queued" else "Sends on reconnect"
            else -> if (row.queued) "Queued" else null
        }
    Bubble(
        model,
        row.text,
        row.attachments,
        status?.let { label ->
            {
                Text(
                    label,
                    Modifier.padding(bottom = 3.5.dp),
                    style = AppTheme.caption,
                    color = AppTheme.colors.foregroundMuted,
                )
            }
        },
        row.context,
        { chip ->
            when (chip.kind) {
                dev.remoteagent.core.ContextChipKind.THREAD -> chip.threadId?.let(actions.openThread)
                dev.remoteagent.core.ContextChipKind.TERMINAL ->
                    if (chip.previewText?.isNotEmpty() == true) actions.showContextPreview(chip)
                    else actions.openTerminal(chip.terminalId)
                else -> Unit
            }
        },
        faded = true,
    )
}

@Composable
private fun AssistantMessage(model: AndroidAppModel, row: AssistantMessageRow, createdAt: Long?, actions: FeedActions) {
    Column(
        Modifier.fillMaxWidth().padding(horizontal = 3.5.dp).padding(bottom = if (row.meta == null) 3.5.dp else 0.dp)
    ) {
        MarkdownText(row.text, onUseArtifactTemplate = model::useArtifactTemplate)
        MessageAttachments(model, row.attachments, alignEnd = false)
        row.changedFiles?.let { ChangedFiles(it, actions) }
        row.meta?.let { MetaRow(it, createdAt, actions) }
    }
}

@Composable
private fun MetaRow(meta: AssistantMeta, createdAt: Long?, actions: FeedActions) {
    val colors = AppTheme.colors
    Row(
        Modifier.padding(top = 3.5.dp, bottom = 14.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        meta.copy.text
            ?.takeIf { meta.copy.visible }
            ?.let { text ->
                Icon(
                    Icons.Outlined.ContentCopy,
                    "Copy",
                    Modifier.size(16.dp).clickable { actions.copy(text) },
                    tint = colors.iconMuted,
                )
            }
        meta.fork?.let { fork ->
            Icon(
                Icons.AutoMirrored.Outlined.CallSplit,
                fork.label,
                Modifier.size(16.dp).clickable { actions.fork(fork.run) },
                tint = colors.iconMuted,
            )
        }
        meta.statusChip?.let { Text(it, style = AppTheme.caption, color = colors.foregroundMuted) }
        if (meta.showTimestamp && createdAt != null)
            Text(clockLabel(createdAt), style = AppTheme.caption, color = colors.foregroundMuted)
    }
}

private fun feedIcon(icon: WorkIcon): ImageVector =
    when (icon) {
        WorkIcon.AGENT -> Icons.Outlined.SmartToy
        WorkIcon.ALERT -> Icons.Outlined.ErrorOutline
        WorkIcon.BROWSER -> Icons.Outlined.Language
        WorkIcon.COMPUTER -> Icons.Outlined.Computer
        WorkIcon.CHECK -> Icons.Outlined.CheckCircle
        WorkIcon.COMMAND -> Icons.Outlined.Terminal
        WorkIcon.EDIT -> Icons.Outlined.Edit
        WorkIcon.EYE -> Icons.Outlined.Visibility
        WorkIcon.GLOBE -> Icons.Outlined.Public
        WorkIcon.SEARCH -> Icons.Outlined.Search
        WorkIcon.HAMMER -> Icons.Outlined.Handyman
        WorkIcon.LOCK -> Icons.Outlined.Lock
        WorkIcon.MESSAGE -> Icons.Outlined.ChatBubbleOutline
        WorkIcon.WARNING -> Icons.Outlined.Warning
        WorkIcon.WRENCH -> Icons.Outlined.Build
        WorkIcon.ZAP -> Icons.Outlined.Bolt
    }

private fun rowIcon(icon: WorkRowIcon): ImageVector =
    when (icon) {
        WorkRowIcon.Brain -> Icons.Outlined.Psychology
        is WorkRowIcon.Logo -> Icons.Outlined.Apps
        is WorkRowIcon.Feed -> feedIcon(icon.v1)
    }

private fun workIcon(row: WorkLogRow): ImageVector =
    when (row) {
        is WorkLogRow.ProviderFailure -> if (row.v1.warning) Icons.Outlined.Warning else Icons.Outlined.ErrorOutline
        is WorkLogRow.Activity -> rowIcon(row.v1.icon)
    }

@Composable
private fun toneColor(tone: WorkIconTone): Color =
    when (tone) {
        WorkIconTone.DEFAULT -> AppTheme.colors.iconMuted
        WorkIconTone.WARNING -> AppTheme.colors.warningForeground
        WorkIconTone.DESTRUCTIVE -> AppTheme.colors.dangerForeground
        WorkIconTone.FAILED -> AppTheme.colors.dangerForeground.copy(alpha = 0.6f)
    }

/** A work-log line: 28 tall, a 24 icon slot and a muted one-line label. */
@Composable
private fun WorkLine(
    icon: ImageVector,
    label: String,
    shimmer: Boolean = false,
    chevron: Boolean? = null,
    tone: WorkIconTone = WorkIconTone.DEFAULT,
    labelColor: Color = AppTheme.colors.foregroundMuted,
    trailing: String? = null,
    onClick: (() -> Unit)? = null,
) {
    Row(
        Modifier.fillMaxWidth()
            .heightIn(min = 28.dp)
            .then(if (onClick != null) Modifier.clickable(onClick = onClick) else Modifier),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(Modifier.width(24.dp), contentAlignment = Alignment.CenterStart) {
            Icon(icon, null, Modifier.size(16.dp), tint = toneColor(tone))
        }
        if (shimmer) Shimmer(label, Modifier.weight(1f), labelColor)
        else
            Text(
                label,
                Modifier.weight(1f),
                style = AppTheme.footnote,
                color = labelColor,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        trailing?.let { Text(it, style = AppTheme.caption, color = AppTheme.colors.foregroundTertiary) }
        if (chevron != null)
            Icon(
                Icons.Outlined.ChevronRight,
                null,
                Modifier.size(12.dp).rotate(if (chevron) 90f else 0f),
                tint = AppTheme.colors.iconMuted,
            )
    }
}

@Composable
private fun WorkRow(model: AndroidAppModel, row: WorkLogRow, actions: FeedActions) {
    val colors = AppTheme.colors
    when (row) {
        is WorkLogRow.ProviderFailure -> {
            val failure = row.v1
            Column(Modifier.padding(vertical = 3.5.dp)) {
                WorkLine(
                    workIcon(row),
                    failure.summary,
                    tone = if (failure.warning) WorkIconTone.WARNING else WorkIconTone.DESTRUCTIVE,
                    labelColor = if (failure.warning) colors.warningForeground else colors.dangerForeground,
                )
                Text(
                    failure.message,
                    Modifier.padding(start = 24.dp),
                    style = AppTheme.caption,
                    color = colors.foregroundMuted,
                )
                failure.retryPreparation?.let { run ->
                    Row(
                        Modifier.padding(start = 28.dp, top = 8.dp)
                            .heightIn(min = 44.dp)
                            .border(1.dp, colors.border, CircleShape)
                            .clip(CircleShape)
                            .clickable { actions.retryPreparation(run) }
                            .padding(horizontal = 16.dp),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(6.dp),
                    ) {
                        Icon(Icons.Outlined.Refresh, null, Modifier.size(13.dp), tint = colors.icon)
                        Text(
                            "Retry",
                            style = AppTheme.footnote,
                            fontWeight = FontWeight.Medium,
                            color = colors.foreground,
                        )
                    }
                }
            }
        }
        is WorkLogRow.Activity -> {
            val activity = row.v1
            val labelColor =
                when (activity.labelTone) {
                    WorkLabelTone.DEFAULT -> colors.foregroundMuted
                    WorkLabelTone.WARNING -> colors.warningForeground
                    WorkLabelTone.DANGER -> colors.rose
                }
            Column {
                WorkLine(
                    workIcon(row),
                    activity.label,
                    shimmer = activity.shimmer,
                    chevron = if (activity.canExpand) activity.expanded else null,
                    tone = activity.iconTone,
                    labelColor = labelColor,
                ) {
                    val thread = activity.opensThread
                    when {
                        thread != null -> actions.openThread(thread)
                        activity.canExpand -> actions.toggleEntry(activity)
                    }
                }
                activity.answerPreview?.let {
                    Text(
                        it,
                        Modifier.padding(start = 24.dp),
                        style = AppTheme.caption,
                        color = colors.foregroundSecondary,
                    )
                }
                if (activity.expanded)
                    activity.detail?.let { detail ->
                        detail.questionAnswer?.let { AnswerHistory(model, it) }
                        WorkDetail(model, detail)
                    }
            }
        }
    }
}

@Composable
private fun WorkDetail(model: AndroidAppModel, detail: dev.remoteagent.core.WorkActivityDetail) {
    detail.reasoning?.takeIf { it.isNotBlank() }?.let {
        MarkdownText(it, onUseArtifactTemplate = model::useArtifactTemplate)
    }
    val text =
        listOfNotNull(
                detail.call?.command,
                detail.call?.argsText,
                detail.call?.args?.joinToString("\n") { "${it.key}: ${it.value}" },
                detail.fullDetail,
                detail.output,
                detail.failedExitCode?.let { "Exit code $it" },
            )
            .filter { it.isNotBlank() }
            .joinToString("\n\n")
    if (text.isEmpty()) return
    Surface(
        Modifier.fillMaxWidth().padding(start = 24.dp, top = 3.5.dp, bottom = 7.dp),
        color = AppTheme.colors.mdCodeBackground,
        shape = RoundedCornerShape(10.dp),
        border = BorderStroke(1.dp, AppTheme.colors.border),
    ) {
        SelectionContainer {
            if (AppTheme.codeWordWrap)
                Text(
                    text,
                    Modifier.fillMaxWidth().heightIn(max = 256.dp)
                        .verticalScroll(rememberScrollState())
                        .padding(10.dp),
                    fontFamily = AppTheme.mono,
                    fontSize = AppTheme.codeFontSize.sp,
                    lineHeight = AppTheme.codeLineHeight.sp,
                    softWrap = true,
                    color = AppTheme.colors.foreground,
                )
            else
                Text(
                    text,
                    Modifier.heightIn(max = 256.dp)
                        .verticalScroll(rememberScrollState())
                        .horizontalScroll(rememberScrollState())
                        .padding(10.dp),
                    fontFamily = AppTheme.mono,
                    fontSize = AppTheme.codeFontSize.sp,
                    lineHeight = AppTheme.codeLineHeight.sp,
                    softWrap = false,
                    color = AppTheme.colors.foreground,
                )
        }
    }
}

@Composable
private fun WorkingRow(label: String) {
    Column(Modifier.fillMaxWidth().padding(vertical = 3.5.dp)) {
        Box(Modifier.heightIn(min = 38.5.dp), contentAlignment = Alignment.CenterStart) { Shimmer(label) }
        HorizontalDivider(color = AppTheme.colors.border)
    }
}

@Composable
private fun FoldLine(label: String, expanded: Boolean, onClick: () -> Unit) {
    Column(Modifier.fillMaxWidth().padding(bottom = 5.25.dp)) {
        Row(
            Modifier.fillMaxWidth().heightIn(min = 38.5.dp).clickable(onClick = onClick),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            Text(label, style = AppTheme.footnote, color = AppTheme.colors.foregroundSecondary)
            Icon(
                Icons.Outlined.ChevronRight,
                null,
                Modifier.size(12.dp).rotate(if (expanded) 90f else 0f),
                tint = AppTheme.colors.iconMuted,
            )
        }
        HorizontalDivider(color = AppTheme.colors.border)
    }
}

@Composable
private fun Divider(
    label: String,
    detail: String?,
    tone: DividerTone,
    active: Boolean = false,
    action: (@Composable () -> Unit)? = null,
) {
    val color = if (tone == DividerTone.DANGER) AppTheme.colors.dangerForeground else AppTheme.colors.foregroundMuted
    Row(
        Modifier.fillMaxWidth().padding(vertical = 10.5.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Box(Modifier.weight(1f).height(1.dp).background(AppTheme.colors.border))
        if (active) Shimmer(label, color = color) else Text(label, style = AppTheme.caption, color = color)
        detail?.let { Text(it, style = AppTheme.caption, color = AppTheme.colors.foregroundTertiary, maxLines = 1) }
        action?.invoke()
        Box(Modifier.weight(1f).height(1.dp).background(AppTheme.colors.border))
    }
}

@Composable
private fun Lifecycle(row: LifecycleRow, actions: FeedActions) {
    val colors = AppTheme.colors
    when (row) {
        is LifecycleRow.Divider ->
            Divider(row.v1.label, row.v1.detail, row.v1.tone) {
                row.v1.action?.let { action ->
                    Text(
                        action.label,
                        Modifier.clickable { actions.openThread(action.thread) },
                        style = AppTheme.caption,
                        color = colors.primaryText,
                    )
                }
            }
        is LifecycleRow.InterruptRequest ->
            WorkLine(Icons.Outlined.ErrorOutline, "${row.v1.label} · ${row.v1.message}", tone = WorkIconTone.WARNING)
        is LifecycleRow.CreatedThread ->
            WorkLine(Icons.AutoMirrored.Outlined.OpenInNew, row.v1.label, trailing = row.v1.actionLabel) {
                actions.openThread(row.v1.thread)
            }
        is LifecycleRow.Subagent ->
            WorkLine(
                Icons.Outlined.SmartToy,
                listOfNotNull(row.v1.title, row.v1.statusLabel.takeIf { it.isNotEmpty() }).joinToString(" · "),
                shimmer = row.v1.liveStatus == dev.remoteagent.core.ItemStatus.RUNNING,
                tone = if (row.v1.failed) WorkIconTone.FAILED else WorkIconTone.DEFAULT,
            ) {
                row.v1.thread?.let(actions.openThread)
            }
    }
}

@Composable
private fun SubagentGroup(card: SubagentGroupCard, rowId: String, actions: FeedActions) {
    val colors = AppTheme.colors
    Column(Modifier.fillMaxWidth().padding(vertical = 3.5.dp)) {
        WorkLine(
            Icons.Outlined.SmartToy,
            listOf(card.label, card.summary).filter { it.isNotEmpty() }.joinToString(" · "),
            shimmer = card.tone == SubagentGroupTone.ACTIVE,
            chevron = if (card.grouped) card.expanded else null,
            tone = if (card.tone == SubagentGroupTone.FAILED) WorkIconTone.FAILED else WorkIconTone.DEFAULT,
            trailing = card.elapsed,
        ) {
            if (card.grouped) actions.toggleGroup(rowId)
            else card.members.firstOrNull()?.link?.thread?.let(actions.openThread)
        }
        if (card.showsMembers)
            card.members.forEach { member ->
                Row(
                    Modifier.fillMaxWidth().padding(start = 24.dp).heightIn(min = 28.dp).clickable(
                        enabled = member.link.thread != null
                    ) {
                        member.link.thread?.let(actions.openThread)
                    },
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(7.dp),
                ) {
                    ProviderIcon(member.link.driver, 12.dp)
                    Text(
                        member.link.title,
                        Modifier.weight(1f),
                        style = AppTheme.footnote,
                        color = colors.foregroundSecondary,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Text(member.link.statusLabel, style = AppTheme.caption, color = colors.foregroundMuted)
                    member.elapsed?.let { Text(it, style = AppTheme.caption, color = colors.foregroundTertiary) }
                }
            }
        if (card.overflow > 0u)
            Text(
                "+${card.overflow} more",
                Modifier.padding(start = 24.dp),
                style = AppTheme.caption,
                color = colors.foregroundMuted,
            )
    }
}

@Composable
private fun ChangedFiles(card: ChangedFilesCard, actions: FeedActions) {
    val colors = AppTheme.colors
    Surface(
        Modifier.fillMaxWidth().padding(top = 14.dp),
        color = colors.subtleStrong,
        shape = RoundedCornerShape(10.dp),
    ) {
        Column(Modifier.padding(vertical = 7.dp)) {
            Row(
                Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(
                    card.title,
                    Modifier.weight(1f),
                    style = AppTheme.footnote,
                    fontWeight = FontWeight.Medium,
                    color = colors.foreground,
                )
                card.stat?.let { DiffStatText(it) }
                card.toggleAllLabel?.let {
                    Text(
                        it,
                        Modifier.clickable { actions.toggleAllFolders(card) },
                        style = AppTheme.caption,
                        color = colors.primaryText,
                    )
                }
                Text(
                    card.openDiffLabel,
                    Modifier.clickable { actions.openDiff(card) },
                    style = AppTheme.caption,
                    color = colors.primaryText,
                )
            }
            card.rows.forEach { file ->
                val directory = file.kind as? ChangedFileRowKind.Directory
                Row(
                    Modifier.fillMaxWidth()
                        .clickable(enabled = directory != null) {
                            directory?.let { actions.toggleFolder(card, file.path, !it.expanded) }
                        }
                        .padding(start = 12.dp + 14.dp * file.depth.toInt(), end = 12.dp, top = 3.dp, bottom = 3.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    if (directory != null)
                        Icon(
                            Icons.Outlined.ChevronRight,
                            null,
                            Modifier.size(12.dp).rotate(if (directory.expanded) 90f else 0f),
                            tint = colors.iconMuted,
                        )
                    else if (file.leadingSpacer) Spacer(Modifier.width(12.dp))
                    Text(
                        file.name,
                        Modifier.weight(1f),
                        style = AppTheme.caption,
                        fontFamily = AppTheme.mono,
                        color = colors.foregroundSecondary,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    file.stat?.let { DiffStatText(it) }
                }
            }
        }
    }
}

@Composable
internal fun DiffStatText(stat: dev.remoteagent.core.DiffStatLabel) {
    Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(stat.additions, style = AppTheme.caption, fontFamily = AppTheme.mono, color = AppTheme.colors.emerald)
        Text(stat.deletions, style = AppTheme.caption, fontFamily = AppTheme.mono, color = AppTheme.colors.rose)
    }
}

@Composable
private fun ProposedPlan(card: PlanCard, useArtifactTemplate: (dev.remoteagent.core.ArtifactTemplate) -> Unit) {
    val colors = AppTheme.colors
    var expanded by
        androidx.compose.runtime.saveable.rememberSaveable(card.plan) { androidx.compose.runtime.mutableStateOf(false) }
    Surface(
        Modifier.fillMaxWidth().padding(bottom = 14.dp),
        color = colors.card,
        shape = RoundedCornerShape(22.dp),
        border = BorderStroke(1.dp, colors.border),
    ) {
        Column(Modifier.padding(14.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(
                    "Plan",
                    Modifier.background(colors.subtleStrong, CircleShape).padding(horizontal = 8.dp, vertical = 2.dp),
                    style = AppTheme.caption,
                    fontWeight = FontWeight.Medium,
                    color = colors.foregroundSecondary,
                )
                Text(
                    card.title,
                    style = AppTheme.footnote,
                    fontWeight = FontWeight.Medium,
                    color = colors.foreground,
                    maxLines = 1,
                )
            }
            val preview = card.collapsedPreview
            MarkdownText(
                if (expanded || preview == null) card.displayedMarkdown else preview,
                onUseArtifactTemplate = useArtifactTemplate,
            )
            if (preview != null)
                Text(
                    if (expanded) "Collapse plan" else "Expand plan",
                    Modifier.clickable { expanded = !expanded },
                    style = AppTheme.footnote,
                    fontWeight = FontWeight.Medium,
                    color = colors.primaryText,
                )
        }
    }
}

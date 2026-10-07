// Declarative native layout with the fixed mobile metrics; the conversation decisions are supplied by core.
@file:Suppress("TooManyFunctions", "LongMethod", "CyclomaticComplexMethod", "LongParameterList", "MagicNumber")

package dev.remoteagent.mobile

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.ArrowDownward
import androidx.compose.material.icons.outlined.ChevronRight
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.Folder
import androidx.compose.material.icons.outlined.Terminal
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.BannerVariant
import dev.remoteagent.core.ChangedFilesDisclosure
import dev.remoteagent.core.ComposerOptions
import dev.remoteagent.core.ComposerShortcuts
import dev.remoteagent.core.HeaderActionKind
import dev.remoteagent.core.HeaderPanelState
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Outcome
import dev.remoteagent.core.RecoveryAction
import dev.remoteagent.core.SetupCardView
import dev.remoteagent.core.SetupTone
import dev.remoteagent.core.ThreadStatus
import dev.remoteagent.core.ThreadView
import dev.remoteagent.core.ThreadViewOptions
import dev.remoteagent.core.TimelineDisclosure
import dev.remoteagent.core.TimelineLayout
import dev.remoteagent.core.WorkingControlView
import kotlinx.coroutines.launch

private const val END_SLACK_PX = 80
private const val MINUTE_MILLIS = 60_000L

private fun List<String>.toggled(id: String) = if (id in this) this - id else this + id

/** What the feed has open; core folds it into the rows. */
private class Disclosure {
    var value by mutableStateOf(TimelineDisclosure(emptyList(), emptyList(), emptyList(), emptyList(), emptyList()))
}

private fun mobileOptions(disclosure: TimelineDisclosure, showScrollToEnd: Boolean) =
    ThreadViewOptions(
        layout = TimelineLayout.MOBILE,
        disclosure = disclosure,
        panels =
            HeaderPanelState(threadPanelOpen = false, terminalOpen = false, rightPanelOpen = false, filesOpen = false),
        composer =
            ComposerOptions(compact = true, alternateModifier = false, shortcuts = ComposerShortcuts(null, null, null)),
        showScrollToEnd = showScrollToEnd,
    )

@Composable
@Suppress("LongMethod")
internal fun ThreadScreen(model: AndroidAppModel, threadId: String) {
    val now = rememberNow()
    val disclosure = remember(threadId) { Disclosure() }
    val listState = rememberLazyListState()
    val atEnd by remember {
        derivedStateOf { listState.firstVisibleItemIndex == 0 && listState.firstVisibleItemScrollOffset < END_SLACK_PX }
    }
    val options = mobileOptions(disclosure.value, !atEnd)
    // Live work and setup count seconds; otherwise relative times move by the minute.
    var live by remember { mutableStateOf(true) }
    val tick = if (live) now else now / MINUTE_MILLIS
    val view by
        rememberView(model.snapshot, threadId, tick, options) {
            it.thread(threadId, System.currentTimeMillis(), options)
        }
    SideEffect { live = view?.let { it.working?.status != null || it.setup.card != null } ?: true }
    var sheet by remember { mutableStateOf<ThreadSheet?>(null) }
    val copy = rememberCopy()
    val current = view
    val header = current?.header
    ScreenScaffold(
        header?.title ?: "",
        subtitle = header?.subtitle,
        onBack = model::back,
        actions = { current?.let { HeaderActions(model, it) } },
    ) {
        if (current == null || current.syncStatus == ThreadStatus.DELETED) {
            Unavailable(current == null)
            return@ScreenScaffold
        }
        val feedActions =
            FeedActions(
                toggleRun = { id ->
                    disclosure.value = disclosure.value.copy(expandedRuns = disclosure.value.expandedRuns.toggled(id))
                },
                toggleAttempt = { id ->
                    disclosure.value =
                        disclosure.value.copy(expandedAttempts = disclosure.value.expandedAttempts.toggled(id))
                },
                toggleGroup = { id ->
                    disclosure.value =
                        disclosure.value.copy(expandedWorkGroups = disclosure.value.expandedWorkGroups.toggled(id))
                },
                toggleEntry = { row ->
                    if (row.loadDetail && !row.expanded) model.perform(Intent.LoadItemDetail(row.id))
                    disclosure.value =
                        disclosure.value.copy(expandedEntries = disclosure.value.expandedEntries.toggled(row.id))
                },
                toggleFolder = { card, path, open ->
                    val others = disclosure.value.changedFiles.filterNot { it.runId == card.run }
                    val existing = disclosure.value.changedFiles.firstOrNull { it.runId == card.run }
                    val next =
                        (existing ?: ChangedFilesDisclosure(card.run, false, emptyMap())).let {
                            it.copy(overrides = it.overrides + (path to open))
                        }
                    disclosure.value = disclosure.value.copy(changedFiles = others + next)
                },
                toggleAllFolders = { card ->
                    val others = disclosure.value.changedFiles.filterNot { it.runId == card.run }
                    val existing = disclosure.value.changedFiles.firstOrNull { it.runId == card.run }
                    disclosure.value =
                        disclosure.value.copy(
                            changedFiles =
                                others + ChangedFilesDisclosure(card.run, !(existing?.allExpanded ?: false), emptyMap())
                        )
                },
                openThread = model::openThread,
                openDiff = { card ->
                    model.perform(Intent.SelectDiffTurn(card.run, card.openDiffPath))
                    model.navigate(Route.Workspace(WorkspaceTab.Diff))
                },
                fork = { run ->
                    model.perform(Intent.Fork(threadId, run)) { result ->
                        (result.getOrNull() as? Outcome.StartedThread)?.let { model.openThread(it.id) }
                    }
                },
                copy = copy,
            )
        Column(Modifier.fillMaxSize().imePadding()) {
            Box(Modifier.weight(1f).fillMaxWidth()) {
                Feed(model, current, now, listState, feedActions) { sheet = ThreadSheet.Setup }
                current.working?.let { working ->
                    WorkingControl(
                        working,
                        Modifier.align(Alignment.BottomCenter).padding(bottom = 8.dp),
                        onQueue = { sheet = ThreadSheet.Queue },
                        onAgents = { sheet = ThreadSheet.Agents },
                        scrollScope = rememberCoroutineScope(),
                        listState = listState,
                    )
                }
            }
            Column(
                Modifier.fillMaxWidth().padding(horizontal = 12.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                current.errorBanner?.let { banner ->
                    Banner(banner.text, banner.variant == BannerVariant.ERROR, banner.dismissLabel) {
                        model.perform(Intent.DismissThreadError(banner.dismissKey))
                    }
                }
                current.limitRecovery?.let { recovery ->
                    LimitRecoveryCard(
                        recovery.title,
                        recovery.resumeLabel,
                        recovery.snoozeLabel,
                        recovery.canSchedule,
                        recovery.snoozeEnabled,
                    ) {
                        model.perform(Intent.LimitRecovery(threadId, it))
                    }
                }
                current.requests.approval?.let { ApprovalCard(model, it) }
                if (current.requests.approval == null)
                    current.requests.questions?.let { QuestionsCard(model, it, current.composer.mobileShowsStop) }
            }
            if (current.requests.approval == null && current.requests.questions == null)
                Composer(model, current.composer) { sheet = ThreadSheet.Settings }
        }
        when (sheet) {
            ThreadSheet.Queue -> current.queue?.let { QueueSheet(model, it) { sheet = null } }
            ThreadSheet.Agents -> current.agents?.let { AgentsSheet(model, it) { sheet = null } }
            ThreadSheet.Settings -> ThreadSettingsSheet(model, current.composer) { sheet = null }
            ThreadSheet.Setup ->
                current.setup.card?.let { card ->
                    SetupDetailsSheet(model, card, { terminal -> model.navigate(Route.Terminal(threadId, terminal)) }) {
                        sheet = null
                    }
                }
            null -> Unit
        }
    }
}

private enum class ThreadSheet {
    Queue,
    Agents,
    Settings,
    Setup,
}

@Composable
private fun HeaderActions(model: AndroidAppModel, view: ThreadView) {
    view.header?.actions?.forEach { action ->
        when (action.kind) {
            HeaderActionKind.FILES ->
                HeaderIconButton(Icons.Outlined.Folder, action.accessibilityLabel, selected = action.selected) {
                    model.navigate(Route.Workspace(WorkspaceTab.Files))
                }
            HeaderActionKind.TERMINAL ->
                HeaderIconButton(Icons.Outlined.Terminal, action.accessibilityLabel) {
                    model.navigate(Route.Terminal(view.threadId, view.terminals.firstOrNull()?.terminalId ?: ""))
                }
            HeaderActionKind.MERGE_BACK ->
                IconButton(onClick = { model.perform(Intent.MergeBack) }, modifier = Modifier.size(48.dp)) {
                    Icon(
                        painterResource(R.drawable.ic_merge),
                        action.accessibilityLabel,
                        Modifier.size(22.dp),
                        tint = AppTheme.colors.headerForeground,
                    )
                }
        }
    }
}

@Composable
private fun Feed(
    model: AndroidAppModel,
    view: ThreadView,
    now: Long,
    listState: androidx.compose.foundation.lazy.LazyListState,
    actions: FeedActions,
    onSetupDetails: () -> Unit,
) {
    val rows = remember(view.rowsRevision) { view.rows.asReversed() }
    val reachedStart by remember {
        derivedStateOf {
            val last = listState.layoutInfo.visibleItemsInfo.lastOrNull()?.index ?: 0
            last >= listState.layoutInfo.totalItemsCount - 2
        }
    }
    LaunchedEffect(reachedStart, view.history.hasMore, view.history.loading) {
        if (reachedStart && view.history.hasMore && !view.history.loading) model.perform(Intent.LoadEarlier)
    }
    LazyColumn(
        Modifier.fillMaxSize().widthIn(max = 960.dp),
        state = listState,
        reverseLayout = true,
        contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = 12.dp, bottom = 56.dp),
    ) {
        view.setup.card
            ?.takeIf { it.showInTimeline }
            ?.let { card -> item(key = "setup") { SetupCard(card, onSetupDetails) } }
        items(rows, key = { it.id }) { row -> FeedRow(model, row, now, actions) }
        if (view.history.loading || view.history.error != null)
            item(key = "history") {
                Box(Modifier.fillMaxWidth().padding(12.dp), contentAlignment = Alignment.Center) {
                    val error = view.history.error
                    if (error != null)
                        TextButton(onClick = { model.perform(Intent.LoadEarlier) }) {
                            Text(error, style = AppTheme.caption, color = AppTheme.colors.dangerForeground)
                        }
                    else
                        CircularProgressIndicator(
                            Modifier.size(18.dp),
                            color = AppTheme.colors.iconMuted,
                            strokeWidth = 2.dp,
                        )
                }
            }
        if (rows.isEmpty() && view.syncStatus != ThreadStatus.LIVE)
            item(key = "loading") {
                Box(Modifier.fillMaxWidth().padding(32.dp), contentAlignment = Alignment.Center) {
                    CircularProgressIndicator(
                        Modifier.size(22.dp),
                        color = AppTheme.colors.iconMuted,
                        strokeWidth = 2.dp,
                    )
                }
            }
    }
}

/** Setup stages; they collapse into the working header once the agent's turn is live. */
@Composable
private fun SetupCard(card: SetupCardView, onDetails: () -> Unit) {
    val colors = AppTheme.colors
    Column(Modifier.fillMaxWidth().padding(vertical = 3.5.dp)) {
        if (card.showHeader)
            Row(
                Modifier.fillMaxWidth().heightIn(min = 38.5.dp).padding(horizontal = 3.5.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(7.dp),
            ) {
                val failed = card.tone == SetupTone.DESTRUCTIVE
                if (card.ownsWorkingSlot || card.phase == dev.remoteagent.core.SetupPhase.RUNNING)
                    Shimmer(card.title, Modifier.weight(1f))
                else
                    Text(
                        card.title,
                        Modifier.weight(1f),
                        style = AppTheme.footnote,
                        color = if (failed) colors.dangerForeground else colors.foregroundSecondary,
                        maxLines = 1,
                    )
                card.elapsed?.let { Text(it, style = AppTheme.micro, color = colors.foregroundSecondary) }
                Row(
                    Modifier.clickable(onClick = onDetails)
                        .padding(horizontal = 7.dp, vertical = 8.dp)
                        .then(
                            if (card.backgroundScript != null)
                                Modifier.border(1.dp, colors.border, CircleShape)
                                    .padding(horizontal = 7.dp, vertical = 3.5.dp)
                            else Modifier
                        ),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(4.dp),
                ) {
                    if (card.backgroundScript != null)
                        CircularProgressIndicator(Modifier.size(10.dp), color = colors.iconMuted, strokeWidth = 1.5.dp)
                    Text(
                        card.backgroundScript ?: "Details",
                        Modifier.widthIn(max = 160.dp),
                        style = AppTheme.micro,
                        color = colors.foregroundSecondary,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    if (card.backgroundScript == null)
                        Icon(Icons.Outlined.ChevronRight, null, Modifier.size(10.dp), tint = colors.iconMuted)
                }
            }
        androidx.compose.material3.HorizontalDivider(color = colors.border)
        if (card.showStages)
            Column(Modifier.padding(top = 3.5.dp, bottom = 7.dp)) {
                card.stages.forEach { SetupStageLine(it, compact = true) }
            }
    }
}

@Composable
private fun WorkingControl(
    working: WorkingControlView,
    modifier: Modifier,
    onQueue: () -> Unit,
    onAgents: () -> Unit,
    scrollScope: kotlinx.coroutines.CoroutineScope,
    listState: androidx.compose.foundation.lazy.LazyListState,
) {
    val colors = AppTheme.colors
    if (!working.hasCapsule && !working.showScrollToEnd) return
    Row(modifier, verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        if (working.hasCapsule)
            Surface(
                shape = CircleShape,
                color = colors.composerSurface,
                border = BorderStroke(1.dp, colors.composerBorder),
                shadowElevation = 2.dp,
            ) {
                Row(
                    Modifier.heightIn(min = 38.5.dp).padding(horizontal = 14.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    working.statusLabel?.let { Shimmer(it, color = colors.foregroundSecondary) }
                    working.queue?.let { segment -> Segment(segment.label, onQueue) }
                    working.agents?.let { segment -> Segment(segment.label, onAgents) }
                }
            }
        if (working.showScrollToEnd)
            Surface(
                onClick = { scrollScope.launch { listState.animateScrollToItem(0) } },
                shape = CircleShape,
                color = colors.composerSurface,
                border = BorderStroke(1.dp, colors.composerBorder),
                shadowElevation = 2.dp,
                modifier = Modifier.size(38.5.dp),
            ) {
                Box(contentAlignment = Alignment.Center) {
                    Icon(Icons.Outlined.ArrowDownward, "Scroll to end", Modifier.size(18.dp), tint = colors.icon)
                }
            }
    }
}

@Composable
private fun Segment(label: String, onClick: () -> Unit) {
    Text(
        label,
        Modifier.clickable(onClick = onClick).padding(start = 12.dp, top = 8.dp, bottom = 8.dp),
        style = AppTheme.footnote,
        fontWeight = FontWeight.Medium,
        color = AppTheme.colors.foreground,
    )
}

@Composable
private fun Banner(text: String, error: Boolean, dismissLabel: String, onDismiss: () -> Unit) {
    val colors = AppTheme.colors
    Row(
        Modifier.fillMaxWidth()
            .background(if (error) colors.danger else colors.warning, RoundedCornerShape(16.dp))
            .border(1.dp, if (error) colors.dangerBorder else colors.warningBorder, RoundedCornerShape(16.dp))
            .padding(start = 14.dp, top = 6.dp, bottom = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text,
            Modifier.weight(1f),
            style = AppTheme.caption,
            color = if (error) colors.dangerForeground else colors.warningForeground,
            maxLines = 4,
        )
        IconButton(onClick = onDismiss) {
            Icon(Icons.Outlined.Close, dismissLabel, Modifier.size(16.dp), tint = colors.iconMuted)
        }
    }
}

@Composable
private fun LimitRecoveryCard(
    title: String,
    resumeLabel: String,
    snoozeLabel: String,
    canSchedule: Boolean,
    snoozeEnabled: Boolean,
    onAction: (RecoveryAction) -> Unit,
) {
    val colors = AppTheme.colors
    Surface(
        Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(20.dp),
        color = colors.cardAlt,
        border = BorderStroke(1.dp, colors.border),
    ) {
        Column(Modifier.padding(14.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Text(title, style = AppTheme.footnote, fontWeight = FontWeight.Bold, color = colors.foreground)
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                RequestButton(resumeLabel, RequestTone.Primary, enabled = canSchedule) {
                    onAction(RecoveryAction.RESUME)
                }
                RequestButton(snoozeLabel, RequestTone.Secondary, enabled = snoozeEnabled) {
                    onAction(RecoveryAction.SNOOZE)
                }
            }
        }
    }
}

@Composable
private fun Unavailable(loading: Boolean) {
    Column(
        Modifier.fillMaxSize().padding(28.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        if (loading)
            CircularProgressIndicator(Modifier.size(22.dp), color = AppTheme.colors.iconMuted, strokeWidth = 2.dp)
        else {
            Text(
                "Thread unavailable",
                style = AppTheme.title,
                fontWeight = FontWeight.Bold,
                textAlign = TextAlign.Center,
            )
            Text(
                "This thread was deleted or is no longer available.",
                Modifier.padding(top = 7.dp),
                style = AppTheme.body,
                color = AppTheme.colors.foregroundMuted,
                textAlign = TextAlign.Center,
            )
        }
    }
}

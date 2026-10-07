// Declarative native layout with the fixed mobile metrics; the conversation decisions are supplied by core.
@file:Suppress("TooManyFunctions", "LongMethod", "CyclomaticComplexMethod", "LongParameterList", "MagicNumber")

package dev.remoteagent.mobile

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.outlined.EditNote
import androidx.compose.material.icons.outlined.FilterList
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.material.icons.outlined.WifiOff
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import dev.remoteagent.core.Driver
import dev.remoteagent.core.Intent
import dev.remoteagent.core.Snapshot
import dev.remoteagent.core.SwipeAction
import dev.remoteagent.core.ThreadAction
import dev.remoteagent.core.ThreadListEmpty
import dev.remoteagent.core.ThreadListItem
import dev.remoteagent.core.ThreadListOptions
import dev.remoteagent.core.ThreadMenuAction
import dev.remoteagent.core.ThreadRow
import dev.remoteagent.core.TimestampFormat

private const val SETTLED_INITIAL_COUNT = 10u
private const val SETTLED_PAGE_COUNT = 25u

/** Provider instance to driver, from the model catalog core holds. */
internal fun instanceDrivers(snapshot: Snapshot): Map<String, Driver> =
    snapshot
        .modelPicker("", null, emptyList())
        .rail
        .mapNotNull { item -> item.instance?.let { it.instanceId to it.driver } }
        .toMap()

@Composable
internal fun HomeScreen(model: AndroidAppModel) {
    val snapshot = model.snapshot
    val now = rememberNow()
    var search by rememberSaveable { mutableStateOf(snapshot.searchQuery()) }
    var searching by rememberSaveable { mutableStateOf(search.isNotEmpty()) }
    var workingExpanded by rememberSaveable { mutableStateOf(false) }
    var snoozedExpanded by rememberSaveable { mutableStateOf(false) }
    var settledExpanded by rememberSaveable { mutableStateOf(true) }
    var settledLimit by rememberSaveable(snapshot.selectedProjectId(), search) { mutableStateOf(SETTLED_INITIAL_COUNT) }
    val actionState = rememberThreadActions()
    val copy = rememberCopy()
    val options =
        ThreadListOptions(
            workingShelfEnabled = false,
            workingShelfExpanded = workingExpanded,
            snoozedShelfExpanded = snoozedExpanded,
            settledShelfExpanded = settledExpanded,
            settledLimit = settledLimit,
            shelfPreferencesLoading = false,
            timestampFormat = TimestampFormat.LOCALE,
        )
    val list by
        rememberView(snapshot, now / MINUTE_MILLIS, options) { it.threadList(System.currentTimeMillis(), options) }
    val drivers by rememberView(snapshot) { instanceDrivers(it) }
    val environment = snapshot.hostName()
    fun closeSearch() {
        search = ""
        searching = false
        model.perform(Intent.Search(""))
    }
    BackHandler(searching) { closeSearch() }
    val rowActions =
        ThreadRowActions(
            open = { row -> model.navigate(Route.Thread(row.id)) },
            swipe = { row, action ->
                model.perform(Intent.Thread(row.id, swipeThreadAction(action) ?: return@ThreadRowActions))
            },
            snooze = { row, child ->
                runThreadMenuAction(model, actionState, row.id, row.title, row.pinned, child.action, copy)
            },
            menu = { row, item -> selectThreadMenuItem(model, actionState, row.id, row.title, row.pinned, item, copy) },
        )
    val listState = rememberLazyListState()
    val expanded by remember { derivedStateOf { listState.firstVisibleItemIndex == 0 } }
    Column(Modifier.fillMaxSize()) {
        HomeToolbar(
            model,
            searching,
            search,
            onSearch = {
                search = it
                model.perform(Intent.Search(it))
            },
            onOpenSearch = { searching = true },
            onCloseSearch = ::closeSearch,
        )
        Surface(
            Modifier.fillMaxSize(),
            color = AppTheme.colors.screen,
            shape = RoundedCornerShape(topStart = 28.dp, topEnd = 28.dp),
        ) {
            Box(Modifier.fillMaxSize()) {
                val current = list
                val empty = current?.empty
                if (current != null && empty != null) EmptyList(empty)
                else
                    LazyColumn(state = listState, contentPadding = PaddingValues(top = 14.dp, bottom = 160.dp)) {
                        items(current?.items.orEmpty(), key = ::itemKey) { item ->
                            when (item) {
                                is ThreadListItem.Thread ->
                                    ThreadListRow(
                                        item.row,
                                        environment,
                                        rowDrivers(item.row, drivers.orEmpty()),
                                        rowActions,
                                    )
                                is ThreadListItem.PendingTask ->
                                    PendingTaskListRow(
                                        item.task,
                                        onOpen = { model.navigate(Route.Thread(item.task.threadId)) },
                                        onDelete = { model.perform(Intent.DiscardPending(item.task.commandId)) },
                                    )
                                is ThreadListItem.WorkingShelf ->
                                    ShelfHeaderRow("Working", item.shelf) { workingExpanded = !workingExpanded }
                                is ThreadListItem.SnoozedShelf ->
                                    ShelfHeaderRow("Snoozed", item.shelf, snoozed = true) {
                                        snoozedExpanded = !snoozedExpanded
                                    }
                                is ThreadListItem.SettledShelf ->
                                    ShelfHeaderRow("Settled", item.shelf) { settledExpanded = !settledExpanded }
                            }
                        }
                        val hidden = current?.hiddenSettledCount ?: 0u
                        if (hidden > 0u)
                            item(key = "show-more") { ShowMoreRow(hidden) { settledLimit += SETTLED_PAGE_COUNT } }
                    }
                HomeFabs(model, expanded, Modifier.align(Alignment.BottomEnd))
            }
        }
    }
    ThreadActionDialogs(model, actionState)
    if (actionState.arranging) ArrangeSheet(model) { actionState.arranging = false }
}

private const val MINUTE_MILLIS = 60_000L

private fun itemKey(item: ThreadListItem): String =
    when (item) {
        is ThreadListItem.Thread -> item.row.key
        is ThreadListItem.PendingTask -> item.task.key
        is ThreadListItem.WorkingShelf -> "working"
        is ThreadListItem.SnoozedShelf -> "snoozed"
        is ThreadListItem.SettledShelf -> "settled"
    }

private fun rowDrivers(row: ThreadRow, drivers: Map<String, Driver>): List<Driver> {
    if (row.providerInstances.lastOrNull()?.let(drivers::containsKey) != true) return emptyList()
    return row.providerInstances.mapNotNull(drivers::get)
}

internal fun swipeThreadAction(action: SwipeAction): ThreadAction? =
    when (action) {
        SwipeAction.SETTLE -> ThreadAction.Settle
        SwipeAction.UNSETTLE -> ThreadAction.Unsettle
        SwipeAction.UNSNOOZE -> ThreadAction.Unsnooze
        SwipeAction.SNOOZE -> null
    }

@Composable
private fun HomeToolbar(
    model: AndroidAppModel,
    searching: Boolean,
    search: String,
    onSearch: (String) -> Unit,
    onOpenSearch: () -> Unit,
    onCloseSearch: () -> Unit,
) {
    Row(
        Modifier.fillMaxWidth().heightIn(min = 64.dp).padding(horizontal = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        if (searching) {
            HeaderIconButton(Icons.AutoMirrored.Outlined.ArrowBack, "Close search", onClick = onCloseSearch)
            SettingsField(search, onSearch, "Search", Modifier.weight(1f))
        } else {
            Row(
                Modifier.weight(1f).padding(start = 16.dp).clickable { model.showHosts() },
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(7.dp),
            ) {
                ConnectionTitle(model)
            }
            HeaderIconButton(Icons.Outlined.Search, "Search threads", onClick = onOpenSearch)
            HeaderIconButton(Icons.Outlined.Settings, "Open settings") { model.navigate(Route.Settings()) }
        }
    }
}

@Composable
private fun ConnectionTitle(model: AndroidAppModel) {
    val colors = AppTheme.colors
    val connected = model.snapshot.connected()
    if (connected) {
        Text("Bex", style = AppTheme.title, fontWeight = FontWeight.Bold, color = colors.headerForeground)
        return
    }
    if (model.busy) CircularProgressIndicator(Modifier.size(20.dp), color = colors.iconMuted, strokeWidth = 2.dp)
    else Icon(Icons.Outlined.WifiOff, null, Modifier.size(15.dp), tint = colors.iconMuted)
    Text(
        if (model.busy) "Connecting…" else "Offline",
        style = AppTheme.body,
        fontWeight = FontWeight.Bold,
        color = colors.foregroundMuted,
        maxLines = 1,
    )
}

@Composable
private fun EmptyList(empty: ThreadListEmpty) {
    Column(
        Modifier.fillMaxSize().padding(horizontal = 28.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        if (empty.loading) {
            CircularProgressIndicator(Modifier.size(24.dp), color = AppTheme.colors.iconMuted, strokeWidth = 2.dp)
            Spacer(Modifier.height(16.dp))
        }
        Text(
            empty.title,
            style = AppTheme.title,
            fontWeight = FontWeight.Bold,
            color = AppTheme.colors.foreground,
            textAlign = TextAlign.Center,
        )
        Text(
            empty.detail,
            Modifier.padding(top = 7.dp),
            style = AppTheme.body,
            color = AppTheme.colors.foregroundMuted,
            textAlign = TextAlign.Center,
        )
    }
}

/** The project filter above the extended New thread button. */
@Composable
private fun HomeFabs(model: AndroidAppModel, expanded: Boolean, modifier: Modifier) {
    val colors = AppTheme.colors
    var filter by remember { mutableStateOf(false) }
    val selected = model.snapshot.selectedProjectId()
    Column(
        modifier.padding(end = 20.dp, bottom = 32.dp),
        horizontalAlignment = Alignment.End,
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Box {
            FloatingActionButton(
                onClick = { filter = true },
                containerColor = colors.secondary,
                contentColor = colors.secondaryForeground,
                shape = RoundedCornerShape(16.dp),
                modifier = Modifier.size(56.dp),
            ) {
                Icon(Icons.Outlined.FilterList, "Filter threads")
            }
            AnchoredMenu(filter, { filter = false }) {
                Text(
                    "Project",
                    Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                    style = AppTheme.caption,
                    color = colors.foregroundSecondary,
                )
                DropdownMenuItem(
                    text = { Text(checked("All projects", selected == null), style = AppTheme.footnote) },
                    onClick = {
                        filter = false
                        model.perform(Intent.FilterProject(null))
                    },
                )
                HorizontalDivider(color = colors.border)
                model.snapshot.projects().forEach { project ->
                    DropdownMenuItem(
                        text = { Text(checked(project.name, selected == project.id), style = AppTheme.footnote) },
                        onClick = {
                            filter = false
                            model.perform(Intent.FilterProject(project.id))
                        },
                    )
                }
            }
        }
        ExtendedFloatingActionButton(
            onClick = { model.navigate(Route.ChooseProject) },
            expanded = expanded,
            icon = { Icon(Icons.Outlined.EditNote, null) },
            text = { Text("New thread", style = AppTheme.footnote, fontWeight = FontWeight.Medium) },
            containerColor = colors.primary,
            contentColor = colors.primaryForeground,
            shape = RoundedCornerShape(16.dp),
        )
    }
}

private fun checked(label: String, on: Boolean) = if (on) "✓  $label" else label

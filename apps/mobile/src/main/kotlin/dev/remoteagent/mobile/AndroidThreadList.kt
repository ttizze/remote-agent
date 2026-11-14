package dev.remoteagent.mobile

import android.graphics.BitmapFactory
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Tab
import androidx.compose.material3.TabRow
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.remoteagent.core.Intent
import dev.remoteagent.core.ListQuery
import dev.remoteagent.core.ListSessions
import dev.remoteagent.core.ProjectIconImages
import dev.remoteagent.core.ProjectSummary
import dev.remoteagent.core.ReadThread
import dev.remoteagent.core.SessionRef
import dev.remoteagent.core.ThreadList
import dev.remoteagent.core.ThreadSummary
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.drop

@Composable
internal fun ThreadListScreen(
    list: ThreadList?,
    query: ListQuery,
    loadingThreads: Boolean,
    perform: (Intent, String?) -> Unit,
    title: @Composable RowScope.() -> Unit,
    emptyContent: (@Composable () -> Unit)? = null,
    scopeKey: String = "",
) {
    val language = LocalUiLanguage.current
    var search by rememberSaveable(scopeKey) { mutableStateOf(query.searchTerm.takeIf { it.isNotEmpty() }) }
    val focusManager = LocalFocusManager.current
    fun closeSearch() {
        search = null
        focusManager.clearFocus()
        perform(Intent.ListSessions(ListSessions(query = query.copy(searchTerm = ""))), null)
    }
    BackHandler(enabled = search != null) { closeSearch() }
    var selectedTab by rememberSaveable { mutableStateOf(0) }
    val currentQuery by rememberUpdatedState(query)
    val dispatch by rememberUpdatedState(perform)
    val listState = rememberLazyListState()
    val reorder = rememberProjectReorder(listState, list?.projects.orEmpty(), perform)
    LaunchedEffect(scopeKey) {
        // Core loads the initial query; every later edit supersedes pending input.
        snapshotFlow { search }
            .drop(1)
            .collectLatest { term ->
                if (term != null) {
                    kotlinx.coroutines.delay(SEARCH_DEBOUNCE_MILLIS)
                    dispatch(Intent.ListSessions(ListSessions(query = currentQuery.copy(searchTerm = term))), null)
                }
            }
    }
    Column(Modifier.fillMaxSize().padding(horizontal = 20.dp)) {
        ThreadListHeader(
            search = search,
            loadingThreads = loadingThreads,
            changeSearch = { if (it == null) closeSearch() else search = it },
            refresh = { perform(Intent.ListSessions(ListSessions(query = query)), null) },
            title = title,
        )
        if (emptyContent != null) {
            Box(Modifier.weight(1f)) { emptyContent() }
        } else {
            TabRow(selectedTabIndex = selectedTab, containerColor = MaterialTheme.colorScheme.background) {
                listOf(language.text("最近のチャット"), language.text("プロジェクト")).forEachIndexed { index, label ->
                    Tab(selected = selectedTab == index, onClick = { selectedTab = index }, text = { Text(label) })
                }
            }
            ThreadListContent(
                list,
                loadingThreads,
                selectedTab,
                reorder,
                perform,
                Modifier.weight(1f).fillMaxWidth(),
            )
            NewChatButton(perform)
        }
    }
}

@Composable
private fun NewChatButton(perform: (Intent, String?) -> Unit) {
    val language = LocalUiLanguage.current
    Button(
        onClick = { perform(Intent.NewChat(""), null) },
        modifier = Modifier.fillMaxWidth().padding(vertical = 12.dp),
        shape = RoundedCornerShape(10.dp),
        contentPadding = PaddingValues(16.dp),
    ) {
        Icon(painterResource(R.drawable.ic_add), null, Modifier.size(20.dp))
        Text(language.text("新しいチャット"), Modifier.padding(start = 8.dp), fontWeight = FontWeight.SemiBold)
    }
}

@Composable
private fun ThreadListContent(
    list: ThreadList?,
    loadingThreads: Boolean,
    selectedTab: Int,
    reorder: ProjectReorder,
    perform: (Intent, String?) -> Unit,
    modifier: Modifier = Modifier,
) {
    val language = LocalUiLanguage.current
    Box(modifier.clipToBounds()) {
        LazyColumn(Modifier.fillMaxSize(), state = reorder.list, contentPadding = PaddingValues(vertical = 16.dp)) {
            list?.notice?.let { notice -> item { Text(notice, color = MaterialTheme.colorScheme.onSurfaceVariant) } }
            if (selectedTab == 0) {
                recentChats(list, loadingThreads, perform, language)
            } else {
                projectThreads(list, loadingThreads, perform, reorder, language)
            }
        }
        reorder.projects
            .firstOrNull { it.listKey == reorder.source }
            ?.let { project ->
                // Draw above the list while the original handle stays put to keep gesture coordinates stable.
                ProjectRow(
                    project,
                    {},
                    reorder,
                    floating = true,
                    modifier =
                        Modifier.graphicsLayer { translationY = reorder.draggedTop }
                            .padding(top = 2.dp)
                            .shadow(12.dp, RoundedCornerShape(12.dp))
                            .background(MaterialTheme.colorScheme.surfaceContainerHigh, RoundedCornerShape(12.dp))
                            .border(
                                1.dp,
                                MaterialTheme.colorScheme.primary.copy(alpha = 0.5f),
                                RoundedCornerShape(12.dp),
                            )
                            .clearAndSetSemantics { testTag = "dragged-project" },
                )
            }
    }
}

private fun LazyListScope.recentChats(
    list: ThreadList?,
    loadingThreads: Boolean,
    perform: (Intent, String?) -> Unit,
    language: dev.remoteagent.core.UiLanguage,
) {
    val threads = list?.threads.orEmpty()
    items(threads, key = { "chat:${it.hostId}:${it.id.listKey}" }) { thread ->
        SummaryRow(
            thread,
            { perform(it, thread.hostId) },
            list
                ?.projects
                ?.firstOrNull { project -> project.hostId == thread.hostId && project.id == thread.projectId }
                ?.name,
        )
    }
    if (list?.hasMore == true)
        item {
            TextButton(
                onClick = { perform(Intent.ExpandRecentChats, null) },
                enabled = !loadingThreads && !list.loadingChats,
            ) {
                LoadingLabel("もっと見る", list.loadingChats)
            }
        }
    if (list != null && threads.isEmpty())
        item {
            Text(
                language.text("チャットがありません。"),
                Modifier.padding(vertical = 24.dp),
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
}

@Composable
private fun LoadingLabel(label: String, loading: Boolean) {
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
        if (loading) CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
        Text(LocalUiLanguage.current.text(label))
    }
}

@Composable
private fun SummaryRow(thread: ThreadSummary, openConversation: (Intent) -> Unit, projectName: String? = null) {
    val language = LocalUiLanguage.current
    Row(
        Modifier.fillMaxWidth()
            .clickable { openConversation(Intent.ReadThread(ReadThread(thread.id, open = true))) }
            .padding(vertical = 16.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Icon(
            painterResource(R.drawable.ic_chat),
            null,
            Modifier.size(22.dp),
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Column(Modifier.weight(1f)) {
            Text(thread.title, maxLines = 2, overflow = TextOverflow.Ellipsis)
            projectName?.let {
                Text(
                    it,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        if (thread.active) CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
        else if (thread.unread) Text(language.text("● 完了・未確認"))
        if (thread.hasChanges == true) {
            Icon(
                painterResource(R.drawable.ic_diff),
                language.text("作成元からの変更あり"),
                Modifier.padding(start = 8.dp).size(18.dp),
                tint = Color(CHANGES_COLOR_ARGB),
            )
        }
        Icon(
            painterResource(R.drawable.ic_chevron),
            null,
            Modifier.size(16.dp),
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
    HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.5f))
}

private const val DRAG_PLACEHOLDER_ALPHA = 0.2f
private const val SEARCH_DEBOUNCE_MILLIS = 200L
private const val CHANGES_COLOR_ARGB = 0xFFFB923C

private val SessionRef.listKey: String
    get() = "session:$provider:$id"

private fun LazyListScope.projectThreads(
    list: ThreadList?,
    loadingThreads: Boolean,
    perform: (Intent, String?) -> Unit,
    reorder: ProjectReorder,
    language: dev.remoteagent.core.UiLanguage,
) {
    list?.projects.orEmpty().forEach { project ->
        projectSection(project, reorder, { perform(it, project.hostId) }, language)
    }
    if (list?.hasMoreProjects == true)
        item {
            TextButton(
                onClick = { perform(Intent.ExpandProjects, null) },
                enabled = !loadingThreads && !list.loadingProjects,
            ) {
                LoadingLabel("もっとプロジェクトを表示", list.loadingProjects)
            }
        }
    if (list != null && list.projects.isEmpty())
        item {
            Text(
                language.text("プロジェクトがありません。"),
                Modifier.padding(vertical = 24.dp),
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    list
        ?.hiddenProjects
        ?.takeIf { it.isNotEmpty() }
        ?.let { hidden ->
            item(key = "hidden-projects") {
                HiddenProjects(hidden) { project, intent -> perform(intent, project.hostId) }
            }
        }
}

private fun LazyListScope.projectSection(
    project: ProjectSummary,
    reorder: ProjectReorder,
    dispatch: (Intent) -> Unit,
    language: dev.remoteagent.core.UiLanguage,
) {
    item(key = "project:${project.listKey}") {
        ProjectDropMarker(reorder, project.listKey, after = false)
        ProjectRow(
            project,
            dispatch,
            reorder,
            modifier = Modifier.alpha(if (reorder.source == project.listKey) DRAG_PLACEHOLDER_ALPHA else 1f),
        )
        ProjectDropMarker(reorder, project.listKey, after = true)
        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.5f))
    }
    if (project.expanded) {
        project.coordinatorUrl?.let {
            item(key = "coordinator:${project.listKey}") {
                Text(
                    language.text("タスクの依頼・振り分けはメイン会話へ。以下はこのPCの作業スレッドです。"),
                    Modifier.padding(horizontal = 12.dp, vertical = 4.dp),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        if (project.loading && !project.hasMore)
            item(key = "loading:${project.listKey}") { CircularProgressIndicator(Modifier.size(18.dp)) }
        project.error?.let { error ->
            item(key = "error:${project.listKey}") {
                ErrorTextView(error, color = androidx.compose.ui.graphics.Color.Unspecified)
                TextButton(onClick = { dispatch(Intent.RefreshProject(project.id)) }) { Text(language.text("再試行")) }
            }
        }
        items(project.threads, key = { "project:${project.listKey}:${it.id.listKey}" }) { SummaryRow(it, dispatch) }
        if (project.hasMore)
            item(key = "more:${project.listKey}") {
                TextButton(
                    onClick = { dispatch(Intent.ExpandThreadList(project.id)) },
                    enabled = !project.loading,
                ) {
                    LoadingLabel("もっと見る", project.loading)
                }
            }
    }
}

@Composable
private fun ProjectRow(
    project: ProjectSummary,
    perform: (Intent) -> Unit,
    reorder: ProjectReorder,
    modifier: Modifier = Modifier,
    floating: Boolean = false,
) {
    Row(
        modifier.fillMaxWidth().padding(vertical = 10.dp),
        horizontalArrangement = Arrangement.SpaceBetween,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (floating) {
            Box(Modifier.size(40.dp, 48.dp), contentAlignment = Alignment.Center) {
                Text("≡", color = MaterialTheme.colorScheme.primary)
            }
        } else {
            ProjectDragHandle(project, reorder)
        }
        Row(
            Modifier.weight(1f).heightIn(min = 48.dp).clickable {
                perform(Intent.SetProjectExpanded(project.id, !project.expanded))
            },
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            ProjectIcon(project.icon, project.monogram, project.iconColor)
            Column(Modifier.weight(1f)) {
                Text(
                    project.name,
                    style = MaterialTheme.typography.titleMedium,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                project.roots.firstOrNull()?.path?.let { path ->
                    Text(
                        path,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        }
        ProjectConversationButton(project, perform)
        if (!floating) ProjectActions(project, perform)
    }
}

@Composable
private fun ProjectIcon(icon: ProjectIconImages?, monogram: String, colorRgb: UInt) {
    val colors = MaterialTheme.colorScheme
    val png = if (colors.background.luminance() < colors.onBackground.luminance()) icon?.darkPng else icon?.lightPng
    val bitmap = remember(png) { png?.let { BitmapFactory.decodeByteArray(it, 0, it.size)?.asImageBitmap() } }
    val color = Color(colorRgb.toInt()).copy(alpha = 1f)
    val modifier = Modifier.size(34.dp).clip(RoundedCornerShape(8.dp)).clearAndSetSemantics {}
    if (bitmap != null) {
        Image(bitmap, contentDescription = null, modifier = modifier)
    } else {
        Box(modifier.background(color.copy(alpha = 0.15f)), contentAlignment = Alignment.Center) {
            Text(monogram, color = color, fontSize = 12.sp, fontWeight = FontWeight.Bold)
        }
    }
}

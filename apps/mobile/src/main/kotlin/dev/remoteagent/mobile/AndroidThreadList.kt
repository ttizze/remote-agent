package dev.remoteagent.mobile

import android.graphics.BitmapFactory
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
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
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.remoteagent.core.Intent
import dev.remoteagent.core.ListQuery
import dev.remoteagent.core.ListSessions
import dev.remoteagent.core.ProjectSummary
import dev.remoteagent.core.ReadThread
import dev.remoteagent.core.SessionRef
import dev.remoteagent.core.ThreadList
import dev.remoteagent.core.ThreadSummary
import dev.remoteagent.core.WorktreeStatus

@Composable
internal fun ThreadListScreen(
    list: ThreadList?,
    query: ListQuery,
    loadingThreads: Boolean,
    perform: (Intent) -> Unit,
    showHosts: () -> Unit,
    hostName: String = "My PC",
    connected: Boolean = false,
) {
    var search by rememberSaveable { mutableStateOf(query.searchTerm) }
    var selectedTab by rememberSaveable { mutableStateOf(0) }
    val currentQuery by rememberUpdatedState(query)
    val dispatch by rememberUpdatedState(perform)
    LaunchedEffect(search) {
        kotlinx.coroutines.delay(SEARCH_DEBOUNCE_MILLIS)
        if (currentQuery.searchTerm != search)
            dispatch(Intent.ListSessions(ListSessions(query = currentQuery.copy(searchTerm = search))))
    }
    Column(Modifier.fillMaxSize().padding(horizontal = 20.dp)) {
        HostSelector(hostName, connected, loadingThreads, showHosts) {
            perform(Intent.ListSessions(ListSessions(query = query)))
        }
        ThreadSearchHeader(search, { search = it })
        TabRow(selectedTabIndex = selectedTab, containerColor = MaterialTheme.colorScheme.background) {
            listOf("最近のチャット", "プロジェクト").forEachIndexed { index, label ->
                Tab(selected = selectedTab == index, onClick = { selectedTab = index }, text = { Text(label) })
            }
        }
        LazyColumn(Modifier.weight(1f).fillMaxWidth(), contentPadding = PaddingValues(vertical = 16.dp)) {
            list?.notice?.let { notice -> item { Text(notice, color = MaterialTheme.colorScheme.onSurfaceVariant) } }
            if (selectedTab == 0) {
                recentChats(list, loadingThreads, perform)
            } else {
                projectThreads(list, perform)
                if (list?.hasMoreProjects == true)
                    item {
                        TextButton(
                            onClick = { perform(Intent.ExpandProjects) },
                            enabled = !loadingThreads && !list.loadingProjects,
                        ) {
                            LoadingLabel("もっとプロジェクトを表示", list.loadingProjects)
                        }
                    }
                if (list != null && list.projects.isEmpty())
                    item {
                        Text(
                            "プロジェクトがありません。",
                            Modifier.padding(vertical = 24.dp),
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
            }
        }
        Button(
            onClick = { perform(Intent.NewChat("")) },
            modifier = Modifier.fillMaxWidth().padding(vertical = 12.dp),
            shape = RoundedCornerShape(10.dp),
            contentPadding = PaddingValues(16.dp),
        ) {
            Icon(painterResource(R.drawable.ic_add), null, Modifier.size(20.dp))
            Text("新しいチャット", Modifier.padding(start = 8.dp), fontWeight = FontWeight.SemiBold)
        }
    }
}

private fun LazyListScope.recentChats(list: ThreadList?, loadingThreads: Boolean, perform: (Intent) -> Unit) {
    val threads = list?.threads.orEmpty()
    items(threads, key = { "chat:${it.id.listKey}" }) {
        SummaryRow(it, perform, list?.projects?.firstOrNull { project -> project.id == it.projectId }?.name)
    }
    if (list?.hasMore == true)
        item {
            TextButton(
                onClick = { perform(Intent.ExpandThreadList(null)) },
                enabled = !loadingThreads && !list.loadingChats,
            ) {
                LoadingLabel("もっと見る", list.loadingChats)
            }
        }
    if (list != null && threads.isEmpty())
        item {
            Text("チャットがありません。", Modifier.padding(vertical = 24.dp), color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
}

@Composable
private fun HostSelector(
    hostName: String,
    connected: Boolean,
    loading: Boolean,
    showHosts: () -> Unit,
    refresh: () -> Unit,
) {
    Row(Modifier.fillMaxWidth().padding(bottom = 20.dp), verticalAlignment = Alignment.CenterVertically) {
        androidx.compose.material3.OutlinedButton(
            onClick = showHosts,
            modifier = Modifier.weight(1f),
            shape = RoundedCornerShape(10.dp),
            border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
            contentPadding = PaddingValues(horizontal = 14.dp, vertical = 12.dp),
        ) {
            Icon(painterResource(R.drawable.ic_computer), null, Modifier.size(20.dp))
            Text(
                hostName,
                Modifier.weight(1f).padding(horizontal = 12.dp),
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                color = MaterialTheme.colorScheme.onSurface,
            )
            Text(
                if (connected) "接続中" else "未接続",
                color =
                    if (connected) MaterialTheme.colorScheme.tertiary else MaterialTheme.colorScheme.onSurfaceVariant,
                fontSize = 12.sp,
            )
            Icon(painterResource(R.drawable.ic_chevron), null, Modifier.padding(start = 8.dp).size(16.dp))
        }
        IconButton(onClick = refresh, enabled = !loading) { Icon(painterResource(R.drawable.ic_refresh), "更新") }
    }
}

@Composable
private fun ThreadSearchHeader(search: String, setSearch: (String) -> Unit) {
    Text("ワークスペース", style = MaterialTheme.typography.headlineSmall, fontWeight = FontWeight.Bold)
    OutlinedTextField(
        search,
        setSearch,
        Modifier.fillMaxWidth().padding(top = 16.dp, bottom = 12.dp),
        placeholder = { Text("プロジェクト・チャットを検索", fontSize = 14.sp) },
        leadingIcon = { Icon(painterResource(R.drawable.ic_search), null) },
        singleLine = true,
        shape = RoundedCornerShape(10.dp),
    )
}

@Composable
private fun LoadingLabel(label: String, loading: Boolean) {
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
        if (loading) CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
        Text(label)
    }
}

@Composable
private fun SummaryRow(thread: ThreadSummary, openConversation: (Intent) -> Unit, projectName: String? = null) {
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
                Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
        if (thread.active) CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
        else if (thread.unread) Text("● 完了・未確認")
        thread.worktreeStatus?.let { status ->
            val unmerged = status == WorktreeStatus.UNMERGED
            Icon(
                painterResource(if (unmerged) R.drawable.ic_diff else R.drawable.ic_merge),
                if (unmerged) "main に未反映の変更あり" else "main にマージ済み",
                Modifier.padding(start = 8.dp).size(18.dp),
                tint = if (unmerged) Color(UNMERGED_COLOR_ARGB) else MaterialTheme.colorScheme.tertiary,
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

private const val SEARCH_DEBOUNCE_MILLIS = 200L
private const val UNMERGED_COLOR_ARGB = 0xFFFB923C

private val SessionRef.listKey: String
    get() = "session:$provider:$id"

private fun LazyListScope.projectThreads(list: ThreadList?, perform: (Intent) -> Unit) {
    val projects = list?.projects.orEmpty()
    projects.forEach { project ->
        item(key = "project:${project.id}") {
            ProjectRow(project, perform)
            HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.5f))
        }
        if (project.expanded) {
            if (project.loading && !project.hasMore)
                item(key = "loading:${project.id}") { CircularProgressIndicator(Modifier.size(18.dp)) }
            project.error?.let { message ->
                item(key = "error:${project.id}") {
                    Text(message)
                    TextButton(onClick = { perform(Intent.RefreshProject(project.id)) }) { Text("再試行") }
                }
            }
            items(project.threads, key = { "project:${project.id}:${it.id.listKey}" }) { SummaryRow(it, perform) }
            if (project.hasMore)
                item(key = "more:${project.id}") {
                    TextButton(onClick = { perform(Intent.ExpandThreadList(project.id)) }, enabled = !project.loading) {
                        LoadingLabel("もっと見る", project.loading)
                    }
                }
        }
    }
}

@Composable
private fun ProjectRow(project: ProjectSummary, perform: (Intent) -> Unit) {
    Row(
        Modifier.fillMaxWidth().padding(vertical = 10.dp),
        horizontalArrangement = Arrangement.SpaceBetween,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Row(
            Modifier.weight(1f).heightIn(min = 48.dp).clickable {
                perform(Intent.SetProjectExpanded(project.id, !project.expanded))
            },
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            ProjectIcon(project.iconPng, project.monogram, project.iconColor)
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
        IconButton(onClick = { perform(Intent.NewChat(project.roots.firstOrNull()?.path.orEmpty())) }) {
            Icon(painterResource(R.drawable.ic_add), "${project.name}で新しいチャット")
        }
    }
}

@Composable
private fun ProjectIcon(png: ByteArray?, monogram: String, colorRgb: UInt) {
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
